use super::*;
use playsparse_game::{PackingPlan, inspect};

fn random(n: usize) -> Vec<u8> {
    let mut s = 0x123456789abcdefu64;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect()
}
fn check(root: &Path, source: &Path) {
    let store = Store::open(root).unwrap();
    assert!(store.verify().is_ok());
    assert_eq!(store.manifest().version, FORMAT_VERSION);
    for file in &store.manifest().files {
        let mut actual = Vec::new();
        for chunk in &file.chunks {
            actual.extend_from_slice(
                &store
                    .read_object(&parse_hash(&chunk.hash).unwrap())
                    .unwrap(),
            );
        }
        assert_eq!(actual, fs::read(source.join(&file.path)).unwrap());
    }
}
#[test]
fn measured_codec_byte_identity_and_generic_regression() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("raw.pak"), random(2000000)).unwrap();
    fs::write(source.join("code.lua"), b"print('hello')\n".repeat(60000)).unwrap();
    let before = inspect(&source).unwrap();
    let plan = PackingPlan::from_profile(&before, false).unwrap();
    for layout in [Layout::Packs, Layout::Loose] {
        let a = t.path().join(format!("{layout:?}-generic"));
        let b = t.path().join(format!("{layout:?}-aware"));
        let options = PackOptions {
            layout,
            ..Default::default()
        };
        let generic = pack_directory(&source, &a, &options).unwrap();
        let aware = pack_directory_with_plan(&source, &b, &options, &plan).unwrap();
        check(&a, &source);
        check(&b, &source);
        assert_eq!(generic.object_bytes, aware.object_bytes);
        assert_eq!(
            fs::read(a.join("manifest.json")).unwrap(),
            fs::read(b.join("manifest.json")).unwrap()
        );
        assert_eq!(
            fs::read(a.join("index/objects.idx")).unwrap(),
            fs::read(b.join("index/objects.idx")).unwrap()
        );
        assert!(aware.measured_raw_objects > 0);
        assert!(aware.zstd_attempted_objects > 0);
        assert_eq!(generic.measured_raw_objects, 0);
    }
    assert_eq!(before, inspect(&source).unwrap());
}
#[test]
fn forged_plan_refused_before_creating_destination_parent() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data.bin"), b"text".repeat(100000)).unwrap();
    let before = inspect(&source).unwrap();
    let mut plan = PackingPlan::from_profile(&before, false).unwrap();
    plan.files[0].compression_strategy = playsparse_game::CompressionStrategy::MeasuredRaw;
    let dest = t.path().join("new/store");
    assert!(pack_directory_with_plan(&source, &dest, &Default::default(), &plan).is_err());
    assert!(!t.path().join("new").exists());
    assert_eq!(before, inspect(&source).unwrap());
}
