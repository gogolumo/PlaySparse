use super::*;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{FileExt, PermissionsExt};

use playsparse_core::Chunker;
use playsparse_store::{PackOptions, Store, pack_directory};

fn fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf, Vec<u8>) {
    // macOS /var and /tmp are aliases. Physical writable storage deliberately
    // follows no symlinks, so use the real spelling of this private fixture.
    let root = root.canonicalize().unwrap();
    let source = root.join("source");
    let store = root.join("store");
    let overlay = root.join("overlay");
    fs::create_dir_all(source.join("assets/empty")).unwrap();
    let bytes = (0..262_157).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    fs::write(source.join("assets/data"), &bytes).unwrap();
    fs::write(source.join("config"), b"original\n").unwrap();
    fs::write(source.join("remove"), b"deleted by update\n").unwrap();
    pack_directory(
        &source,
        &store,
        &PackOptions {
            chunker: Chunker::Fixed,
            chunk_size: 4096,
            ..Default::default()
        },
    )
    .unwrap();
    (source, store, overlay, bytes)
}

#[test]
fn open_handles_retain_inode_and_data_across_rename_replace_unlink() {
    let temp = tempfile::tempdir().unwrap();
    let (_, store, overlay, _) = fixture(temp.path());
    let fs = WritableFs::open(&store, 8192, &overlay, None).unwrap();
    let original = fs.overlay.metadata("config").unwrap();
    let fh = fs.open_file(INodeNo(original.id), libc::O_RDWR).unwrap();
    let file = fs.file(INodeNo(original.id), fh).unwrap();
    fs.overlay
        .write(&file.handle, 0, b"modified", false)
        .unwrap();
    fs.overlay.rename("config", "renamed", true).unwrap();
    assert_eq!(fs.entry(INodeNo(original.id)).unwrap().path, "renamed");
    fs.overlay.create("config", 0o644, true).unwrap();
    fs.overlay.rename("config", "renamed", true).unwrap();
    fs.overlay.set_mode_handle(&file.handle, 0o600).unwrap();
    assert_eq!(fs.overlay.metadata("renamed").unwrap().mode, 0o644);
    assert_eq!(
        fs.overlay.metadata_handle(&file.handle).unwrap().mode,
        0o600
    );
    assert_eq!(fs.entry(INodeNo(original.id)).unwrap().id, original.id);
    assert_eq!(fs.overlay.read(&file.handle, 0, 64).unwrap(), b"modified\n");
    assert_eq!(
        fs.overlay.metadata_handle(&file.handle).unwrap().id,
        original.id
    );
    fs.overlay.unlink("renamed").unwrap();
    assert_eq!(fs.overlay.read(&file.handle, 0, 64).unwrap(), b"modified\n");
    fs.flush_file(INodeNo(original.id), fh).unwrap();
    fs.remove_handle(INodeNo(original.id), fh).unwrap();
    assert_eq!(fs.file(INodeNo(original.id), fh).err(), Some(Errno::EBADF));
}

#[test]
fn writable_namespace_rejects_escape_names_and_overlapping_storage() {
    let temp = tempfile::tempdir().unwrap();
    let (_, store, overlay, _) = fixture(temp.path());
    let mounted = temp.path().join("mounted");
    fs::create_dir(&mounted).unwrap();
    assert!(overlay_path(&store, &mounted, &overlay).is_ok());
    assert!(overlay_path(&store, &mounted, &store.join("overlay")).is_err());
    assert!(overlay_path(&store, &mounted, &mounted.join("overlay")).is_err());
    assert!(overlay_path(&store, &mounted, temp.path()).is_err());
    let backend = WritableFs::open(&store, 8192, &overlay, None).unwrap();
    for name in ["..", ".", "/absolute", "a/b", "a\\b", "a:b"] {
        assert_eq!(
            backend.child_path(INodeNo(ROOT), OsStr::new(name)),
            Err(Errno::EINVAL)
        );
    }
    assert_eq!(
        backend.child_path(INodeNo(ROOT), OsStr::new(&"x".repeat(256))),
        Err(Errno::ENAMETOOLONG)
    );
}

#[test]
fn overlay_errors_preserve_useful_posix_statuses() {
    for (kind, expected) in [
        (std::io::ErrorKind::AlreadyExists, Errno::EEXIST),
        (std::io::ErrorKind::NotADirectory, Errno::ENOTDIR),
        (std::io::ErrorKind::DirectoryNotEmpty, Errno::ENOTEMPTY),
        (std::io::ErrorKind::PermissionDenied, Errno::EACCES),
        (std::io::ErrorKind::StorageFull, Errno::ENOSPC),
    ] {
        assert_eq!(errno(Error::Io(std::io::Error::from(kind))), expected);
    }
}

/// Ordinary tests never mark this PASS without a genuine mounted filesystem.
#[test]
#[ignore = "requires /dev/fuse or approved macFUSE and mount permission"]
fn real_writable_mount_update_remount_mmap_and_immutable_base() {
    let temp = tempfile::tempdir().unwrap();
    let (source, store, overlay, mut expected) = fixture(temp.path());
    let mounted = temp.path().join("mounted");
    fs::create_dir(&mounted).unwrap();
    let base_manifest = fs::read(store.join("manifest.json")).unwrap();
    let base_index = fs::read(store.join("index/objects.idx")).unwrap();
    let mut config = crate::fuse::mount_config();
    config
        .mount_options
        .retain(|option| *option != MountOption::RO);
    config.mount_options.push(MountOption::RW);
    {
        let backend = WritableFs::open(&store, 16384, &overlay, None).unwrap();
        let session = crate::spawn_session(backend, &mounted, &config).unwrap();
        let data = OpenOptions::new()
            .read(true)
            .write(true)
            .open(mounted.join("assets/data"))
            .unwrap();
        data.write_all_at(b"patched-cross-chunk", 4091).unwrap();
        expected[4091..4110].copy_from_slice(b"patched-cross-chunk");
        data.sync_all().unwrap();
        // SAFETY: this test owns the only writer, the length stays fixed, and
        // the mount/file outlive the mapping.
        let mut shared_mapping = unsafe { memmap2::MmapMut::map_mut(&data) }.unwrap();
        shared_mapping[65536..65540].copy_from_slice(b"MMAP");
        expected[65536..65540].copy_from_slice(b"MMAP");
        shared_mapping.flush().unwrap();
        drop(shared_mapping);
        data.sync_all().unwrap();
        let mut existing_read = File::open(mounted.join("config")).unwrap();
        fs::rename(mounted.join("config"), mounted.join("config.old")).unwrap();
        fs::write(mounted.join("config"), b"replacement\n").unwrap();
        fs::rename(mounted.join("config"), mounted.join("config.old")).unwrap();
        let mut old_contents = String::new();
        existing_read.read_to_string(&mut old_contents).unwrap();
        assert_eq!(old_contents, "original\n");
        assert_eq!(
            fs::read(mounted.join("config.old")).unwrap(),
            b"replacement\n"
        );
        fs::remove_file(mounted.join("config.old")).unwrap();
        existing_read
            .set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        assert_eq!(existing_read.metadata().unwrap().len(), 9);
        assert_eq!(
            existing_read.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::create_dir(mounted.join("update")).unwrap();
        fs::write(mounted.join("update/staged"), b"v1\n").unwrap();
        let mut appended = OpenOptions::new()
            .append(true)
            .open(mounted.join("update/staged"))
            .unwrap();
        appended.write_all(b"v2\n").unwrap();
        appended.sync_all().unwrap();
        drop(appended);
        fs::rename(mounted.join("update"), mounted.join("installed")).unwrap();
        let truncated = OpenOptions::new()
            .write(true)
            .open(mounted.join("installed/staged"))
            .unwrap();
        truncated.set_len(5).unwrap();
        truncated.sync_all().unwrap();
        drop(truncated);
        assert_eq!(
            fs::read(mounted.join("installed/staged")).unwrap(),
            b"v1\nv2"
        );
        fs::set_permissions(
            mounted.join("installed/staged"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(mounted.join("installed/concurrent"), b"").unwrap();
        std::thread::scope(|scope| {
            for worker in 0..4u8 {
                let path = mounted.join("installed/concurrent");
                scope.spawn(move || {
                    let mut file = OpenOptions::new().append(true).open(path).unwrap();
                    for _ in 0..32 {
                        file.write_all(&[b'A' + worker; 4]).unwrap();
                    }
                    file.sync_all().unwrap();
                });
            }
        });
        let appended = fs::read(mounted.join("installed/concurrent")).unwrap();
        assert_eq!(appended.len(), 512);
        for record in appended.as_chunks::<4>().0 {
            assert!(record.iter().all(|byte| *byte == record[0]));
            assert!((b'A'..=b'D').contains(&record[0]));
        }
        for worker in b'A'..=b'D' {
            assert_eq!(appended.iter().filter(|byte| **byte == worker).count(), 128);
        }
        assert_eq!(
            fs::metadata(mounted.join("installed/staged"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_file(mounted.join("remove")).unwrap();
        fs::remove_dir(mounted.join("assets/empty")).unwrap();
        assert_eq!(fs::read_dir(mounted.join("installed")).unwrap().count(), 2);
        // SAFETY: no writes occur while mapped, and the mount stays alive.
        let mapping = unsafe { memmap2::Mmap::map(&data) }.unwrap();
        assert_eq!(&mapping[..], expected);
        drop(mapping);
        drop(data);
        drop(existing_read);
        // Dropping a background session starts unmounting but does not guarantee
        // the filesystem thread has released the overlay lock before the next
        // mount. Join explicitly so remount validation is deterministic.
        session.umount_and_join().unwrap();
    }
    {
        let backend = WritableFs::open(&store, 16384, &overlay, None).unwrap();
        let session = crate::spawn_session(backend, &mounted, &config).unwrap();
        assert_eq!(fs::read(mounted.join("assets/data")).unwrap(), expected);
        assert_eq!(
            fs::read(mounted.join("installed/staged")).unwrap(),
            b"v1\nv2"
        );
        assert_eq!(
            fs::metadata(mounted.join("installed/concurrent"))
                .unwrap()
                .len(),
            512
        );
        assert!(!mounted.join("remove").exists());
        assert!(!mounted.join("config").exists());
        assert!(!mounted.join("config.old").exists());
        assert!(!mounted.join("assets/empty").exists());
        // Dropping a background session starts unmounting but does not guarantee
        // the filesystem thread has released the overlay lock before the next
        // mount. Join explicitly so remount validation is deterministic.
        session.umount_and_join().unwrap();
    }
    assert_eq!(fs::read(source.join("config")).unwrap(), b"original\n");
    assert_eq!(
        fs::read(source.join("remove")).unwrap(),
        b"deleted by update\n"
    );
    let original = (0..262_157).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    assert_eq!(fs::read(source.join("assets/data")).unwrap(), original);
    assert_eq!(
        fs::read(store.join("manifest.json")).unwrap(),
        base_manifest
    );
    assert_eq!(
        fs::read(store.join("index/objects.idx")).unwrap(),
        base_index
    );
    Store::open(&store).unwrap().verify().unwrap();
    assert_eq!(fs::read_dir(&mounted).unwrap().count(), 0);
}
