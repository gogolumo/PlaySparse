use playsparse_desktop::{
    LaunchDescriptor, Operation, Service,
    compat::{Record, Status},
};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};
fn wait(s: &Service) -> playsparse_desktop::Job {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let job = s.snapshot().jobs.last().unwrap().clone();
        if job.state != "running" {
            return job;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn repair_retains_corrupt_store_backup_and_unchanged_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"original").unwrap();
    let root = temp.path().join("app");
    let s = Service::open(&root, Path::new("unused")).unwrap();
    let g = s.add_game(&source).unwrap();
    s.start_job(&g.id, Operation::Analyze).unwrap();
    assert_eq!(wait(&s).state, "completed");
    s.start_job(&g.id, Operation::Optimize).unwrap();
    assert_eq!(wait(&s).state, "completed");
    let old = s.snapshot().games[0].store.clone().unwrap();
    fs::write(old.join("COMMITTED.json"), b"{truncated").unwrap();
    s.start_job(&g.id, Operation::Verify).unwrap();
    assert_eq!(wait(&s).state, "failed");
    s.start_job(&g.id, Operation::Repair).unwrap();
    assert_eq!(wait(&s).state, "completed");
    let new = s.snapshot().games[0].store.clone().unwrap();
    assert_ne!(old, new);
    playsparse_store::Store::open(&new)
        .unwrap()
        .verify()
        .unwrap();
    assert_eq!(fs::read(old.join("COMMITTED.json")).unwrap(), b"{truncated");
    assert_eq!(fs::read(source.join("data")).unwrap(), b"original");
    assert!(root.join("library.backup.json").is_file());
}
#[test]
fn repair_failure_keeps_original_pointer_and_corruption_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"original").unwrap();
    let root = temp.path().join("app");
    let s = Service::open(&root, Path::new("unused")).unwrap();
    let g = s.add_game(&source).unwrap();
    s.start_job(&g.id, Operation::Analyze).unwrap();
    wait(&s);
    s.start_job(&g.id, Operation::Optimize).unwrap();
    wait(&s);
    let old = s.snapshot().games[0].store.clone().unwrap();
    fs::write(old.join("COMMITTED.json"), b"bad").unwrap();
    fs::rename(&source, temp.path().join("retained-source")).unwrap();
    s.start_job(&g.id, Operation::Repair).unwrap();
    assert_eq!(wait(&s).state, "failed");
    assert_eq!(s.snapshot().games[0].store.as_ref(), Some(&old));
    assert_eq!(fs::read(old.join("COMMITTED.json")).unwrap(), b"bad");
}
#[test]
fn truncated_metadata_explicit_backup_restore_retains_failed_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let s = Service::open(&root, Path::new("unused")).unwrap();
    let settings = s.snapshot().settings.clone();
    s.update_settings(settings).unwrap();
    drop(s);
    fs::write(root.join("library.json"), b"{truncated").unwrap();
    assert!(Service::open(&root, Path::new("unused")).is_err());
    let evidence = Service::restore_library_backup(&root).unwrap();
    assert_eq!(fs::read(evidence).unwrap(), b"{truncated");
    assert!(Service::open(&root, Path::new("unused")).is_ok());
}
#[test]
fn unpublished_metadata_temp_does_not_override_committed_library() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let s = Service::open(&root, Path::new("unused")).unwrap();
    drop(s);
    fs::write(root.join("interrupted-state.tmp"), b"{partial").unwrap();
    let s = Service::open(&root, Path::new("unused")).unwrap();
    assert!(s.snapshot().games.is_empty());
    assert_eq!(
        fs::read(root.join("interrupted-state.tmp")).unwrap(),
        b"{partial"
    );
}
#[test]
fn profiles_are_bounded_evidence_requires_provenance() {
    for profile in [
        include_bytes!("../../../compat/profiles/plain-executable.json").as_slice(),
        include_bytes!("../../../compat/profiles/launcher-child.json"),
        include_bytes!("../../../compat/profiles/multiple-executables.json"),
        include_bytes!("../../../compat/profiles/case-sensitive.json"),
        include_bytes!("../../../compat/profiles/writable-save.json"),
        include_bytes!("../../../compat/profiles/missing-executable.json"),
        include_bytes!("../../../compat/profiles/crashing-executable.json"),
    ] {
        assert_eq!(Record::parse(profile).unwrap().status, Status::Unknown);
    }
    let mut record: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../compat/profiles/plain-executable.json"
    ))
    .unwrap();
    record["status"] = serde_json::json!("Compatible");
    assert!(Record::parse(&serde_json::to_vec(&record).unwrap()).is_err());
    record["status"] = serde_json::json!("Unsupported");
    assert_eq!(
        Record::parse(&serde_json::to_vec(&record).unwrap())
            .unwrap()
            .status,
        Status::Unsupported
    );
    record["launch_target"] = serde_json::json!("../escape");
    assert!(Record::parse(&serde_json::to_vec(&record).unwrap()).is_err());
    assert!(Record::parse(&vec![b' '; 65537]).is_err());
}
#[test]
fn missing_and_changed_executable_invalidate_confirmation() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("game"), b"old executable").unwrap();
    let s = Service::open(&temp.path().join("app"), Path::new("unused")).unwrap();
    let g = s.add_game(&source).unwrap();
    let descriptor = |name: &str| LaunchDescriptor {
        executable: name.into(),
        args: vec![],
        compatibility_confirmed: true,
    };
    assert!(s.configure_launch(&g.id, descriptor("missing")).is_err());
    s.configure_launch(&g.id, descriptor("game")).unwrap();
    let old = s.snapshot().games[0].launch_fingerprint.clone().unwrap();
    fs::write(source.join("game"), b"replacement").unwrap();
    assert_ne!(
        old,
        playsparse_desktop::compat::fingerprint(&source, "game").unwrap()
    );
}
#[cfg(unix)]
#[test]
fn symlink_hardlink_and_traversal_targets_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("original"), b"unchanged").unwrap();
    fs::hard_link(root.join("original"), root.join("hard")).unwrap();
    std::os::unix::fs::symlink("original", root.join("sym")).unwrap();
    for path in ["hard", "sym", "../escape"] {
        assert!(playsparse_desktop::compat::fingerprint(root, path).is_err());
    }
    assert_eq!(fs::read(root.join("original")).unwrap(), b"unchanged");
}
#[test]
fn diagnostic_bundle_omits_names_paths_arguments_and_tokens() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("private-token-name");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("game"), b"game").unwrap();
    let s = Service::open(&temp.path().join("app"), Path::new("unused")).unwrap();
    let g = s.add_game(&source).unwrap();
    s.configure_launch(
        &g.id,
        LaunchDescriptor {
            executable: "game".into(),
            args: vec!["secret-token".into()],
            compatibility_confirmed: true,
        },
    )
    .unwrap();
    let text = fs::read_to_string(s.export_diagnostics().unwrap()).unwrap();
    assert!(
        !text.contains("secret-token")
            && !text.contains("private-token-name")
            && !text.contains(&temp.path().display().to_string())
    );
}

#[test]
fn evidenced_profile_and_generated_crash_save_case_modes() {
    let mut profile: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../compat/profiles/plain-executable.json"
    ))
    .unwrap();
    profile["status"] = serde_json::json!("Compatible");
    profile["tested_version"] = serde_json::json!("0.1.0");
    profile["test_date"] = serde_json::json!("2026-10-07");
    profile["evidence"] = serde_json::json!([{"level":"CI simulation","receipt":"synthetic-test","commit":"0000000000000000000000000000000000000000"}]);
    assert_eq!(
        Record::parse(&serde_json::to_vec(&profile).unwrap())
            .unwrap()
            .status,
        Status::Compatible
    );
    let fixture = env!("CARGO_BIN_EXE_playsparse-fixture");
    assert_eq!(
        std::process::Command::new(fixture)
            .arg("crash")
            .status()
            .unwrap()
            .code(),
        Some(42)
    );
    let temp = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new(fixture)
            .arg("save")
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::read(temp.path().join("fixture-save.txt")).unwrap(),
        b"isolated save"
    );
    fs::create_dir(temp.path().join("Case")).unwrap();
    fs::write(temp.path().join("Case/Data.txt"), b"case-sensitive fixture").unwrap();
    fs::write(temp.path().join("Case/data.txt"), b"different case fixture").unwrap();
    if fs::read(temp.path().join("Case/Data.txt")).unwrap() == b"case-sensitive fixture" {
        assert!(
            std::process::Command::new(fixture)
                .arg("case")
                .current_dir(temp.path())
                .status()
                .unwrap()
                .success()
        );
    } else {
        // Actual source volume is case insensitive. Profile is Unsupported here, not a passing case translation test.
        profile["status"] = serde_json::json!("Unsupported");
        assert_eq!(
            Record::parse(&serde_json::to_vec(&profile).unwrap())
                .unwrap()
                .status,
            Status::Unsupported
        );
    }
}
