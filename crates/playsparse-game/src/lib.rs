//! Offline experimental analysis. No dependency on Python or a game runtime.
use playsparse_core::{Error, Result, parse_hash, valid_path};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub mod scanner;
pub mod zip;

pub const VERSION: u32 = 1;
pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_FILES: usize = 8192;
pub const SAMPLE_BYTES: usize = 64 * 1024;
pub const TARGET_BYTES: u32 = 256 * 1024;

pub(crate) fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub algorithm: String,
    pub digest: String,
    pub directories: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub name: Option<String>,
    pub store: Option<String>,
    pub appid: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EngineSignal {
    pub key: String,
    pub confidence: Option<u8>,
    pub evidence_paths: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Engine {
    pub key: String,
    pub label: String,
    pub confidence: Option<u8>,
    pub version: Option<String>,
    pub evidence_paths: Vec<String>,
    pub other_signals: Vec<EngineSignal>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Executable {
    pub path: String,
    pub managed: bool,
    pub architecture: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FileClass {
    Code,
    ScriptsText,
    Metadata,
    Texture,
    Audio,
    Video,
    ShaderCache,
    Binary,
    Container,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub offset: u64,
    pub bytes: u32,
    pub blake3: String,
    pub zstd_bytes: u32,
    pub entropy_millibits_per_byte: u32,
    pub cdc_chunks: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    pub samples: Vec<Sample>,
    pub sampled_bytes: u64,
    pub sampled_zstd_bytes: u64,
    pub repeated_sample_blocks: u32,
    pub incompressible_candidate: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzedFile {
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub blake3: String,
    pub class: FileClass,
    pub container_hint: Option<String>,
    pub signature: Option<String>,
    pub measurement: Measurement,
    /// End offsets of validated original ZIP local records and the metadata tail.
    pub zip_boundaries: Option<Vec<u64>>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hints {
    pub startup_regions: Vec<String>,
    pub runtime_policy: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GameProfile {
    pub schema_version: u32,
    pub generated_by: String,
    /// None deliberately: content-derived artifacts are reproducible, timings are report-only.
    pub generated_at: Option<String>,
    pub scanner_status: String,
    pub scanner_truncated: bool,
    pub game: Game,
    pub engine: Engine,
    pub executables: Vec<Executable>,
    pub anti_cheat_detected: Vec<String>,
    pub source_identity: SourceIdentity,
    pub files: Vec<AnalyzedFile>,
    pub hints: Hints,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChunkStrategy {
    Cdc,
    ZipRecordsCdc,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CompressionStrategy {
    TryZstd,
    MeasuredRaw,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlannedFile {
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub blake3: String,
    pub class: FileClass,
    pub container_hint: Option<String>,
    pub measurement: Measurement,
    pub chunk_strategy: ChunkStrategy,
    pub target_chunk_bytes: u32,
    pub zstd_level: i32,
    pub compression_strategy: CompressionStrategy,
    pub boundaries: Vec<u64>,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackingPlan {
    pub schema_version: u32,
    pub generated_by: String,
    pub source_identity: SourceIdentity,
    pub container_aware: bool,
    pub experimental_skip_compression: bool,
    pub files: Vec<PlannedFile>,
}

fn text(s: &str, limit: usize) -> bool {
    !s.is_empty()
        && s.len() <= limit
        && !s.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
}
fn key(s: &str) -> bool {
    s.len() <= 64 && !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
fn paths(paths: &[String], limit: usize) -> Result<()> {
    if paths.len() > limit || paths.iter().any(|p| p.len() > 1024 || !valid_path(p)) {
        return Err(invalid("unsafe or excessive artifact paths"));
    }
    Ok(())
}
fn identity(identity: &SourceIdentity) -> Result<()> {
    if identity.algorithm != "playsparse-source-blake3-v1" {
        return Err(invalid("unknown source identity algorithm"));
    }
    parse_hash(&identity.digest)?;
    paths(&identity.directories, MAX_FILES)?;
    if identity.directories.windows(2).any(|p| p[0] >= p[1]) {
        return Err(invalid("unsorted directories"));
    }
    Ok(())
}
fn measurement(m: &Measurement, size: u64) -> Result<()> {
    if m.samples.len() > 3 {
        return Err(invalid("too many probes"));
    }
    let mut bytes = 0u64;
    let mut compressed = 0u64;
    let mut last = None;
    for s in &m.samples {
        parse_hash(&s.blake3)?;
        if s.bytes == 0
            || s.bytes as usize > SAMPLE_BYTES
            || s.zstd_bytes > (SAMPLE_BYTES * 2) as u32
            || s.entropy_millibits_per_byte > 8000
            || s.cdc_chunks > 16
            || s.offset
                .checked_add(s.bytes as u64)
                .is_none_or(|end| end > size)
            || last.is_some_and(|off| s.offset <= off)
        {
            return Err(invalid("invalid probe bounds"));
        }
        last = Some(s.offset);
        bytes += s.bytes as u64;
        compressed += s.zstd_bytes as u64;
    }
    if bytes != m.sampled_bytes
        || compressed != m.sampled_zstd_bytes
        || m.repeated_sample_blocks > 2
    {
        return Err(invalid("invalid probe accounting"));
    }
    Ok(())
}
fn boundaries(b: &[u64], size: u64) -> Result<()> {
    if b.is_empty()
        || b.len() > zip::MAX_ENTRIES + 1
        || b.last() != Some(&size)
        || b[0] == 0
        || b.windows(2).any(|v| v[0] >= v[1])
    {
        return Err(invalid("invalid container boundaries"));
    }
    Ok(())
}
impl GameProfile {
    pub fn load(path: &Path) -> Result<Self> {
        let v: Self = load_json(path)?;
        v.validate()?;
        Ok(v)
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != VERSION
            || self.generated_by != "playsparse-game-v1"
            || self.generated_at.is_some()
        {
            return Err(invalid("unsupported game profile version/provenance"));
        }
        identity(&self.source_identity)?;
        if self.files.len() > MAX_FILES
            || self.executables.len() > 64
            || self.anti_cheat_detected.len() > 32
            || self.engine.other_signals.len() > 8
            || !key(&self.engine.key)
            || !text(&self.engine.label, 128)
            || self.engine.confidence.is_some_and(|v| v > 100)
            || self.engine.version.as_ref().is_some_and(|v| !text(v, 64))
            || !matches!(
                self.scanner_status.as_str(),
                "generic" | "absent-fallback" | "universal-modder"
            )
            || self.hints.runtime_policy != "unchanged-static-default"
            || !self.hints.startup_regions.is_empty()
        {
            return Err(invalid("invalid game profile metadata"));
        }
        for v in [&self.game.name, &self.game.store, &self.game.appid]
            .into_iter()
            .flatten()
        {
            if !text(v, 128) {
                return Err(invalid("unsafe game label"));
            }
        }
        paths(&self.engine.evidence_paths, 64)?;
        for s in &self.engine.other_signals {
            if !key(&s.key) || s.confidence.is_some_and(|v| v > 100) {
                return Err(invalid("invalid engine signal"));
            }
            paths(&s.evidence_paths, 64)?;
        }
        for e in &self.executables {
            if !valid_path(&e.path) || !text(&e.architecture, 32) {
                return Err(invalid("unsafe executable hint"));
            }
        }
        for a in &self.anti_cheat_detected {
            if !text(a, 128) {
                return Err(invalid("unsafe anti-cheat label"));
            }
        }
        if self.files.windows(2).any(|f| f[0].path >= f[1].path) {
            return Err(invalid("unsorted/duplicate files"));
        }
        for f in &self.files {
            if !valid_path(&f.path)
                || f.path.len() > 1024
                || f.mode > 0o777
                || f.container_hint.as_ref().is_some_and(|v| !text(v, 64))
                || f.signature.as_ref().is_some_and(|v| !text(v, 64))
            {
                return Err(invalid("unsafe analyzed file"));
            }
            parse_hash(&f.blake3)?;
            measurement(&f.measurement, f.size)?;
            if let Some(b) = &f.zip_boundaries {
                boundaries(b, f.size)?;
            }
        }
        Ok(())
    }
    pub fn matches_source(&self, measured: &Self) -> Result<()> {
        self.validate()?;
        if self.source_identity != measured.source_identity
            || self.files.len() != measured.files.len()
            || self.files.iter().zip(&measured.files).any(|(a, b)| {
                a.path != b.path || a.size != b.size || a.blake3 != b.blake3 || a.mode != b.mode
            })
        {
            return Err(invalid(
                "source/profile mismatch; inspect the current source again",
            ));
        }
        Ok(())
    }
}
impl PackingPlan {
    pub fn from_profile(p: &GameProfile, container_aware: bool) -> Result<Self> {
        Self::experimental(p, container_aware, false)
    }
    pub fn experimental(
        p: &GameProfile,
        container_aware: bool,
        skip_compression: bool,
    ) -> Result<Self> {
        p.validate()?;
        let files=p.files.iter().map(|f| {
            let use_zip=container_aware && f.zip_boundaries.is_some();
            PlannedFile { path:f.path.clone(), size:f.size, mode:f.mode, blake3:f.blake3.clone(), class:f.class,
                container_hint:f.container_hint.clone(), measurement:f.measurement.clone(),
                chunk_strategy:if use_zip {ChunkStrategy::ZipRecordsCdc} else {ChunkStrategy::Cdc},
                target_chunk_bytes:TARGET_BYTES,zstd_level:3,
                compression_strategy:if skip_compression && f.measurement.incompressible_candidate {CompressionStrategy::MeasuredRaw} else {CompressionStrategy::TryZstd},
                boundaries:if use_zip {f.zip_boundaries.clone().unwrap_or_default()} else {vec![]},
                reason:if skip_compression && f.measurement.incompressible_candidate {"three bounded Zstd probes failed to shrink; raw is experimental, unsampled bytes may compress"} else {"retain generic Zstd with per-object raw fallback"}.into(),
            }
        }).collect();
        Ok(Self {
            schema_version: VERSION,
            generated_by: "playsparse-game-v1".into(),
            source_identity: p.source_identity.clone(),
            container_aware,
            experimental_skip_compression: skip_compression,
            files,
        })
    }
    pub fn load(path: &Path) -> Result<Self> {
        let v: Self = load_json(path)?;
        v.validate()?;
        Ok(v)
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != VERSION
            || self.generated_by != "playsparse-game-v1"
            || self.files.len() > MAX_FILES
        {
            return Err(invalid("unsupported packing plan version or bounds"));
        }
        identity(&self.source_identity)?;
        if self.files.windows(2).any(|f| f[0].path >= f[1].path) {
            return Err(invalid("unsorted/duplicate plan files"));
        }
        for f in &self.files {
            if !valid_path(&f.path)
                || f.path.len() > 1024
                || f.target_chunk_bytes != TARGET_BYTES
                || f.mode > 0o777
                || f.zstd_level != 3
                || !text(&f.reason, 256)
                || f.container_hint.as_ref().is_some_and(|s| !text(s, 64))
            {
                return Err(invalid("unsafe plan file"));
            }
            parse_hash(&f.blake3)?;
            measurement(&f.measurement, f.size)?;
            match f.chunk_strategy {
                ChunkStrategy::Cdc if f.boundaries.is_empty() => (),
                ChunkStrategy::ZipRecordsCdc if self.container_aware => {
                    boundaries(&f.boundaries, f.size)?
                }
                _ => return Err(invalid("invalid planned chunk strategy")),
            }
            if f.compression_strategy == CompressionStrategy::MeasuredRaw
                && !self.experimental_skip_compression
            {
                return Err(invalid(
                    "measured raw requires explicit experimental opt-in",
                ));
            }
        }
        Ok(())
    }
    /// Imported measurements and offsets never authorize packing without recomputation.
    pub fn verify_source(&self, source: &Path) -> Result<()> {
        self.validate()?;
        let measured = inspect(source)?;
        let expected = Self::experimental(
            &measured,
            self.container_aware,
            self.experimental_skip_compression,
        )?;
        if self != &expected {
            return Err(invalid(
                "source/plan mismatch or unverified packing decision",
            ));
        }
        Ok(())
    }
}
pub fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let mut input = File::open(path)?;
    if input.metadata()?.len() > MAX_ARTIFACT_BYTES {
        return Err(invalid("artifact exceeds 16 MiB"));
    }
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(invalid("artifact exceeds 16 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|e| invalid(format!("invalid artifact JSON: {e}")))
}

fn classify(path: &str, head: &[u8]) -> (FileClass, Option<String>, Option<String>) {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let signature = if head.starts_with(b"PK\x03\x04") {
        Some("zip-local-header")
    } else if head.starts_with(b"UnityFS") {
        Some("unityfs")
    } else if head.starts_with(b"GDPC") {
        Some("godot-pck")
    } else if head.starts_with(b"VPK\0") || head.starts_with(&[0x34, 0x12, 0xaa, 0x55]) {
        Some("source-vpk")
    } else if head.starts_with(b"BSA\0") {
        Some("bethesda-bsa")
    } else if head.starts_with(b"BTDX") {
        Some("bethesda-ba2")
    } else if head.starts_with(b"RPF7") {
        Some("rage-rpf")
    } else if head.starts_with(b"MZ")
        || head.starts_with(b"\x7fELF")
        || head.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
    {
        Some("executable-header")
    } else {
        None
    };
    let hint = match ext.as_str() {
        "zip" | "pk3" | "pk4" | "jar" | "love" => Some("zip-family"),
        "pak" => Some("ambiguous-pak"),
        "utoc" | "ucas" => Some("unreal-iostore-candidate"),
        "pck" => Some("godot-pck-candidate"),
        "vpk" => Some("source-vpk-candidate"),
        "bsa" | "ba2" => Some("bethesda-archive-candidate"),
        "rpf" => Some("rage-rpf-candidate"),
        "archive" => Some("redengine-archive-candidate"),
        "dcx" | "bnd" | "bdt" => Some("fromsoft-container-candidate"),
        "bundle" | "assets" => Some("unity-assets-candidate"),
        _ => None,
    };
    let class = if signature == Some("executable-header") {
        FileClass::Code
    } else if hint.is_some() || signature.is_some() {
        FileClass::Container
    } else {
        match ext.as_str() {
            "exe" | "dll" | "so" | "dylib" => FileClass::Code,
            "js" | "lua" | "py" | "txt" | "gd" | "cs" => FileClass::ScriptsText,
            "json" | "ini" | "xml" | "cfg" | "yaml" => FileClass::Metadata,
            "dds" | "png" | "jpg" | "ktx" => FileClass::Texture,
            "ogg" | "mp3" | "wav" | "flac" => FileClass::Audio,
            "mp4" | "webm" | "bik" | "bk2" => FileClass::Video,
            "spv" | "shader" | "cache" => FileClass::ShaderCache,
            "bin" | "dat" => FileClass::Binary,
            _ => FileClass::Unknown,
        }
    };
    (class, hint.map(str::to_owned), signature.map(str::to_owned))
}
fn probes(input: &mut File, size: u64) -> Result<Measurement> {
    let length = size.min(SAMPLE_BYTES as u64) as usize;
    let offsets = if length == 0 {
        BTreeSet::new()
    } else {
        BTreeSet::from([0, (size - length as u64) / 2, size - length as u64])
    };
    let mut samples = Vec::new();
    let mut seen = BTreeSet::new();
    let mut repeated = 0;
    for offset in offsets {
        let mut raw = vec![0; length];
        input.seek(SeekFrom::Start(offset))?;
        input.read_exact(&mut raw)?;
        let hash = blake3::hash(&raw).to_string();
        if !seen.insert(hash.clone()) {
            repeated += 1;
        }
        let compressed = zstd::bulk::compress(&raw, 3)?;
        let mut counts = [0u32; 256];
        for b in &raw {
            counts[*b as usize] += 1;
        }
        let entropy = counts
            .iter()
            .filter(|n| **n > 0)
            .map(|n| {
                let p = *n as f64 / raw.len() as f64;
                -p * p.log2()
            })
            .sum::<f64>();
        let chunks = fastcdc::v2020::FastCDC::new(&raw, 65536, 262144, 1048576).count() as u32;
        samples.push(Sample {
            offset,
            bytes: length as u32,
            blake3: hash,
            zstd_bytes: compressed.len() as u32,
            entropy_millibits_per_byte: (entropy * 1000.0).round() as u32,
            cdc_chunks: chunks,
        });
    }
    let candidate = size >= 3 * SAMPLE_BYTES as u64
        && samples.len() == 3
        && repeated == 0
        && samples
            .iter()
            .all(|s| s.zstd_bytes >= s.bytes && s.entropy_millibits_per_byte >= 7900);
    Ok(Measurement {
        sampled_bytes: samples.iter().map(|s| s.bytes as u64).sum(),
        sampled_zstd_bytes: samples.iter().map(|s| s.zstd_bytes as u64).sum(),
        samples,
        repeated_sample_blocks: repeated,
        incompressible_candidate: candidate,
    })
}
pub fn inspect(source: &Path) -> Result<GameProfile> {
    let root = source.canonicalize()?;
    if !root.is_dir() {
        return Err(invalid("source must be a directory"));
    }
    let mut entries = Vec::new();
    let mut directories = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .min_depth(1)
        .follow_links(false)
        .max_open(16)
    {
        let e = entry.map_err(|e| invalid(e.to_string()))?;
        let p = e
            .path()
            .strip_prefix(&root)
            .map_err(|e| invalid(e.to_string()))?
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 source path"))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        if !valid_path(&p) || p.len() > 1024 {
            return Err(invalid("unsafe source path"));
        }
        if e.file_type().is_file() {
            entries.push((p, e.into_path()));
        } else if e.file_type().is_dir() {
            directories.push(p);
        } else {
            return Err(invalid("symlinks and special source files are unsupported"));
        }
        if entries.len() + directories.len() > MAX_FILES {
            return Err(invalid("source exceeds 8192 analysis entries"));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    directories.sort();
    let mut files = Vec::new();
    let mut artifact_budget: u64 = directories.iter().map(|p| p.len() as u64 + 8).sum();
    for (path, full) in entries {
        let mut input = File::open(full)?;
        let before = input.metadata()?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0; 1024 * 1024];
        let mut head = Vec::new();
        let mut read = 0u64;
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            if head.is_empty() {
                head.extend_from_slice(&buffer[..n.min(64)]);
            }
            hasher.update(&buffer[..n]);
            read = read
                .checked_add(n as u64)
                .ok_or_else(|| invalid("source size overflow"))?;
        }
        let measurement = probes(&mut input, before.len())?;
        let (class, container_hint, signature) = classify(&path, &head);
        let zip_boundaries = if head.starts_with(b"PK\x03\x04") {
            zip::boundaries(&mut input).ok()
        } else {
            None
        };
        let after = input.metadata()?;
        if read != before.len()
            || before.len() != after.len()
            || before.modified()? != after.modified()?
        {
            return Err(invalid("source changed during inspect"));
        }
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            before.permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = if before.permissions().readonly() {
            0o444
        } else {
            0o644
        };
        let analyzed = AnalyzedFile {
            path,
            size: before.len(),
            mode,
            blake3: hasher.finalize().to_string(),
            class,
            container_hint,
            signature,
            measurement,
            zip_boundaries,
        };
        artifact_budget += serde_json::to_vec(&analyzed)
            .map_err(|e| invalid(e.to_string()))?
            .len() as u64;
        if artifact_budget > MAX_ARTIFACT_BYTES - 8192 {
            return Err(invalid("analysis artifact exceeds bounded metadata budget"));
        }
        files.push(analyzed);
    }
    let mut h = blake3::Hasher::new();
    h.update(b"playsparse-source-blake3-v1\0");
    for d in &directories {
        h.update(b"D");
        h.update(&(d.len() as u64).to_le_bytes());
        h.update(d.as_bytes());
    }
    for f in &files {
        h.update(b"F");
        h.update(&(f.path.len() as u64).to_le_bytes());
        h.update(f.path.as_bytes());
        h.update(&f.size.to_le_bytes());
        h.update(&f.mode.to_le_bytes());
        h.update(&parse_hash(&f.blake3)?);
    }
    let p = GameProfile {
        schema_version: VERSION,
        generated_by: "playsparse-game-v1".into(),
        generated_at: None,
        scanner_status: "generic".into(),
        scanner_truncated: false,
        game: Game {
            name: None,
            store: None,
            appid: None,
        },
        engine: Engine {
            key: "unknown".into(),
            label: "Unknown engine".into(),
            confidence: None,
            version: None,
            evidence_paths: vec![],
            other_signals: vec![],
        },
        executables: vec![],
        anti_cheat_detected: vec![],
        source_identity: SourceIdentity {
            algorithm: "playsparse-source-blake3-v1".into(),
            digest: h.finalize().to_string(),
            directories,
        },
        files,
        hints: Hints {
            startup_regions: vec![],
            runtime_policy: "unchanged-static-default".into(),
        },
    };
    p.validate()?;
    Ok(p)
}

#[cfg(test)]
mod tests;
