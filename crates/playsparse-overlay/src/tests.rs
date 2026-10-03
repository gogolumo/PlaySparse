use super::*;
use playsparse_core::{Chunker, Layout};
use playsparse_store::{PackOptions, pack_directory};
use std::{fs, path::PathBuf, sync::Barrier};

struct Fixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    store: PathBuf,
    root: PathBuf,
    base: Arc<RangeResolver>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_layout(Layout::Packs)
    }

    fn with_layout(layout: Layout) -> Self {
        let temp = tempfile::tempdir().unwrap();
        // macOS /var and /tmp are system symlinks. Overlay storage deliberately
        // requires canonical ancestors; fixture paths use their real spelling.
        let path = fs::canonicalize(temp.path()).unwrap();
        let source = path.join("source");
        fs::create_dir_all(source.join("dir/nested")).unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        fs::write(source.join("alpha"), b"abcdefghijklmnop").unwrap();
        fs::write(source.join("replace"), b"old destination").unwrap();
        fs::write(source.join("dir/nested/file"), b"nested immutable bytes").unwrap();
        fs::write(source.join("empty-file"), []).unwrap();
        let store = path.join("store");
        pack_directory(
            &source,
            &store,
            &PackOptions {
                layout,
                chunker: Chunker::Fixed,
                chunk_size: 4096,
                ..Default::default()
            },
        )
        .unwrap();
        let base = Arc::new(RangeResolver::open(&store, 32 * 1024).unwrap());
        let root = path.join("overlay");
        Self {
            _temp: temp,
            source,
            store,
            root,
            base,
        }
    }

    fn open(&self) -> Overlay {
        Overlay::open(self.base.clone(), &self.root).unwrap()
    }
}

fn bytes(overlay: &Overlay, path: &str) -> Vec<u8> {
    let handle = overlay.open_file(path, false, false).unwrap();
    overlay.read(&handle, 0, 1024 * 1024).unwrap()
}

#[test]
fn handle_delete_preserves_replacements_and_follows_live_renames() {
    let fixture = Fixture::new();
    let overlay = fixture.open();
    let old = overlay.open_file("replace", false, false).unwrap();
    let replacement = overlay.create("staged", 0o644, true).unwrap();
    overlay
        .write(&replacement, 0, b"new replacement", false)
        .unwrap();
    overlay.rename("staged", "replace", true).unwrap();
    overlay.delete_handle(&old).unwrap();
    overlay.delete_handle(&old).unwrap();
    assert_eq!(bytes(&overlay, "replace"), b"new replacement");
    assert_eq!(overlay.read(&old, 0, 64).unwrap(), b"old destination");

    overlay.rename("replace", "moved", false).unwrap();
    overlay.delete_handle(&replacement).unwrap();
    assert!(overlay.metadata("moved").is_err());
    assert_eq!(
        overlay.read(&replacement, 0, 64).unwrap(),
        b"new replacement"
    );

    let nonempty = overlay.open_dir("dir").unwrap();
    assert!(
        matches!(overlay.delete_handle(&nonempty), Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::DirectoryNotEmpty)
    );
    let old_directory = overlay.open_dir("empty").unwrap();
    overlay.mkdir("new-directory", 0o755).unwrap();
    overlay.rename("new-directory", "empty", true).unwrap();
    let child = overlay.create("empty/child", 0o644, true).unwrap();
    overlay.write(&child, 0, b"keep this child", false).unwrap();
    overlay.delete_handle(&old_directory).unwrap();
    assert_eq!(bytes(&overlay, "empty/child"), b"keep this child");
    overlay.sync().unwrap();
    drop(child);
    drop(nonempty);
    drop(old_directory);
    drop(old);
    drop(replacement);
    drop(overlay);
    let remounted = fixture.open();
    assert_eq!(bytes(&remounted, "empty/child"), b"keep this child");
    assert!(remounted.metadata("moved").is_err());
}

fn digest_tree(root: &Path) -> BTreeMap<String, String> {
    fn collect(root: &Path, path: &Path, result: &mut BTreeMap<String, String>) {
        for item in fs::read_dir(path).unwrap() {
            let path = item.unwrap().path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                result.insert(name, "directory".into());
                collect(root, &path, result);
            } else {
                result.insert(name, blake3::hash(&fs::read(path).unwrap()).to_string());
            }
        }
    }
    let mut result = BTreeMap::new();
    collect(root, root, &mut result);
    result
}

fn is_kind(error: Error, expected: std::io::ErrorKind) -> bool {
    matches!(error, Error::Io(error) if error.kind() == expected)
}

#[test]
fn create_partial_append_truncate_modes_and_remount() {
    for layout in [Layout::Packs, Layout::Loose] {
        let fixture = Fixture::with_layout(layout);
        let original = digest_tree(&fixture.source);
        let immutable = digest_tree(&fixture.store);
        {
            let overlay = fixture.open();
            assert_eq!(overlay.metadata("").unwrap().id, 1);
            assert!(overlay.metadata("").unwrap().is_dir);
            let read = overlay.open_file("alpha", false, false).unwrap();
            let write = overlay.open_file("alpha", true, false).unwrap();
            assert_eq!(overlay.metrics().copy_up_files, 0);
            assert_eq!(overlay.write(&write, 4, b"WXYZ", false).unwrap(), 4);
            assert_eq!(overlay.read(&read, 0, 100).unwrap(), b"abcdWXYZijklmnop");
            assert_eq!(overlay.write(&write, u64::MAX, b"!", true).unwrap(), 1);
            overlay.truncate(&write, 12).unwrap();
            overlay.truncate(&write, 20).unwrap();
            assert_eq!(
                overlay.read(&read, 0, 30).unwrap(),
                b"abcdWXYZijkl\0\0\0\0\0\0\0\0"
            );
            overlay.flush(&write).unwrap();
            overlay.mkdir("new-dir", 0o750).unwrap();
            let created = overlay.create("new-dir/file", 0o640, true).unwrap();
            overlay.write(&created, 3, b"123", false).unwrap();
            overlay.set_mode_handle(&created, 0o600).unwrap();
            assert_eq!(overlay.metadata_handle(&created).unwrap().size, 6);
            overlay.flush_all().unwrap();
            let metrics = overlay.metrics();
            assert_eq!(metrics.copy_up_files, 1);
            assert_eq!(metrics.copy_up_bytes, 16);
            assert_eq!(metrics.overlay_bytes_written, 8);
            assert_eq!(metrics.overlay_files, 2);
            assert_eq!(metrics.overlay_logical_bytes, 26);
        }
        let status = Overlay::status(&fixture.root).unwrap();
        assert_eq!(status.copy_up_files, 0);
        assert_eq!(status.overlay_files, 2);
        {
            let overlay = fixture.open();
            assert_eq!(bytes(&overlay, "alpha"), b"abcdWXYZijkl\0\0\0\0\0\0\0\0");
            assert_eq!(bytes(&overlay, "new-dir/file"), b"\0\0\x00123");
            assert_eq!(overlay.metadata("new-dir/file").unwrap().mode, 0o600);
            assert_eq!(overlay.metadata("new-dir").unwrap().mode, 0o750);
        }
        assert_eq!(digest_tree(&fixture.source), original);
        assert_eq!(digest_tree(&fixture.store), immutable);
        fixture.base.store().verify().unwrap();
    }
}

#[test]
fn stable_file_handles_survive_rename_replace_and_unlink() {
    let fixture = Fixture::new();
    {
        let overlay = fixture.open();
        let source = overlay.open_file("alpha", true, false).unwrap();
        let old_destination = overlay.open_file("replace", true, false).unwrap();
        let source_id = overlay.metadata_handle(&source).unwrap().id;
        overlay.rename("alpha", "replace", true).unwrap();
        assert_eq!(overlay.metadata_handle(&source).unwrap().path, "replace");
        assert_eq!(overlay.metadata("replace").unwrap().id, source_id);
        assert_eq!(bytes(&overlay, "replace"), b"abcdefghijklmnop");
        assert_eq!(
            overlay.read(&old_destination, 0, 100).unwrap(),
            b"old destination"
        );
        // Mutating the replaced base inode must not overwrite the new path.
        overlay
            .write(&old_destination, 0, b"orphan", false)
            .unwrap();
        overlay.set_mode_handle(&old_destination, 0o600).unwrap();
        assert_eq!(bytes(&overlay, "replace"), b"abcdefghijklmnop");
        assert_ne!(overlay.metadata("replace").unwrap().mode, 0o600);
        overlay.write(&source, 0, b"new", false).unwrap();
        overlay.unlink("replace").unwrap();
        assert!(overlay.metadata("replace").is_err());
        assert!(overlay.metadata_id(source_id).is_err());
        assert_eq!(overlay.read(&source, 0, 100).unwrap(), b"newdefghijklmnop");
        overlay.write(&source, 3, b"!", false).unwrap();
        overlay.flush(&source).unwrap();
        let fresh = overlay.create("replace", 0o644, true).unwrap();
        overlay.write(&fresh, 0, b"fresh", false).unwrap();
        assert_ne!(overlay.metadata_handle(&fresh).unwrap().id, source_id);
        assert_eq!(overlay.read(&source, 0, 100).unwrap(), b"new!efghijklmnop");
        assert_eq!(bytes(&overlay, "replace"), b"fresh");
    }
    let overlay = fixture.open();
    assert!(overlay.metadata("alpha").is_err());
    assert_eq!(bytes(&overlay, "replace"), b"fresh");
}

#[test]
fn renamed_base_tree_directory_handles_and_tombstones_persist() {
    let fixture = Fixture::new();
    {
        let overlay = fixture.open();
        let dir = overlay.open_dir("dir").unwrap();
        let nested = overlay.open_dir("dir/nested").unwrap();
        let file = overlay.open_file("dir/nested/file", true, false).unwrap();
        let file_id = overlay.metadata_handle(&file).unwrap().id;
        overlay.rename("dir", "moved", false).unwrap();
        assert_eq!(overlay.handle_entry(&nested).unwrap().path, "moved/nested");
        assert_eq!(overlay.list_handle(&dir).unwrap()[0].path, "moved/nested");
        assert_eq!(
            overlay.metadata_id(file_id).unwrap().path,
            "moved/nested/file"
        );
        assert_eq!(
            overlay.read(&file, 0, 100).unwrap(),
            b"nested immutable bytes"
        );
        overlay.set_mode("moved/nested", 0o700).unwrap();
        assert_eq!(overlay.metrics().copy_up_files, 0);
        overlay.unlink("moved/nested/file").unwrap();
        overlay.rmdir("moved/nested").unwrap();
        assert!(overlay.list_handle(&nested).unwrap().is_empty());
        overlay.rmdir("moved").unwrap();
        assert!(overlay.list_handle(&dir).unwrap().is_empty());
        assert_eq!(
            overlay.read(&file, 0, 100).unwrap(),
            b"nested immutable bytes"
        );
        // Recreating original directory leaves old descendants hidden.
        overlay.mkdir("dir", 0o755).unwrap();
        assert!(overlay.list("dir").unwrap().is_empty());
    }
    let overlay = fixture.open();
    assert!(overlay.list("dir").unwrap().is_empty());
    assert!(overlay.metadata("moved").is_err());
    assert!(overlay.metadata("dir/nested/file").is_err());
}

#[test]
fn rename_empty_directory_replace_and_file_replace_remount() {
    let fixture = Fixture::new();
    {
        let overlay = fixture.open();
        overlay.mkdir("staging", 0o700).unwrap();
        overlay.rename("staging", "empty", true).unwrap();
        assert_eq!(overlay.metadata("empty").unwrap().mode, 0o700);
        let replacement = overlay.create("replacement.tmp", 0o755, true).unwrap();
        overlay
            .write(&replacement, 0, b"new executable", false)
            .unwrap();
        overlay.flush(&replacement).unwrap();
        overlay.rename("replacement.tmp", "replace", true).unwrap();
        assert_eq!(bytes(&overlay, "replace"), b"new executable");
    }
    let overlay = fixture.open();
    assert_eq!(bytes(&overlay, "replace"), b"new executable");
    assert_eq!(overlay.metadata("replace").unwrap().mode, 0o755);
    assert_eq!(overlay.metadata("empty").unwrap().mode, 0o700);
}

#[test]
fn strict_paths_read_permissions_limits_and_wrong_types() {
    let fixture = Fixture::new();
    let overlay = fixture.open();
    for path in [
        "../x", "/x", "C:/x", "a\\x", "a//x", "a/./x", "a/../x", "a\0", "a\nx",
    ] {
        assert!(overlay.metadata(path).is_err(), "{path:?}");
        assert!(overlay.open_file(path, true, false).is_err());
        assert!(overlay.create(path, 0o644, true).is_err());
        assert!(overlay.mkdir(path, 0o755).is_err());
        assert!(overlay.unlink(path).is_err());
        assert!(overlay.rmdir(path).is_err());
        assert!(overlay.rename("alpha", path, true).is_err());
        assert!(overlay.rename(path, "target", true).is_err());
    }
    let read = overlay.open_file("alpha", false, false).unwrap();
    assert!(is_kind(
        overlay.write(&read, 0, b"no", false).unwrap_err(),
        std::io::ErrorKind::PermissionDenied
    ));
    assert!(is_kind(
        overlay.truncate(&read, 0).unwrap_err(),
        std::io::ErrorKind::PermissionDenied
    ));
    assert!(matches!(
        overlay.read(&read, 0, MAX_READ_BYTES + 1),
        Err(Error::ReadTooLarge)
    ));
    assert!(overlay.read(&read, u64::MAX, 10).unwrap().is_empty());
    let write = overlay.open_file("alpha", true, false).unwrap();
    assert!(overlay.write(&write, u64::MAX, b"x", false).is_err());
    assert!(overlay.truncate(&write, MAX_FILE_SIZE + 1).is_err());
    assert_eq!(overlay.metrics().copy_up_files, 0);
    assert!(is_kind(
        overlay.open_file("dir", false, false).err().unwrap(),
        std::io::ErrorKind::IsADirectory
    ));
    assert!(is_kind(
        overlay.open_dir("alpha").err().unwrap(),
        std::io::ErrorKind::NotADirectory
    ));
    assert!(is_kind(
        overlay.list("alpha").unwrap_err(),
        std::io::ErrorKind::NotADirectory
    ));
    assert!(is_kind(
        overlay.unlink("dir").unwrap_err(),
        std::io::ErrorKind::IsADirectory
    ));
    assert!(is_kind(
        overlay.rmdir("alpha").unwrap_err(),
        std::io::ErrorKind::NotADirectory
    ));
    assert!(is_kind(
        overlay.rmdir("dir").unwrap_err(),
        std::io::ErrorKind::DirectoryNotEmpty
    ));
    assert!(overlay.unlink("").is_err());
    assert!(overlay.rmdir("").is_err());
    assert!(overlay.mkdir("", 0o755).is_err());
    assert!(overlay.create("", 0o644, true).is_err());
    assert!(is_kind(
        overlay.create("alpha", 0o644, true).err().unwrap(),
        std::io::ErrorKind::AlreadyExists
    ));
    assert!(is_kind(
        overlay.create("alpha/child", 0o644, true).err().unwrap(),
        std::io::ErrorKind::NotADirectory
    ));
    assert!(overlay.create("missing/child", 0o644, true).is_err());
    assert!(overlay.create("bad-mode", 0o7777, true).is_err());
    assert!(overlay.set_mode("alpha", 0o7777).is_err());
    assert!(is_kind(
        overlay.rename("alpha", "replace", false).unwrap_err(),
        std::io::ErrorKind::AlreadyExists
    ));
    assert!(is_kind(
        overlay.rename("dir", "alpha", true).unwrap_err(),
        std::io::ErrorKind::NotADirectory
    ));
    assert!(is_kind(
        overlay.rename("alpha", "dir", true).unwrap_err(),
        std::io::ErrorKind::IsADirectory
    ));
    assert!(is_kind(
        overlay.rename("empty", "dir", true).unwrap_err(),
        std::io::ErrorKind::DirectoryNotEmpty
    ));
    assert!(overlay.rename("dir", "dir/nested/new", true).is_err());
    assert!(overlay.rename("alpha", "alpha/new", true).is_err());
    overlay.rename("alpha", "alpha", false).unwrap();
}

#[test]
fn truncate_zero_skips_copy_and_large_sparse_offsets_stay_u64() {
    let fixture = Fixture::new();
    let overlay = fixture.open();
    let handle = overlay.open_file("alpha", true, true).unwrap();
    assert_eq!(overlay.read(&handle, 0, 100).unwrap(), b"");
    assert_eq!(overlay.metrics().copy_up_files, 1);
    assert_eq!(overlay.metrics().copy_up_bytes, 0);
    let large = overlay.create("large", 0o644, true).unwrap();
    let offset = (8u64 << 30) + 7;
    overlay.truncate(&large, offset + 4).unwrap();
    overlay.write(&large, offset, b"huge", false).unwrap();
    assert_eq!(overlay.metadata_handle(&large).unwrap().size, offset + 4);
    assert_eq!(overlay.read(&large, offset - 2, 6).unwrap(), b"\0\0huge");
    assert_eq!(overlay.read(&large, offset + 4, 10).unwrap(), b"");
    assert_eq!(overlay.read(&large, u64::MAX, 10).unwrap(), b"");
}

#[test]
fn exclusive_lock_is_retained_by_handles_and_discard_preserves_base() {
    let fixture = Fixture::new();
    let immutable = digest_tree(&fixture.store);
    let original = digest_tree(&fixture.source);
    let overlay = fixture.open();
    let handle = overlay.create("added", 0o644, true).unwrap();
    overlay.write(&handle, 0, b"data", false).unwrap();
    overlay.unlink("alpha").unwrap();
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    assert!(Overlay::status(&fixture.root).is_err());
    assert!(Overlay::discard(&fixture.root).is_err());
    drop(overlay);
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    drop(handle);
    assert_eq!(Overlay::status(&fixture.root).unwrap().overlay_files, 1);
    Overlay::discard(&fixture.root).unwrap();
    assert_eq!(Overlay::status(&fixture.root).unwrap().overlay_files, 0);
    assert!(fixture.root.join(LOCK).is_file());
    assert!(fixture.root.join(STATE).is_file());
    let overlay = fixture.open();
    assert_eq!(bytes(&overlay, "alpha"), b"abcdefghijklmnop");
    assert!(overlay.metadata("added").is_err());
    assert_eq!(digest_tree(&fixture.store), immutable);
    assert_eq!(digest_tree(&fixture.source), original);
}

#[cfg(windows)]
#[test]
fn canonical_verbatim_windows_storage_paths_are_supported() {
    let fixture = Fixture::new();
    let parent = fs::canonicalize(fixture.root.parent().unwrap()).unwrap();
    assert!(parent.as_os_str().to_string_lossy().starts_with(r"\\?\"));
    let root = parent.join("verbatim-overlay");
    let overlay = Overlay::open(fixture.base.clone(), &root).unwrap();
    let handle = overlay.create("file", 0o644, true).unwrap();
    overlay
        .write(&handle, 0, b"verbatim path works", false)
        .unwrap();
    overlay.flush(&handle).unwrap();
    drop(handle);
    drop(overlay);
    assert_eq!(Overlay::status(&root).unwrap().overlay_files, 1);
    let overlay = Overlay::open(fixture.base.clone(), &root).unwrap();
    assert_eq!(bytes(&overlay, "file"), b"verbatim path works");
}

#[test]
fn refuses_overlap_unrecognized_roots_and_foreign_handles() {
    let fixture = Fixture::new();
    for path in [
        &fixture.store,
        &fixture.store.join("nested-overlay"),
        fixture.store.parent().unwrap(),
    ] {
        assert!(Overlay::open(fixture.base.clone(), path).is_err());
    }
    assert!(!fixture.store.join("nested-overlay").exists());
    fs::create_dir(&fixture.root).unwrap();
    fs::write(fixture.root.join("user-file"), b"never delete").unwrap();
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    assert!(Overlay::discard(&fixture.root).is_err());
    assert!(!fixture.root.join(LOCK).exists());
    assert_eq!(
        fs::read(fixture.root.join("user-file")).unwrap(),
        b"never delete"
    );
    fs::remove_file(fixture.root.join("user-file")).unwrap();
    let overlay = fixture.open();
    let other_fixture = Fixture::new();
    let other = other_fixture.open();
    let foreign = other.open_file("alpha", true, false).unwrap();
    assert!(overlay.read(&foreign, 0, 2).is_err());
    assert!(overlay.write(&foreign, 0, b"x", false).is_err());
    assert!(overlay.truncate(&foreign, 0).is_err());
    assert!(overlay.flush(&foreign).is_err());
    assert!(overlay.metadata_handle(&foreign).is_err());
}

#[test]
fn metadata_corruption_missing_objects_and_wrong_base_fail_closed() {
    let fixture = Fixture::new();
    let data_name = {
        let overlay = fixture.open();
        let handle = overlay.create("added", 0o644, true).unwrap();
        overlay.write(&handle, 0, b"data", false).unwrap();
        super::data_name(overlay.metadata_handle(&handle).unwrap().id)
    };
    let valid = fs::read(fixture.root.join(STATE)).unwrap();
    fs::write(fixture.root.join(STATE), b"{truncated").unwrap();
    assert!(matches!(
        Overlay::open(fixture.base.clone(), &fixture.root),
        Err(Error::Corrupt(_))
    ));
    assert!(Overlay::discard(&fixture.root).is_err());
    fs::write(fixture.root.join(STATE), &valid).unwrap();
    let mut envelope: Envelope = serde_json::from_slice(&valid).unwrap();
    envelope.snapshot.tombstones.insert("alpha".into());
    fs::write(
        fixture.root.join(STATE),
        serde_json::to_vec(&envelope).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        Overlay::open(fixture.base.clone(), &fixture.root),
        Err(Error::Corrupt(_))
    ));
    fs::write(fixture.root.join(STATE), &valid).unwrap();
    let moved = fixture.root.join("removed-data");
    fs::rename(fixture.root.join("data").join(&data_name), &moved).unwrap();
    // Unknown files in the overlay root are refused before any mutation.
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    fs::remove_file(&moved).unwrap();
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    fs::write(fixture.root.join("data").join(data_name), b"restored").unwrap();
    let second_source = fixture.store.parent().unwrap().join("different-source");
    fs::create_dir(&second_source).unwrap();
    fs::write(second_source.join("other"), b"other base").unwrap();
    let second_store = fixture.store.parent().unwrap().join("different-store");
    pack_directory(&second_source, &second_store, &PackOptions::default()).unwrap();
    let second = Arc::new(RangeResolver::open(&second_store, 0).unwrap());
    assert!(matches!(
        Overlay::open(second, &fixture.root),
        Err(Error::Corrupt(_))
    ));
    assert!(Overlay::status(&fixture.root).is_ok());
}

fn edit_snapshot(root: &Path, edit: impl FnOnce(&mut Snapshot)) {
    let state = root.join(STATE);
    let mut envelope: Envelope = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    edit(&mut envelope.snapshot);
    envelope.snapshot_blake3 =
        blake3::hash(&serde_json::to_vec(&envelope.snapshot).unwrap()).to_string();
    fs::write(state, serde_json::to_vec(&envelope).unwrap()).unwrap();
}

#[test]
fn syntactically_valid_corrupt_metadata_is_rejected() {
    let fixture = Fixture::new();
    {
        let overlay = fixture.open();
        overlay.create("added", 0o644, true).unwrap();
    }
    let state = fs::read(fixture.root.join(STATE)).unwrap();
    type SnapshotEdit = Box<dyn FnOnce(&mut Snapshot)>;
    let changes: Vec<SnapshotEdit> = vec![
        Box::new(|snapshot| {
            snapshot.overrides.get_mut("added").unwrap().id = 1;
        }),
        Box::new(|snapshot| {
            snapshot.overrides.get_mut("added").unwrap().mode = 0o7777;
        }),
        Box::new(|snapshot| {
            snapshot.next_id = 1;
        }),
        Box::new(|snapshot| {
            snapshot.tombstones.insert("../escape".into());
        }),
        Box::new(|snapshot| {
            snapshot.tombstones.insert("added".into());
        }),
        Box::new(|snapshot| {
            let record = snapshot.overrides.remove("added").unwrap();
            snapshot.overrides.insert("missing/child".into(), record);
        }),
        Box::new(|snapshot| {
            let record = snapshot.overrides.remove("added").unwrap();
            snapshot.overrides.insert("alpha/child".into(), record);
        }),
        Box::new(|snapshot| {
            snapshot.overrides.get_mut("added").unwrap().content = RecordContent::Base {
                source: "../escape".into(),
            };
        }),
        Box::new(|snapshot| {
            snapshot.overrides.get_mut("added").unwrap().content = RecordContent::Base {
                source: "dir".into(),
            };
        }),
        Box::new(|snapshot| {
            snapshot.overrides.get_mut("added").unwrap().content = RecordContent::Base {
                source: "alpha".into(),
            };
        }),
    ];
    for edit in changes {
        fs::write(fixture.root.join(STATE), &state).unwrap();
        edit_snapshot(&fixture.root, edit);
        assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    }
}

#[test]
fn interrupted_metadata_and_copy_up_orphans_are_collected_without_data_loss() {
    let fixture = Fixture::new();
    let kept = {
        let overlay = fixture.open();
        let handle = overlay.create("kept", 0o644, true).unwrap();
        overlay.write(&handle, 0, b"published", false).unwrap();
        data_name(overlay.metadata_handle(&handle).unwrap().id)
    };
    fs::write(fixture.root.join(temp_name(0)), b"interrupted metadata").unwrap();
    fs::write(
        fixture.root.join("data").join("ffffffffffffff00.data"),
        b"incomplete unreferenced copy-up",
    )
    .unwrap();
    let overlay = fixture.open();
    assert_eq!(bytes(&overlay, "kept"), b"published");
    assert_eq!(bytes(&overlay, "alpha"), b"abcdefghijklmnop");
    assert!(!fixture.root.join(temp_name(0)).exists());
    assert!(!fixture.root.join("data/ffffffffffffff00.data").exists());
    assert!(fixture.root.join("data").join(kept).is_file());
    assert_eq!(overlay.metrics().overlay_files, 1);
}

#[test]
fn per_file_serialization_shared_reads_and_atomic_append() {
    let fixture = Fixture::new();
    let overlay = fixture.open();
    let handle = overlay.open_file("alpha", true, false).unwrap();
    let barrier = Arc::new(Barrier::new(9));
    let mut workers = Vec::new();
    for id in 0..8u8 {
        let overlay = overlay.clone();
        let handle = handle.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..30 {
                overlay.write(&handle, 0, &[id + b'0'; 64], true).unwrap();
                let read = overlay.read(&handle, 0, 16).unwrap();
                assert_eq!(read, b"abcdefghijklmnop");
                overlay.metadata_id(handle.node.id).unwrap();
                overlay.entries().unwrap();
                overlay.list("").unwrap();
            }
        }));
    }
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    let result = overlay.read(&handle, 0, 100000).unwrap();
    assert_eq!(result.len(), 16 + 8 * 30 * 64);
    for chunk in result[16..].as_chunks::<64>().0 {
        assert!(chunk.iter().all(|byte| *byte == chunk[0]));
    }
    assert_eq!(overlay.metrics().copy_up_files, 1);
    assert_eq!(overlay.metrics().copy_up_bytes, 16);
    assert_eq!(overlay.metrics().overlay_bytes_written, 8 * 30 * 64);
}

#[cfg(unix)]
#[test]
fn symlink_root_ancestor_data_metadata_and_object_escapes_are_rejected() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let parent = fixture.root.parent().unwrap();
    let outside = parent.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("valuable"), b"never touch").unwrap();
    symlink(&outside, &fixture.root).unwrap();
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    assert!(Overlay::discard(&fixture.root).is_err());
    assert!(Overlay::open(fixture.base.clone(), &fixture.root.join("nested")).is_err());
    fs::remove_file(&fixture.root).unwrap();
    let id = {
        let overlay = fixture.open();
        let handle = overlay.create("file", 0o644, true).unwrap();
        overlay.write(&handle, 0, b"data", false).unwrap();
        overlay.metadata_handle(&handle).unwrap().id
    };
    let object = fixture.root.join("data").join(data_name(id));
    fs::remove_file(&object).unwrap();
    symlink(outside.join("valuable"), &object).unwrap();
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    assert!(Overlay::status(&fixture.root).is_err());
    assert!(Overlay::discard(&fixture.root).is_err());
    fs::remove_file(&object).unwrap();
    fs::write(&object, b"data").unwrap();
    let state = fixture.root.join(STATE);
    let valid = fs::read(&state).unwrap();
    fs::remove_file(&state).unwrap();
    symlink(outside.join("valuable"), &state).unwrap();
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    assert!(Overlay::discard(&fixture.root).is_err());
    fs::remove_file(&state).unwrap();
    fs::write(&state, valid).unwrap();
    let data = fixture.root.join("data");
    fs::rename(&data, parent.join("saved-data")).unwrap();
    symlink(&outside, &data).unwrap();
    assert!(Overlay::open(fixture.base.clone(), &fixture.root).is_err());
    assert!(Overlay::discard(&fixture.root).is_err());
    assert_eq!(fs::read(outside.join("valuable")).unwrap(), b"never touch");
}

#[cfg(unix)]
#[test]
fn retained_directory_identity_prevents_storage_redirect_after_open() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let overlay = fixture.open();
    let parent = fixture.root.parent().unwrap();
    let outside = parent.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("valuable"), b"never touch").unwrap();
    let relocated = parent.join("relocated-overlay");
    fs::rename(&fixture.root, &relocated).unwrap();
    symlink(&outside, &fixture.root).unwrap();
    let handle = overlay.create("created", 0o644, true).unwrap();
    overlay
        .write(&handle, 0, b"inside original root", false)
        .unwrap();
    overlay.flush(&handle).unwrap();
    assert_eq!(fs::read(outside.join("valuable")).unwrap(), b"never touch");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert!(relocated.join(STATE).is_file());
    assert!(
        relocated
            .join("data")
            .join(data_name(overlay.metadata_handle(&handle).unwrap().id))
            .is_file()
    );
}
