use playsparse_desktop::{
    Operation, RuntimeState, Service, readiness, runtime_state, validate_transition,
};
use std::{fs, path::Path};
#[test]
fn inspection_rejects_stores_duplicates_files_and_missing_folders_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("Game");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"original").unwrap();
    let service = Service::open(&temp.path().join("app"), Path::new("unused")).unwrap();
    let inspection = service.inspect_installation(&source).unwrap();
    assert_eq!(inspection.logical_bytes, 8);
    assert_eq!(inspection.files, 1);
    assert_eq!(fs::read(source.join("data")).unwrap(), b"original");
    assert!(service.inspect_installation(&source.join("data")).is_err());
    assert!(
        service
            .inspect_installation(&temp.path().join("missing"))
            .is_err()
    );
    let game = service.add_game(&source).unwrap();
    assert!(service.inspect_installation(&source).is_err());
    assert_eq!(runtime_state(&game, &[]), RuntimeState::Unoptimized);
    for operation in [
        Operation::Mount,
        Operation::Launch,
        Operation::Stop,
        Operation::Unmount,
        Operation::Recover,
    ] {
        assert!(validate_transition(&game, operation).is_err());
    }
    assert!(validate_transition(&game, Operation::Analyze).is_ok());
    let store = temp.path().join("store");
    fs::create_dir(&store).unwrap();
    fs::write(store.join("COMMITTED.json"), b"{}").unwrap();
    assert!(service.inspect_installation(&store).is_err());
}
#[cfg(unix)]
#[test]
fn discovery_does_not_follow_symlinks_and_excludes_support_programs() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("Game");
    fs::create_dir(&source).unwrap();
    for name in ["game", "uninstaller", "crash-reporter", "updater"] {
        let p = source.join(name);
        fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    symlink("/bin/sh", source.join("outside")).unwrap();
    let service = Service::open(&temp.path().join("app"), Path::new("unused")).unwrap();
    let report = service.inspect_installation(&source).unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].executable, "game");
    let game = service.add_game(&source).unwrap();
    for executable in ["outside", "../Game/game", "missing"] {
        assert!(
            service
                .configure_launch(
                    &game.id,
                    playsparse_desktop::LaunchDescriptor {
                        executable: executable.into(),
                        args: vec![],
                        compatibility_confirmed: true
                    }
                )
                .is_err()
        );
    }
    service
        .configure_launch(
            &game.id,
            playsparse_desktop::LaunchDescriptor {
                executable: "game".into(),
                args: vec!["$(not-expanded)".into()],
                compatibility_confirmed: true,
            },
        )
        .unwrap();
}
#[test]
fn prerequisites_are_not_reported_as_a_successful_mount() {
    use serde_json::json;
    let report = json!({"os":"macos","arch":"aarch64","mount_backend":{"available":true,"backend":"macFUSE"},"mount_test":{"status":"NOT RUN"}});
    assert_eq!(readiness(&report)["state"], "Unknown");
    let mut passed = report.clone();
    passed["mount_test"]["status"] = json!("PASS");
    assert_eq!(readiness(&passed)["state"], "Ready");
    passed["mount_backend"]["available"] = json!(false);
    assert_eq!(readiness(&passed)["state"], "Action required");
    passed["mount_diagnostic"]["status"] = json!("UNSUPPORTED_VERSION");
    assert_eq!(readiness(&passed)["state"], "Unsupported");
}
#[test]
fn running_and_interrupted_states_reject_unsafe_transitions() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("Game");
    fs::create_dir(&source).unwrap();
    let service = Service::open(&temp.path().join("app"), Path::new("unused")).unwrap();
    let mut game = service.add_game(&source).unwrap();
    game.session = Some(playsparse_desktop::Session {
        state: "running".into(),
        mountpoint: temp.path().join("mount"),
        overlay: temp.path().join("overlay"),
        error: None,
    });
    assert_eq!(runtime_state(&game, &[]), RuntimeState::Running);
    for op in [
        Operation::Launch,
        Operation::Unmount,
        Operation::Recover,
        Operation::Optimize,
    ] {
        assert!(validate_transition(&game, op).is_err());
    }
    assert!(validate_transition(&game, Operation::Stop).is_ok());
    game.session.as_mut().unwrap().state = "needs_attention".into();
    assert!(validate_transition(&game, Operation::Launch).is_err());
    assert!(validate_transition(&game, Operation::Recover).is_ok());
}
#[cfg(target_os = "macos")]
#[test]
fn macos_bundle_candidate_validates_plist_executable_and_preserves_bundle_target() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let bundle = temp.path().join("Game.app");
    fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    fs::write(bundle.join("Contents/Info.plist"), b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>Game</string></dict></plist>").unwrap();
    fs::write(bundle.join("Contents/MacOS/Game"), b"fixture").unwrap();
    fs::set_permissions(
        bundle.join("Contents/MacOS/Game"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let candidates = playsparse_desktop::product::inspect(temp.path())
        .unwrap()
        .candidates;
    assert_eq!(candidates[0].executable, "Game.app");
    assert_eq!(candidates[0].kind, "macos_bundle");
    fs::write(bundle.join("Contents/Info.plist"), b"invalid").unwrap();
    assert!(playsparse_desktop::product::bundle_executable(&bundle).is_err());
}

#[test]
fn missing_store_recovery_forgets_metadata_without_deleting_files() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("Game");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"original").unwrap();
    let root = temp.path().join("app");
    let service = Service::open(&root, Path::new("unused")).unwrap();
    let game = service.add_game(&source).unwrap();
    drop(service);
    let path = root.join("library.json");
    let mut db: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    db["games"][0]["store"] = serde_json::json!(temp.path().join("absent-store"));
    db["games"][0]["verified"] = serde_json::json!(true);
    fs::write(&path, serde_json::to_vec(&db).unwrap()).unwrap();
    let restarted = Service::open(&root, Path::new("unused")).unwrap();
    assert!(!restarted.snapshot().games[0].verified);
    restarted.forget_missing_store(&game.id).unwrap();
    assert!(restarted.snapshot().games[0].store.is_none());
    assert_eq!(fs::read(source.join("data")).unwrap(), b"original");
}
