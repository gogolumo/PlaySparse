use super::*;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::process::Command;

use playsparse_core::{Chunker, Layout};
use playsparse_store::{PackOptions, pack_directory};

fn pattern(offset: usize) -> u8 {
    ((offset * 31 + offset / 4096) % 251) as u8
}

#[test]
fn namespace_preserves_empty_directories_and_large_sizes() {
    let namespace = Namespace::from_entries(
        ["assets", "assets/empty", "世界"].into_iter(),
        [
            ("game", (12u64 << 30) + 17, 0o755),
            ("世界/config", 7, 0o644),
        ]
        .into_iter(),
        1000,
        1000,
    )
    .unwrap();
    let root = namespace.directory(INodeNo(ROOT)).unwrap();
    assert_eq!(root.attr.nlink, 4);
    let assets = namespace
        .lookup(INodeNo(ROOT), OsStr::new("assets"))
        .unwrap();
    assert_eq!(assets.attr.nlink, 3);
    let empty = namespace
        .lookup(assets.attr.ino, OsStr::new("empty"))
        .unwrap();
    assert!(empty.children.is_empty());
    assert_eq!(
        namespace
            .lookup(empty.attr.ino, OsStr::new(".."))
            .unwrap()
            .attr
            .ino,
        assets.attr.ino
    );
    let game = namespace.lookup(INodeNo(ROOT), OsStr::new("game")).unwrap();
    assert_eq!(game.attr.size, (12u64 << 30) + 17);
    assert_eq!(game.attr.perm, 0o555);
    assert_eq!(
        namespace.directory(game.attr.ino).unwrap_err(),
        Errno::ENOTDIR
    );
    assert_eq!(
        namespace
            .lookup(INodeNo(ROOT), OsStr::new("absent"))
            .unwrap_err(),
        Errno::ENOENT
    );
}

#[test]
fn namespace_rejects_path_collisions_and_unsupported_names() {
    assert!(
        Namespace::from_entries(["file"].into_iter(), [("file", 0, 0o644)].into_iter(), 0, 0)
            .is_err()
    );
    assert!(
        Namespace::from_entries(
            [].into_iter(),
            [("missing/file", 0, 0o644)].into_iter(),
            0,
            0
        )
        .is_err()
    );
    assert!(
        Namespace::from_entries([].into_iter(), [("../file", 0, 0o644)].into_iter(), 0, 0).is_err()
    );
    let long = "x".repeat(256);
    assert!(
        Namespace::from_entries(
            [].into_iter(),
            [(long.as_str(), 0, 0o644)].into_iter(),
            0,
            0
        )
        .is_err()
    );
}

#[test]
fn rejects_nonempty_and_overlapping_mountpoints() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    let mountpoint = temp.path().join("mount");
    fs::create_dir(&store).unwrap();
    fs::create_dir(&mountpoint).unwrap();
    assert!(validate_mountpoint(&store, &mountpoint).is_ok());
    fs::write(mountpoint.join("important"), b"unchanged").unwrap();
    assert!(validate_mountpoint(&store, &mountpoint).is_err());
    assert_eq!(
        fs::read(mountpoint.join("important")).unwrap(),
        b"unchanged"
    );
    assert!(validate_mountpoint(&store, temp.path()).is_err());
    assert!(validate_mountpoint(&store, &store).is_err());
}

#[test]
fn callbacks_resolve_only_requested_chunks_and_report_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let bytes: Vec<_> = (0..65549).map(pattern).collect();
    fs::write(source.join("data"), &bytes).unwrap();
    let store = temp.path().join("store");
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
    let backend = StoreFs::open(&store, 8192).unwrap();
    let ino = backend
        .namespace
        .lookup(INodeNo(ROOT), OsStr::new("data"))
        .unwrap()
        .attr
        .ino;
    assert_eq!(
        backend
            .open_file(ino, OpenFlags(libc::O_WRONLY))
            .unwrap_err(),
        Errno::EROFS
    );
    assert_eq!(
        backend
            .open_file(ino, OpenFlags(libc::O_RDONLY | libc::O_TRUNC))
            .unwrap_err(),
        Errno::EROFS
    );
    assert_eq!(
        backend.read_file(ino, FileHandle(ino.0), 4095, 2).unwrap(),
        &bytes[4095..4097]
    );
    assert_eq!(backend.resolver.metrics().decompressions, 2);
    assert!(
        backend
            .read_file(ino, FileHandle(ino.0), u64::MAX, 7)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        backend.read_file(ino, FileHandle(0), 0, 1).unwrap_err(),
        Errno::EBADF
    );
    assert!(backend.resolver.metrics().resident_bytes <= 8192);
    let chunk = &backend.resolver.manifest().file("data").unwrap().chunks[10];
    let record = backend
        .resolver
        .store()
        .lookup(&playsparse_core::parse_hash(&chunk.hash).unwrap())
        .unwrap();
    let pack = OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            store
                .join("packs")
                .join(format!("pack-{:04}.psp", record.pack_id)),
        )
        .unwrap();
    let mut byte = [0u8];
    pack.read_at(&mut byte, record.offset).unwrap();
    byte[0] ^= 0x80;
    pack.write_at(&byte, record.offset).unwrap();
    assert_eq!(
        backend
            .read_file(ino, FileHandle(ino.0), chunk.offset, 1)
            .unwrap_err(),
        Errno::EIO
    );
}

/// This test is intentionally ignored by ordinary CI; running it without a
/// real FUSE device is an error, never a fake successful mount.
#[test]
#[ignore = "requires real /dev/fuse or installed macFUSE, mount permission, and cc"]
fn real_mount_random_concurrent_mmap_and_native_execution() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let store = temp.path().join("store");
    let mountpoint = temp.path().join("mounted");
    fs::create_dir_all(source.join("assets/empty")).unwrap();
    fs::create_dir(&mountpoint).unwrap();
    let bytes: Arc<Vec<u8>> = Arc::new((0..2 * 1024 * 1024 + 13).map(pattern).collect());
    fs::write(source.join("assets/data"), bytes.as_slice()).unwrap();
    fs::write(source.join("config"), b"playsparse-test-v1\n").unwrap();
    fs::write(source.join("zero"), b"").unwrap();
    // Force multiple directory replies, testing continuation cookies.
    fs::create_dir(source.join("many")).unwrap();
    for index in 0..1200 {
        fs::write(source.join(format!("many/{index:04}")), b"").unwrap();
    }
    let c_path = temp.path().join("testgame.c");
    fs::write(&c_path, NATIVE_TEST_PROGRAM).unwrap();
    let executable = source.join("testgame");
    let output = Command::new("cc")
        .arg("-O2")
        .arg(&c_path)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("cc is required for the native executable test");
    assert!(
        output.status.success(),
        "cc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original_executable = fs::read(&executable).unwrap();
    let original_mode = fs::metadata(&executable).unwrap().permissions().mode();
    for layout in [Layout::Packs, Layout::Loose] {
        let store = store.with_extension(if layout == Layout::Packs {
            "packs"
        } else {
            "loose"
        });
        let stats = pack_directory(
            &source,
            &store,
            &PackOptions {
                layout,
                chunker: Chunker::Fixed,
                chunk_size: 64 * 1024,
                ..Default::default()
            },
        )
        .unwrap();
        let backend = StoreFs::open(&store, 512 * 1024).unwrap();
        let resolver = backend.resolver.clone();
        let session = fuser::spawn_mount(backend, &mountpoint, &mount_config())
            .expect("real FUSE mount must succeed");
        // FUSE initialization completes on the serving thread; the first open
        // waits for its response and therefore also verifies initialization.
        let file = Arc::new(File::open(mountpoint.join("assets/data")).unwrap());
        assert_eq!(file.metadata().unwrap().len(), bytes.len() as u64);
        let mut first = [0u8; 7];
        assert_eq!(file.read_at(&mut first, 65535).unwrap(), first.len());
        assert_eq!(first, bytes[65535..65542]);
        assert!(
            resolver.metrics().raw_bytes_loaded < bytes.len() as u64,
            "a seven-byte mounted read must not decompress the full file"
        );
        assert_eq!(fs::read_dir(&mountpoint).unwrap().count(), 5);
        // Start concurrent IO before the sequential read populates every
        // kernel page. Each thread faults a different page of one cold chunk.
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            for thread in 0..16 {
                let start = &start;
                let file = file.clone();
                let bytes = bytes.clone();
                scope.spawn(move || {
                    start.wait();
                    let offset = 24 * 65536 + thread * 4096;
                    let mut got = [0u8; 128];
                    assert_eq!(file.read_at(&mut got, offset as u64).unwrap(), got.len());
                    assert_eq!(got, bytes[offset..offset + got.len()]);
                });
            }
        });
        let mut sequential = Vec::new();
        (&*file).read_to_end(&mut sequential).unwrap();
        assert_eq!(sequential, *bytes);
        assert_eq!(
            fs::read_dir(mountpoint.join("assets/empty"))
                .unwrap()
                .count(),
            0
        );
        let names: std::collections::BTreeSet<_> = fs::read_dir(mountpoint.join("many"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1200);
        assert_eq!(fs::read(mountpoint.join("zero")).unwrap(), b"");
        for (offset, len) in [
            (0, 7),
            (65535, 3),
            (65536, 65537),
            (bytes.len() - 3, 9),
            (bytes.len(), 9),
            (bytes.len() + 9, 9),
        ] {
            let mut got = vec![0u8; len];
            let count = file.read_at(&mut got, offset as u64).unwrap();
            let start = offset.min(bytes.len());
            assert_eq!(&got[..count], &bytes[start..(start + len).min(bytes.len())]);
        }
        for threads in [1, 2, 4, 8, 16] {
            std::thread::scope(|scope| {
                for thread in 0..threads {
                    let file = file.clone();
                    let bytes = bytes.clone();
                    scope.spawn(move || {
                        let mut seed = thread as u64 + 23;
                        for _ in 0..128 {
                            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                            let offset = (seed as usize) % bytes.len();
                            let mut got = [0u8; 4096];
                            let count = file.read_at(&mut got, offset as u64).unwrap();
                            assert_eq!(
                                &got[..count],
                                &bytes[offset..(offset + got.len()).min(bytes.len())]
                            );
                        }
                    });
                }
            });
        }
        // SAFETY: the store is immutable throughout this mapping; the backing
        // mount/session stays alive until the mapping is dropped.
        let mapping = unsafe { memmap2::Mmap::map(&*file) }.unwrap();
        for offset in (0..bytes.len()).step_by(4096) {
            assert_eq!(mapping[offset], bytes[offset]);
        }
        assert_eq!(&mapping[65535..65539], &bytes[65535..65539]);
        let output = Command::new(mountpoint.join("testgame"))
            .current_dir(&mountpoint)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "native program failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "MOUNTED_EXECUTABLE_OK"
        );
        assert_eq!(
            fs::read(mountpoint.join("testgame")).unwrap(),
            original_executable
        );
        assert_eq!(
            fs::metadata(mountpoint.join("testgame"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            original_mode & 0o555
        );
        assert_eq!(
            OpenOptions::new()
                .write(true)
                .open(mountpoint.join("config"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
        assert_eq!(
            fs::write(mountpoint.join("created"), b"no")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
        drop(mapping);
        drop(file);
        session.umount_and_join().unwrap();
        assert_eq!(fs::read_dir(&mountpoint).unwrap().count(), 0);
        assert_eq!(fs::read(source.join("assets/data")).unwrap(), *bytes);
        assert_eq!(fs::read(&executable).unwrap(), original_executable);
        assert_eq!(
            fs::metadata(&executable).unwrap().permissions().mode(),
            original_mode
        );
        println!(
            "REAL_FUSE_TEST_OK layout={layout:?} logical_bytes={} physical_bytes={} random_threads=1,2,4,8,16 mmap=true native_exec=true original_unchanged=true",
            stats.logical_bytes, stats.physical_bytes
        );
    }
}

const NATIVE_TEST_PROGRAM: &str = r#"
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
int main(void) {
    char config[32] = {0};
    FILE *c = fopen("config", "rb");
    if (!c || fread(config, 1, sizeof(config)-1, c) != 19) return 1;
    fclose(c);
    if (strcmp(config, "playsparse-test-v1\n")) return 2;
    int fd = open("assets/data", O_RDONLY);
    struct stat st;
    if (fd < 0 || fstat(fd, &st) || st.st_size != 2*1024*1024+13) return 3;
    const unsigned char *p = mmap(NULL, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
    if (p == MAP_FAILED) return 4;
    for (size_t i = 0; i < (size_t)st.st_size; i += 4096)
        if (p[i] != (i*31+i/4096)%251) return 5;
    unsigned char buf[8192];
    for (size_t i = 65535; i < (size_t)st.st_size-sizeof(buf); i += 131071) {
        if (pread(fd, buf, sizeof(buf), i) != sizeof(buf)) return 6;
        for (size_t j = 0; j < sizeof(buf); j++)
            if (buf[j] != ((i+j)*31+(i+j)/4096)%251) return 7;
    }
    if (munmap((void*)p, st.st_size) || close(fd)) return 8;
    puts("MOUNTED_EXECUTABLE_OK");
    return 0;
}
"#;
