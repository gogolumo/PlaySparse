use anyhow::{Result, bail};
use playsparse_game::{GameProfile, PackingPlan, scanner};
use serde_json::{Value, json};
use std::{path::Path, time::Instant};

/// Bound the exact bytes we publish, including pretty-printing and the trailing newline.
fn artifact_bytes(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    bounded_artifact_bytes(value, playsparse_game::MAX_ARTIFACT_BYTES as usize)
}
fn bounded_artifact_bytes(value: &impl serde::Serialize, limit: usize) -> Result<Vec<u8>> {
    use std::io::Write;
    struct BoundedBytes {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for BoundedBytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|end| end > self.limit)
            {
                return Err(std::io::Error::other(
                    "game artifact exceeds its serialized byte limit",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedBytes {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n")?;
    Ok(output.bytes)
}
fn write_artifact_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Refuse sidecars inside source before creating their parents (including symlink ancestors).
fn outside_source(output: &Path, source: &Path) -> Result<()> {
    let source = source.canonicalize()?;
    let absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    let mut existing = absolute.as_path();
    let mut tail = Vec::new();
    while !existing.exists() {
        tail.push(
            existing
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid output ancestor"))?,
        );
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid output parent"))?;
    }
    let mut projected = existing.canonicalize()?;
    for part in tail.into_iter().rev() {
        projected.push(part);
    }
    // Components with .. can otherwise conceal the projected destination.
    if absolute
        .components()
        .any(|p| matches!(p, std::path::Component::ParentDir))
        || projected.starts_with(source)
    {
        bail!("profile/plan output must be outside source, with no parent traversal");
    }
    Ok(())
}
pub fn inspect(
    source: &Path,
    output: Option<&Path>,
    plan_output: Option<&Path>,
    container_aware: bool,
    skip_compression: bool,
    mode: scanner::Mode,
    program: &Path,
) -> Result<Value> {
    if let Some(output) = output {
        outside_source(output, source)?;
    }
    if let Some(output) = plan_output {
        outside_source(output, source)?;
    }
    let start = Instant::now();
    let cpu = playsparse_core::process_resources().0;
    let mut profile = playsparse_game::inspect(source)?;
    scanner::apply(&mut profile, &source.canonicalize()?, mode, program)?;
    if mode != scanner::Mode::Generic {
        profile.matches_source(&playsparse_game::inspect(source)?)?;
    }
    // Validate both complete sidecars before creating either output file. The
    // compact inventory budget does not include pretty-printing or plan fields.
    let profile_bytes = output.map(|_| artifact_bytes(&profile)).transpose()?;
    let plan_bytes = plan_output
        .map(|_| {
            artifact_bytes(&PackingPlan::experimental(
                &profile,
                container_aware,
                skip_compression,
            )?)
        })
        .transpose()?;
    if let (Some(output), Some(bytes)) = (output, profile_bytes) {
        write_artifact_exclusive(output, &bytes)?;
    }
    if let (Some(output), Some(bytes)) = (plan_output, plan_bytes) {
        write_artifact_exclusive(output, &bytes)?;
    }
    Ok(
        json!({"profile":profile,"measurement_wall_seconds":start.elapsed().as_secs_f64(),"measurement_cpu_seconds":playsparse_core::process_resources().0.zip(cpu).map(|(end,start)|end-start),"measurement_note":"full identity hash and at most three 64 KiB Zstd level 3 probes per file; scanner hints are untrusted; no already-compressed byte attribution","source_unchanged":true,"startup":"NOT RUN"}),
    )
}
pub fn analyze(
    source: &Path,
    profile_path: &Path,
    output: Option<&Path>,
    container_aware: bool,
    skip_compression: bool,
    temp_dir: &Path,
) -> Result<Value> {
    if let Some(output) = output {
        outside_source(output, source)?;
    }
    let imported = GameProfile::load(profile_path)?;
    let start = Instant::now();
    let cpu = playsparse_core::process_resources().0;
    let measured = playsparse_game::inspect(source)?;
    imported.matches_source(&measured)?;
    let plan = PackingPlan::experimental(&measured, container_aware, skip_compression)?;
    // Publish a separate versioned artifact before the pack transaction begins.
    if let Some(output) = output {
        write_artifact_exclusive(output, &artifact_bytes(&plan)?)?;
    }
    super::workspace::preflight(
        source,
        temp_dir,
        playsparse_game::TARGET_BYTES,
        playsparse_core::Layout::Packs,
        1,
        "temporary",
    )?;
    std::fs::create_dir_all(temp_dir)?;
    let work = tempfile::Builder::new()
        .prefix("playsparse-profile-analysis-")
        .tempdir_in(temp_dir)?;
    let candidate = playsparse_store::pack_directory_with_plan(
        source,
        &work.path().join("candidate"),
        &Default::default(),
        &plan,
    )?;
    let mut classes = std::collections::BTreeMap::new();
    for f in &measured.files {
        let key = serde_json::to_value(f.class)?
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        *classes.entry(key).or_insert(0u64) += f.size;
    }
    let raw_candidate: u64 = measured
        .files
        .iter()
        .filter(|f| f.measurement.incompressible_candidate)
        .map(|f| f.size)
        .sum();
    imported.matches_source(&playsparse_game::inspect(source)?)?;
    Ok(
        json!({"experimental":true,"engine":imported.engine,"game":imported.game,"scanner_status":imported.scanner_status,"anti_cheat_detected":imported.anti_cheat_detected,"file_class_bytes":classes,"measured_incompressible_candidate_file_bytes":raw_candidate,"candidate_note":"candidate file bytes include unsampled regions; not proven already-compressed bytes","already_compressed_bytes":null,"candidate":candidate,"candidate_analysis_wall_seconds":start.elapsed().as_secs_f64(),"candidate_analysis_cpu_seconds":playsparse_core::process_resources().0.zip(cpu).map(|(end,start)|end-start),"plan":plan,"runtime_policy":"unchanged-static-default","source_unchanged":true,"temporary_store_removed_on_exit":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn artifact_limit_includes_exact_pretty_bytes_and_newline() {
        let value = json!({"x": "é"});
        let mut expected = serde_json::to_vec_pretty(&value).unwrap();
        expected.push(b'\n');
        assert_eq!(
            bounded_artifact_bytes(&value, expected.len()).unwrap(),
            expected
        );
        assert!(bounded_artifact_bytes(&value, expected.len() - 1).is_err());
        assert!(bounded_artifact_bytes(&value, expected.len() - 2).is_err());
    }
    #[test]
    fn refuse_nested_output_without_mutating_source() {
        let t = tempfile::tempdir().unwrap();
        let source = t.path().join("source");
        fs::create_dir(&source).unwrap();
        assert!(outside_source(&source.join("new/profile.json"), &source).is_err());
        assert!(!source.join("new").exists());
        assert!(outside_source(&t.path().join("profile.json"), &source).is_ok());
    }
}
