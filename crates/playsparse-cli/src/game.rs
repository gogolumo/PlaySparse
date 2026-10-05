use anyhow::{Result, bail};
use playsparse_game::{GameProfile, PackingPlan, scanner};
use serde_json::{Value, json};
use std::{path::Path, time::Instant};

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
    if let Some(output) = output {
        super::write_json_exclusive(output, &serde_json::to_value(&profile)?)?;
    }
    if let Some(output) = plan_output {
        let plan = PackingPlan::from_profile(&profile, container_aware)?;
        super::write_json_exclusive(output, &serde_json::to_value(&plan)?)?;
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
) -> Result<Value> {
    if let Some(output) = output {
        outside_source(output, source)?;
    }
    let imported = GameProfile::load(profile_path)?;
    let start = Instant::now();
    let cpu = playsparse_core::process_resources().0;
    let measured = playsparse_game::inspect(source)?;
    imported.matches_source(&measured)?;
    let plan = PackingPlan::from_profile(&measured, container_aware)?;
    // Publish a separate versioned artifact before the pack transaction begins.
    if let Some(output) = output {
        super::write_json_exclusive(output, &serde_json::to_value(&plan)?)?;
    }
    let work = tempfile::tempdir()?;
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
    fn refuse_nested_output_without_mutating_source() {
        let t = tempfile::tempdir().unwrap();
        let source = t.path().join("source");
        fs::create_dir(&source).unwrap();
        assert!(outside_source(&source.join("new/profile.json"), &source).is_err());
        assert!(!source.join("new").exists());
        assert!(outside_source(&t.path().join("profile.json"), &source).is_ok());
    }
}
