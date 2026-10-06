//! Reusable measured analysis and conservative workspace budgets.
use anyhow::Result;
use playsparse_core::{Chunker, Codec, Layout};
use playsparse_store::{PackOptions, Store};
use serde_json::{Value, json};
use std::{fs, path::Path};
pub mod workspace;
pub fn analyze(source: &Path, chunk_size: u32, temp_dir: &Path) -> Result<Value> {
    analyze_observed(source, chunk_size, temp_dir, &mut |_| Ok(()))
}
pub fn analyze_observed(
    source: &Path,
    chunk_size: u32,
    temp_dir: &Path,
    observer: &mut dyn FnMut(playsparse_store::Progress) -> playsparse_core::Result<()>,
) -> Result<Value> {
    observer(playsparse_store::Progress {
        stage: "measuring_source",
        bytes: 0,
        files: 0,
    })?;
    let budget = workspace::preflight(source, temp_dir, chunk_size, Layout::Packs, 2, "temporary")?;
    fs::create_dir_all(temp_dir)?;
    let work = tempfile::Builder::new()
        .prefix("playsparse-analysis-")
        .tempdir_in(temp_dir)?;
    let fixed = playsparse_store::pack_directory_observed(
        source,
        &work.path().join("fixed"),
        &PackOptions {
            chunker: Chunker::Fixed,
            chunk_size,
            ..Default::default()
        },
        &mut |mut progress| {
            progress.stage = match progress.stage {
                "scanning" => "fixed_scanning",
                "packing" => "testing_fixed_chunks",
                "verifying" => "fixed_verifying",
                _ => "fixed_finalizing",
            };
            observer(progress)
        },
    )?;
    let cdc = playsparse_store::pack_directory_observed(
        source,
        &work.path().join("cdc"),
        &PackOptions {
            chunk_size,
            ..Default::default()
        },
        &mut |mut progress| {
            progress.stage = match progress.stage {
                "scanning" => "cdc_scanning",
                "packing" => "testing_cdc_chunks",
                "verifying" => "cdc_verifying",
                _ => "cdc_finalizing",
            };
            observer(progress)
        },
    )?;
    let store = Store::open(&work.path().join("cdc"))?;
    let mut hashes = std::collections::BTreeSet::new();
    let mut duplicates = 0u64;
    let mut cdc_reuse = 0u64;
    let mut refs = std::collections::BTreeSet::new();
    for file in &store.manifest().files {
        if !hashes.insert(&file.hash) {
            duplicates += file.size;
        }
        for chunk in &file.chunks {
            if !refs.insert(&chunk.hash) {
                cdc_reuse += chunk.raw_size as u64;
            }
        }
    }
    let compressible: u64 = store
        .index()
        .values()
        .filter(|r| r.codec == Codec::Zstd)
        .map(|r| r.raw_size as u64)
        .sum();
    let incompressible: u64 = store
        .index()
        .values()
        .filter(|r| r.codec == Codec::Raw)
        .map(|r| r.raw_size as u64)
        .sum();
    observer(playsparse_store::Progress {
        stage: "finalizing_analysis",
        bytes: cdc.logical_bytes,
        files: cdc.files,
    })?;
    Ok(
        json!({"title":"PlaySparse Analysis","temporary_workspace":budget,"measurement":"measured full scan, two temporary verified stores; no extrapolation","source_modified":false,"files":cdc.files,"logical_bytes":cdc.logical_bytes,"exact_duplicate_file_bytes":duplicates,"cdc_duplicate_reuse_bytes":cdc_reuse,"unique_compressible_raw_bytes":compressible,"unique_incompressible_raw_bytes":incompressible,"already_compressed_bytes":null,"high_entropy_bytes":null,"classification_note":"Codec choice is measured. Already-compressed and high-entropy attribution is not inferred from filename or compression ratio.","fixed_chunks":fixed,"cdc_chunks":cdc,"projected_safe_mode":{"measurement":"measured on this input","physical_bytes":cdc.physical_bytes,"metadata_bytes":cdc.metadata_bytes},"temporary_stores_removed_on_exit":true}),
    )
}
