use super::*;
use std::fs;
#[cfg(unix)]
use std::io::Write;

fn random_bytes(n: usize) -> Vec<u8> {
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
fn source() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    fs::write(t.path().join("raw.pak"), random_bytes(800000)).unwrap();
    fs::write(
        t.path().join("text.dat"),
        b"repeatable text\n".repeat(60000),
    )
    .unwrap();
    t
}
#[test]
fn deterministic_measurement_and_plan() {
    let t = source();
    let a = inspect(t.path()).unwrap();
    let b = inspect(t.path()).unwrap();
    assert_eq!(a, b);
    assert!(a.files[0].measurement.incompressible_candidate);
    assert!(!a.files[1].measurement.incompressible_candidate);
    assert_eq!(a.files[0].measurement.samples.len(), 3);
    assert_eq!(a.files[0].container_hint.as_deref(), Some("ambiguous-pak"));
    let p = PackingPlan::experimental(&a, false, true).unwrap();
    p.verify_source(t.path()).unwrap();
    assert_eq!(
        p.files[0].compression_strategy,
        CompressionStrategy::MeasuredRaw
    );
    assert_eq!(
        p.files[1].compression_strategy,
        CompressionStrategy::TryZstd
    );
    let default = PackingPlan::from_profile(&a, false).unwrap();
    assert!(
        default
            .files
            .iter()
            .all(|f| f.compression_strategy == CompressionStrategy::TryZstd)
    );
}
#[test]
fn artifact_validation_and_source_mismatch() {
    let t = source();
    let mut p = inspect(t.path()).unwrap();
    let original = p.clone();
    p.schema_version = 99;
    assert!(p.validate().is_err());
    p = original.clone();
    p.files[0].path = "../escape".into();
    assert!(p.validate().is_err());
    p = original.clone();
    p.engine.evidence_paths.push("/Users/private/source".into());
    assert!(p.validate().is_err());
    p = original.clone();
    let mut v = serde_json::to_value(&p).unwrap();
    v["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<GameProfile>(v).is_err());
    fs::write(t.path().join("raw.pak"), random_bytes(799999)).unwrap();
    let now = inspect(t.path()).unwrap();
    assert!(p.matches_source(&now).is_err());
    let plan = PackingPlan::from_profile(&p, false).unwrap();
    assert!(plan.verify_source(t.path()).is_err());
}
#[test]
fn forged_decision_is_remeasured() {
    let t = source();
    let p = inspect(t.path()).unwrap();
    let mut plan = PackingPlan::from_profile(&p, false).unwrap();
    plan.files[1].compression_strategy = CompressionStrategy::MeasuredRaw;
    assert!(plan.verify_source(t.path()).is_err());
    plan.schema_version = 5;
    assert!(plan.validate().is_err());
}
#[test]
fn scanner_unknown_and_multiple_signals_no_path_leak() {
    let t = source();
    let mut p = inspect(t.path()).unwrap();
    let input = serde_json::json!({"path":t.path(),"name":"Fixture","store":null,"appid":null,"engine":{"key":"unknown-future-engine","confidence":10,"evidence":["raw.pak","/Users/private/game"]},"other_engine_signals":[{"key":"unreal","evidence":["raw.pak"]},{"key":"electron","evidence":[]}],"saves":["/Users/private/save"],"anti_cheat":["EasyAntiCheat"],"routes":["never persisted"]});
    scanner::normalize(&serde_json::to_vec(&input).unwrap(), t.path(), &mut p).unwrap();
    assert_eq!(p.engine.key, "unknown-future-engine");
    assert_eq!(p.engine.other_signals.len(), 2);
    assert_eq!(p.engine.evidence_paths, vec!["raw.pak"]);
    assert!(!serde_json::to_string(&p).unwrap().contains("/Users/"));
    assert_eq!(
        PackingPlan::experimental(&p, false, true).unwrap().files[0].compression_strategy,
        CompressionStrategy::MeasuredRaw
    );
}
#[test]
fn scanner_malformed_oversized_and_wrong_source() {
    let t = source();
    let mut p = inspect(t.path()).unwrap();
    assert!(scanner::normalize(b"not json", t.path(), &mut p).is_err());
    assert!(scanner::normalize(&vec![b' '; 1024 * 1024 + 1], t.path(), &mut p).is_err());
    let q = tempfile::tempdir().unwrap();
    let input = serde_json::json!({"path":q.path(),"engine":{"key":"native","confidence":10}});
    assert!(scanner::normalize(&serde_json::to_vec(&input).unwrap(), t.path(), &mut p).is_err());
}
#[test]
fn absent_scanner_falls_back_without_changing_plan() {
    let t = source();
    let mut p = inspect(t.path()).unwrap();
    let plan = PackingPlan::from_profile(&p, false).unwrap();
    let missing = t.path().join("no-such-scanner");
    scanner::apply(&mut p, t.path(), scanner::Mode::Auto, &missing).unwrap();
    assert_eq!(p.scanner_status, "absent-fallback");
    assert_eq!(PackingPlan::from_profile(&p, false).unwrap(), plan);
    assert!(scanner::apply(&mut p, t.path(), scanner::Mode::UniversalModder, &missing).is_err());
}
#[test]
fn bounded_artifact_read() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("huge.json");
    let f = File::create(&path).unwrap();
    f.set_len(MAX_ARTIFACT_BYTES + 1).unwrap();
    assert!(GameProfile::load(&path).is_err());
}
#[test]
fn extension_is_not_compression_proof() {
    let t = tempfile::tempdir().unwrap();
    fs::write(t.path().join("archive.pak"), b"A".repeat(800000)).unwrap();
    let p = inspect(t.path()).unwrap();
    assert!(!p.files[0].measurement.incompressible_candidate);
}
pub(crate) fn zip_fixture() -> Vec<u8> {
    let name = b"data.bin";
    let raw = b"hello original bytes";
    let mut b = Vec::new();
    b.extend_from_slice(b"PK\x03\x04");
    b.extend_from_slice(&20u16.to_le_bytes());
    b.extend_from_slice(&[0; 8]);
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    b.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    b.extend_from_slice(&(name.len() as u16).to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(name);
    b.extend_from_slice(raw);
    let cd = b.len() as u32;
    b.extend_from_slice(b"PK\x01\x02");
    b.extend_from_slice(&20u16.to_le_bytes());
    b.extend_from_slice(&20u16.to_le_bytes());
    b.extend_from_slice(&[0; 8]);
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    b.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    b.extend_from_slice(&(name.len() as u16).to_le_bytes());
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(name);
    let cd_size = b.len() as u32 - cd;
    b.extend_from_slice(b"PK\x05\x06");
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&cd_size.to_le_bytes());
    b.extend_from_slice(&cd.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b
}
#[test]
fn zip_valid_boundaries_and_malformed_count_offset_flags() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("data.pk3");
    let b = zip_fixture();
    fs::write(&path, &b).unwrap();
    let bounds = zip::boundaries(&mut File::open(&path).unwrap()).unwrap();
    assert_eq!(bounds.len(), 2);
    assert_eq!(bounds[1], b.len() as u64);
    for (offset, bytes) in [
        (b.len() - 22 + 10, 65535u16.to_le_bytes().to_vec()),
        (b.len() - 22 + 16, u32::MAX.to_le_bytes().to_vec()),
        (6, 8u16.to_le_bytes().to_vec()),
    ] {
        let mut malformed = b.clone();
        malformed[offset..offset + bytes.len()].copy_from_slice(&bytes);
        fs::write(&path, malformed).unwrap();
        assert!(zip::boundaries(&mut File::open(&path).unwrap()).is_err());
    }
    fs::write(&path, &b[..b.len() - 1]).unwrap();
    assert!(zip::boundaries(&mut File::open(&path).unwrap()).is_err());
    // Every truncation fails boundedly, without allocation from an untrusted entry count.
    for n in 0..b.len() {
        fs::write(&path, &b[..n]).unwrap();
        assert!(zip::boundaries(&mut File::open(&path).unwrap()).is_err());
    }
}
#[cfg(unix)]
fn script(dir: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("scanner");
    let mut f = File::create(&p).unwrap();
    writeln!(f, "#!/bin/sh\n{body}").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
    p
}
#[cfg(unix)]
#[test]
fn scanner_deadline_and_bounded_streams() {
    let t = tempfile::tempdir().unwrap();
    let p = script(t.path(), "sleep 5");
    let now = std::time::Instant::now();
    assert!(scanner::run(&p, t.path(), std::time::Duration::from_millis(100)).is_err());
    assert!(now.elapsed() < std::time::Duration::from_secs(2));
    let p = script(t.path(), "head -c 1100000 /dev/zero");
    assert!(
        scanner::run(&p, t.path(), std::time::Duration::from_secs(5))
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
    let p = script(t.path(), "head -c 1100000 /dev/zero >&2");
    assert!(
        scanner::run(&p, t.path(), std::time::Duration::from_secs(5))
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
}
#[cfg(unix)]
#[test]
fn scanner_literal_argv() {
    let t = tempfile::tempdir().unwrap();
    let p = script(t.path(), "printf '%s\\n' \"$@\"");
    let source = Path::new("quoted ' ; $(touch nope) Unicode игра");
    let result = scanner::run(&p, source, std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(
        String::from_utf8(result).unwrap(),
        format!("scan\n{}\n--json\n", source.display())
    );
    assert!(!t.path().join("nope").exists());
}
