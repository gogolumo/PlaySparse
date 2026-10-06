use playsparse_desktop::{LaunchDescriptor, Operation, Service};
use playsparse_range::RangeResolver;
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn fixture(source: &Path) -> Vec<u8> {
    fs::create_dir_all(source.join("assets")).unwrap();
    let bytes = b"PlaySparse generated integration fixture.\n".repeat(200_000);
    fs::write(source.join("assets/data.bin"), &bytes).unwrap();
    fs::write(source.join("assets/duplicate.bin"), &bytes).unwrap();
    fs::write(
        source.join("readme.txt"),
        b"Exact bytes. Originals retained.\n",
    )
    .unwrap();
    bytes
}
fn wait(service: &Service) -> playsparse_desktop::Job {
    let start = Instant::now();
    loop {
        let job = service.snapshot().jobs.last().unwrap().clone();
        if job.state != "running" {
            return job;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "job timed out");
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn real_engine_flow_persists_library_and_keeps_exact_source_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("Generated game");
    let expected = fixture(&source);
    let root = temp.path().join("application");
    let service = Service::open(&root, Path::new("unused-engine")).unwrap();
    let game = service.add_game(&source).unwrap();
    service.start_job(&game.id, Operation::Analyze).unwrap();
    assert_eq!(wait(&service).state, "completed");
    let snapshot = service.snapshot();
    let analysis = snapshot.games[0].analysis.as_ref().unwrap();
    assert_eq!(analysis["files"], 3);
    assert!(analysis["exact_duplicate_file_bytes"].as_u64().unwrap() > 0);
    assert!(
        analysis["temporary_workspace"]["required_estimated_bytes"]
            .as_u64()
            .unwrap()
            > expected.len() as u64 * 4
    );
    service.start_job(&game.id, Operation::Optimize).unwrap();
    assert_eq!(wait(&service).state, "completed");
    let store = service.snapshot().games[0].store.clone().unwrap();
    assert!(service.snapshot().games[0].verified);
    assert_eq!(
        RangeResolver::open(&store, 1024 * 1024)
            .unwrap()
            .read_range("assets/data.bin", 123, 10000)
            .unwrap(),
        expected[123..10123]
    );
    service.start_job(&game.id, Operation::Verify).unwrap();
    assert_eq!(wait(&service).state, "completed");
    assert_eq!(fs::read(source.join("assets/data.bin")).unwrap(), expected);
    drop(service);
    let restarted = Service::open(&root, Path::new("unused-engine")).unwrap();
    assert_eq!(
        restarted.snapshot().games[0].source,
        source.canonicalize().unwrap()
    );
    assert_eq!(restarted.snapshot().games[0].store.as_ref(), Some(&store));
    restarted.remove_game(&game.id).unwrap();
    assert!(source.is_dir() && store.is_dir());
}
#[test]
fn overlap_traversal_conflicting_jobs_and_cancellation_are_rejected_safely() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fixture(&source);
    let root = temp.path().join("application");
    let service = Service::open(&root, Path::new("unused")).unwrap();
    assert!(service.add_game(temp.path()).is_err());
    let game = service.add_game(&source).unwrap();
    assert!(service.add_game(&source).is_err());
    let mut settings = service.snapshot().settings;
    settings.storage_dir = source.join("store");
    assert!(service.update_settings(settings).is_err());
    assert!(!source.join("store").exists());
    assert!(
        service
            .configure_launch(
                &game.id,
                LaunchDescriptor {
                    executable: "../game".into(),
                    args: vec![],
                    compatibility_confirmed: true
                }
            )
            .is_err()
    );
    let job = service.start_job(&game.id, Operation::Analyze).unwrap();
    assert!(service.start_job(&game.id, Operation::Analyze).is_err());
    assert!(service.remove_game(&game.id).is_err());
    service.cancel_job(&job.id).unwrap();
    assert_eq!(wait(&service).state, "cancelled");
    assert!(service.snapshot().games[0].analysis.is_none());
    assert!(Service::open(&root, Path::new("unused")).is_err());
}
#[test]
fn engine_failure_never_registers_success_and_database_corruption_is_retained() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("application");
    let service = Service::open(&root, Path::new("unused")).unwrap();
    let source = temp.path().join("source");
    fixture(&source);
    let game = service.add_game(&source).unwrap();
    fs::remove_dir_all(&source).unwrap();
    service.start_job(&game.id, Operation::Analyze).unwrap();
    assert_eq!(wait(&service).state, "failed");
    assert!(service.snapshot().games[0].analysis.is_none());
    assert!(!service.snapshot().games[0].verified);
    drop(service);
    fs::write(root.join("library.json"), "{bad").unwrap();
    assert!(Service::open(&root, Path::new("unused")).is_err());
    assert_eq!(
        fs::read_to_string(root.join("library.json")).unwrap(),
        "{bad"
    );
}

#[test]
fn cancellation_at_publication_removes_staging_and_preserves_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let expected = fixture(&source);
    let destination = temp.path().join("store");
    let mut stages = Vec::new();
    let result = playsparse_store::pack_directory_observed(
        &source,
        &destination,
        &Default::default(),
        &mut |progress| {
            stages.push(progress.stage);
            if progress.stage == "publishing" {
                Err(playsparse_core::Error::Invalid("Test cancellation".into()))
            } else {
                Ok(())
            }
        },
    );
    assert!(result.is_err());
    assert!(stages.contains(&"verifying"));
    assert!(!destination.exists());
    assert!(!fs::read_dir(temp.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".playsparse-transaction-")
    }));
    assert_eq!(fs::read(source.join("assets/data.bin")).unwrap(), expected);
}

#[test]
fn concurrent_registration_cannot_duplicate_a_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fixture(&source);
    let service = Service::open(&temp.path().join("app"), Path::new("unused")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let service = service.clone();
            let source = source.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                service.add_game(&source).is_ok()
            })
        })
        .collect();
    let successes = threads
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(successes, 1);
    assert_eq!(service.snapshot().games.len(), 1);
}
#[test]
fn interrupted_jobs_and_sessions_become_attention_without_killing_persisted_pids() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("application");
    let source = temp.path().join("source");
    fixture(&source);
    let service = Service::open(&root, Path::new("unused")).unwrap();
    let game = service.add_game(&source).unwrap();
    drop(service);
    let path = root.join("library.json");
    let mut db: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    db["games"][0]["session"] = serde_json::json!({"state":"running", "mountpoint":root.canonicalize().unwrap().join("runtimes").join(&game.id).join("mount"),"overlay":root.join("overlay"),"error":null});
    db["jobs"] = serde_json::json!([{"id":"interrupted", "game_id":game.id, "operation":"analyze", "state":"running", "stage":"packing", "bytes":42,"files":0,"started_at":1,"finished_at":null,"error":null,"cancellable":true}]);
    fs::write(&path, serde_json::to_vec(&db).unwrap()).unwrap();
    let service = Service::open(&root, Path::new("unused")).unwrap();
    assert_eq!(service.snapshot().jobs[0].state, "interrupted");
    assert_eq!(
        service.snapshot().games[0].session.as_ref().unwrap().state,
        "needs_attention"
    );
    assert!(!service.can_close());
    #[cfg(unix)]
    {
        service.recover_session(&game.id).unwrap();
        assert!(service.can_close());
        assert!(service.snapshot().games[0].session.is_none());
    }
}
/// Opt-in physical mount gate, never confused with ordinary engine tests.
#[test]
#[ignore = "requires installed filesystem driver and PLAYSPARSE_DESKTOP_ENGINE"]
fn native_mount_exact_bytes_launch_stop_unmount() {
    let engine = std::env::var_os("PLAYSPARSE_DESKTOP_ENGINE").expect("Set absolute engine path");
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let expected = fixture(&source);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::write(source.join("fixture-game"), b"#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(
            source.join("fixture-game"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let service = Service::open(&temp.path().join("app"), Path::new(&engine)).unwrap();
    let game = service.add_game(&source).unwrap();
    service.start_job(&game.id, Operation::Analyze).unwrap();
    assert_eq!(wait(&service).state, "completed");
    service.start_job(&game.id, Operation::Optimize).unwrap();
    assert_eq!(wait(&service).state, "completed");
    service.mount_game(&game.id).unwrap();
    let mount = service.snapshot().games[0]
        .session
        .as_ref()
        .unwrap()
        .mountpoint
        .clone();
    #[cfg(windows)]
    let mount = std::path::PathBuf::from(format!("{}\\", mount.display()));
    assert_eq!(fs::read(mount.join("assets/data.bin")).unwrap(), expected);
    fs::write(mount.join("save.txt"), b"overlay save").unwrap();
    assert!(!source.join("save.txt").exists());
    #[cfg(unix)]
    {
        service
            .configure_launch(
                &game.id,
                LaunchDescriptor {
                    executable: "fixture-game".into(),
                    args: vec![],
                    compatibility_confirmed: true,
                },
            )
            .unwrap_err();
        // Launch configuration is deliberately immutable while mounted.
    }
    service.unmount_game(&game.id, true).unwrap();
    #[cfg(unix)]
    {
        service
            .configure_launch(
                &game.id,
                LaunchDescriptor {
                    executable: "fixture-game".into(),
                    args: vec![],
                    compatibility_confirmed: true,
                },
            )
            .unwrap();
        service.mount_game(&game.id).unwrap();
        service.launch_game(&game.id).unwrap();
        assert_eq!(
            service.snapshot().games[0].session.as_ref().unwrap().state,
            "running"
        );
        assert!(service.unmount_game(&game.id, true).is_err());
        service.stop_game(&game.id).unwrap();
        service.unmount_game(&game.id, true).unwrap();
    }
    assert!(service.snapshot().games[0].session.is_none());
    assert_eq!(fs::read(source.join("assets/data.bin")).unwrap(), expected);
}
