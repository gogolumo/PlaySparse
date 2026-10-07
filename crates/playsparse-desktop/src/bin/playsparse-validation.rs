//! Standalone kit: download beside the engine/fixture, no source checkout or Rust installation required.
use anyhow::{Context, Result, ensure};
use playsparse_desktop::{LaunchDescriptor, Operation, Service};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            ensure!(!entry.file_type()?.is_symlink(), "Fixture symlink");
            if entry.file_type()?.is_dir() {
                walk(root, &path, out)?;
            } else {
                out.insert(
                    path.strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    blake3::hash(&fs::read(path)?).to_hex().to_string(),
                );
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out)?;
    Ok(out)
}
fn wait(service: &Service) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let job = service
            .snapshot()
            .jobs
            .last()
            .context("Missing job")?
            .clone();
        if job.state != "running" {
            ensure!(
                job.state == "completed",
                "Job {}: {:?}",
                job.state,
                job.error
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Job timeout; retained work directory"
        );
        thread::sleep(Duration::from_millis(50));
    }
}
fn stage(receipt: &mut Value, name: &str, test: impl FnOnce() -> Result<()>) -> Result<()> {
    let start = timestamp();
    let result = test();
    receipt["test_stages"].as_array_mut().unwrap().push(json!({"name":name,"started_at":start,"finished_at":timestamp(),"status":if result.is_ok(){"PASS"}else{"FAIL"},"error":result.as_ref().err().map(|e|format!("{e:#}"))}));
    println!("{} {name}", if result.is_ok() { "PASS" } else { "FAIL" });
    result
}
fn run(args: &BTreeMap<String, String>, work: &Path, receipt: &mut Value) -> Result<()> {
    let engine =
        PathBuf::from(args.get("--engine").context("--engine required")?).canonicalize()?;
    let fixture =
        PathBuf::from(args.get("--fixture").context("--fixture required")?).canonicalize()?;
    let portable = args.contains_key("--portable");
    let service_only = args.contains_key("--service-only");
    let mut app_child = None;
    stage(receipt, "app launches", || {
        if portable || service_only {
            return Ok(());
        }
        let app = PathBuf::from(
            args.get("--app")
                .context("--app required for native validation")?,
        )
        .canonicalize()?;
        let child = Command::new(app)
            .env("PLAYSPARSE_DESKTOP_DATA_DIR", work.join("gui-library"))
            .spawn()?;
        app_child = Some(child);
        println!(
            "Confirm the actual PlaySparse library window is visible: type VISIBLE and press Enter."
        );
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        ensure!(
            answer.trim() == "VISIBLE",
            "GUI visibility was not confirmed"
        );
        ensure!(
            app_child.as_mut().unwrap().try_wait()?.is_none(),
            "App exited"
        );
        Ok(())
    })?;
    if portable || service_only {
        receipt["test_stages"][0]["status"] = json!("NOT RUN");
    }
    let service = Service::open(&work.join("service-library"), &engine)?;
    stage(receipt, "engine launches", || {
        let system = service.system_status()?;
        ensure!(system.is_object(), "Engine report invalid");
        Ok(())
    })?;
    let system = service.system_status()?;
    receipt["filesystem_provider"] = system["readiness"]["driver"].clone();
    receipt["readiness"] = system["readiness"].clone();
    stage(receipt, "filesystem provider readiness", || {
        if !portable {
            ensure!(
                system["readiness"]["can_attempt_mount"] == true,
                "Provider unavailable; install explicitly then use a fresh work directory"
            );
        }
        Ok(())
    })?;
    let source = work.join("generated-source");
    fs::create_dir_all(source.join("assets"))?;
    let expected = b"PlaySparse generated fixture. Originals retained.\n".repeat(40000);
    fs::write(source.join("assets/data.bin"), &expected)?;
    fs::write(source.join("assets/duplicate.bin"), &expected)?;
    let name = if cfg!(windows) {
        "fixture.exe"
    } else {
        "fixture"
    };
    fs::copy(fixture, source.join(name))?;
    receipt["source_before_hashes"] = json!(hashes(&source)?);
    let mut registered = None;
    stage(receipt, "fixture registration", || {
        registered = Some(service.add_game(&source)?);
        Ok(())
    })?;
    let game = registered.context("Fixture registration failed")?;
    service.configure_launch(
        &game.id,
        LaunchDescriptor {
            executable: name.into(),
            args: vec!["launcher".into()],
            compatibility_confirmed: true,
        },
    )?;
    for (operation, name) in [
        (Operation::Analyze, "analysis"),
        (Operation::Optimize, "optimization"),
        (Operation::Verify, "verification"),
    ] {
        stage(receipt, name, || {
            service.start_job(&game.id, operation)?;
            wait(&service)
        })?;
    }
    if !portable {
        stage(receipt, "mount", || {
            service.perform_runtime(&game.id, "mount", false)
        })?;
        let mount = service.snapshot().games[0]
            .session
            .as_ref()
            .unwrap()
            .mountpoint
            .clone();
        #[cfg(windows)]
        let mount = PathBuf::from(format!("{}\\", mount.display()));
        stage(receipt, "exact file read", || {
            ensure!(
                fs::read(mount.join("assets/data.bin"))? == expected,
                "Different mounted bytes"
            );
            Ok(())
        })?;
        stage(receipt, "overlay write", || {
            fs::write(mount.join("save.txt"), b"generated save")?;
            ensure!(!source.join("save.txt").exists(), "Source modified");
            Ok(())
        })?;
        stage(receipt, "launch test fixture", || {
            service.perform_runtime(&game.id, "launch", false)
        })?;
        stage(receipt, "process detection", || {
            let deadline = Instant::now() + Duration::from_secs(4);
            loop {
                service.reconcile()?;
                let s = service.snapshot().games[0].session.clone().unwrap();
                if s.state == "launcher_exited_but_game_running" {
                    ensure!(
                        service.unmount_game(&game.id, true).is_err(),
                        "Unmount accepted live descendants"
                    );
                    return Ok(());
                }
                ensure!(
                    Instant::now() < deadline,
                    "Descendant not detected: {:?}",
                    s.processes
                );
                thread::sleep(Duration::from_millis(100));
            }
        })?;
        // The launcher exited; Stop acts on its owned root and must retain the child.
        stage(receipt, "stop", || service.stop_game(&game.id))?;
        thread::sleep(Duration::from_secs(6));
        service.reconcile()?;
        stage(receipt, "unmount", || {
            service.perform_runtime(&game.id, "unmount", true)
        })?;
    } else {
        for name in [
            "mount",
            "exact file read",
            "overlay write",
            "launch test fixture",
            "process detection",
            "stop",
            "unmount",
        ] {
            receipt["test_stages"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":name,"status":"NOT RUN"}));
        }
    }
    stage(receipt, "post-unmount verification", || {
        service.start_job(&game.id, Operation::Verify)?;
        wait(&service)
    })?;
    receipt["source_after_hashes"] = json!(hashes(&source)?);
    let unchanged = receipt["source_before_hashes"] == receipt["source_after_hashes"];
    stage(receipt, "source unchanged", || {
        ensure!(unchanged, "Source changed");
        Ok(())
    })?;
    if let Some(mut child) = app_child {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}
fn main() -> Result<()> {
    let values: Vec<_> = std::env::args().skip(1).collect();
    let mut args = BTreeMap::new();
    let mut index = 0;
    while index < values.len() {
        let key = values[index].clone();
        index += 1;
        if key == "--portable" || key == "--service-only" {
            args.insert(key, "true".into());
        } else {
            args.insert(
                key,
                values.get(index).context("Missing argument value")?.clone(),
            );
            index += 1;
        }
    }
    if let Some(root) = args.get("--restore-library") {
        println!(
            "{}",
            Service::restore_library_backup(Path::new(root))?.display()
        );
        return Ok(());
    }
    let work = PathBuf::from(
        args.get("--work")
            .context("--work required (fresh directory)")?,
    );
    fs::create_dir(&work).context("Use a fresh work directory; failed evidence is retained")?;
    let work = work.canonicalize()?;
    let commit = args
        .get("--commit")
        .context("--commit required from artifact manifest")?;
    ensure!(
        commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "Full commit SHA required"
    );
    ensure!(
        commit == env!("PLAYSPARSE_BUILD_COMMIT"),
        "Commit differs from compiled validation tool provenance"
    );
    let evidence = args
        .get("--evidence")
        .map(String::as_str)
        .unwrap_or("physical hardware validation");
    ensure!(
        [
            "CI simulation",
            "hosted native test",
            "physical hardware validation"
        ]
        .contains(&evidence),
        "Invalid evidence level"
    );
    let mut receipt = json!({"schema":1,"OS":std::env::consts::OS,"OS_version":std::env::consts::FAMILY,"architecture":std::env::consts::ARCH,"PlaySparse_version":env!("CARGO_PKG_VERSION"),"commit_SHA":commit,"build_dirty":env!("PLAYSPARSE_BUILD_DIRTY"),"evidence_level":evidence,"started_at":timestamp(),"test_stages":[],"errors":[],"source_before_hashes":{},"source_after_hashes":{}});
    // Host version obtained with literal argv, no shell evaluation.
    #[cfg(unix)]
    if let Ok(output) = Command::new("uname").args(["-srv"]).output() {
        receipt["OS_version"] = json!(String::from_utf8_lossy(&output.stdout).trim());
    }
    #[cfg(windows)]
    {
        receipt["OS_version"] = json!(windows_version());
    }
    let result = run(&args, &work, &mut receipt);
    if let Err(e) = &result {
        receipt["errors"] = json!([format!("{e:#}")]);
    }
    let source = work.join("generated-source");
    if source.is_dir() {
        receipt["source_after_hashes"] = json!(hashes(&source).ok());
    }
    receipt["finished_at"] = json!(timestamp());
    receipt["pass_fail"] = json!(if result.is_ok() {
        if args.contains_key("--portable") {
            "PORTABLE PASS; native stages NOT RUN"
        } else {
            "PASS"
        }
    } else {
        "FAIL"
    });
    let output = work.join(format!("validation-{}.json", std::env::consts::OS));
    fs::write(&output, serde_json::to_vec_pretty(&receipt)?)?;
    println!("{} — receipt: {}", receipt["pass_fail"], output.display());
    fs::write(
        work.join("summary.txt"),
        format!(
            "{}\nEvidence: {evidence}\nCommercial game compatibility: NOT VERIFIED\nReceipt: {}\n",
            receipt["pass_fail"],
            output.display()
        ),
    )?;
    result
}
#[cfg(windows)]
fn windows_version() -> String {
    #[repr(C)]
    struct Version {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(version: *mut Version) -> i32;
    }
    let mut version = Version {
        size: std::mem::size_of::<Version>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    if unsafe { RtlGetVersion(&mut version) } >= 0 {
        format!("{}.{}.{}", version.major, version.minor, version.build)
    } else {
        "NOT VERIFIED".into()
    }
}
