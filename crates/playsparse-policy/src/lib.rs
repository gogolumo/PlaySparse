//! Versioned policy and bounded observations; cache and prefetch execute decisions.
use playsparse_core::{Error, Result, valid_path};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read},
    num::NonZeroUsize,
    path::Path,
};

pub const VERSION: u32 = 1;
const MAX_POLICY_BYTES: u64 = 1024 * 1024;
const MAX_TRACKED: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Eviction {
    Lru,
    DecayingHotness,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prefetch {
    pub enabled: bool,
    pub sequential_reads: u32,
    pub max_chunks: usize,
    pub budget_bytes: usize,
    pub queue_depth: usize,
    pub ttl_ms: u64,
}
impl Default for Prefetch {
    fn default() -> Self {
        Self {
            enabled: true,
            sequential_reads: 3,
            max_chunks: 2,
            budget_bytes: 8 * 1024 * 1024,
            queue_depth: 32,
            ttl_ms: 500,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u32,
    /// Optional cap on the caller's cache budget; never increases it.
    pub cache_bytes: Option<usize>,
    pub eviction: Eviction,
    pub decay_accesses: u64,
    pub prefetch: Prefetch,
    /// Initial file priority, subsequently combined with decaying observations.
    pub files: BTreeMap<String, u32>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            version: VERSION,
            cache_bytes: None,
            eviction: Eviction::DecayingHotness,
            decay_accesses: 64,
            prefetch: Prefetch::default(),
            files: BTreeMap::new(),
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        let p = &self.prefetch;
        if self.version != VERSION
            || !(1..=1_000_000).contains(&self.decay_accesses)
            || self
                .cache_bytes
                .is_some_and(|bytes| bytes as u64 > 4 * 1024 * 1024 * 1024u64)
            || !(2..=32).contains(&p.sequential_reads)
            || !(1..=8).contains(&p.max_chunks)
            || p.budget_bytes > 128 * 1024 * 1024
            || !(1..=256).contains(&p.queue_depth)
            || !(1..=30_000).contains(&p.ttl_ms)
            || self.files.len() > MAX_TRACKED
        {
            return Err(Error::Invalid(
                "unsupported policy version or unbounded policy parameters".into(),
            ));
        }
        if self
            .files
            .iter()
            .any(|(path, priority)| !valid_path(path) || !(1..=1024).contains(priority))
        {
            return Err(Error::Invalid(
                "policy contains invalid file path or priority".into(),
            ));
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_POLICY_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_POLICY_BYTES {
            return Err(Error::Invalid("policy exceeds 1 MiB".into()));
        }
        let policy: Self = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Invalid(format!("invalid policy: {e}")))?;
        policy.validate()?;
        Ok(policy)
    }
}

/// Validated trace summary supplies bounded initial priorities. Optimization is
/// a deterministic suggestion; its benefit must be measured on replay.
pub fn optimize_trace(trace: &Path) -> Result<Policy> {
    let summary = playsparse_trace::summarize(trace)?;
    let mut policy = Policy::default();
    for file in summary["hot_files"].as_array().into_iter().flatten() {
        if let (Some(path), Some(events)) = (file["path"].as_str(), file["events"].as_u64())
            && valid_path(path)
        {
            policy
                .files
                .insert(path.into(), events.clamp(1, 1024) as u32);
        }
    }
    // The summary's strict adjacency metric groups callback workers; policy
    // prediction follows visible files and handles kernel overlapping windows.
    let mut reader = BufReader::new(File::open(trace)?);
    let mut line = Vec::new();
    let mut tracker = Tracker::default();
    let (mut reads, mut sequential, mut events) = (0u64, 0u64, 0usize);
    loop {
        line.clear();
        let count = reader
            .by_ref()
            .take(256 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        events += 1;
        if count > 256 * 1024 || events > 2_000_000 {
            return Err(Error::Invalid("policy trace exceeds bounded limits".into()));
        }
        let event: playsparse_trace::Event = serde_json::from_slice(&line)
            .map_err(|error| Error::Invalid(format!("invalid policy trace: {error}")))?;
        if event.version != playsparse_trace::VERSION
            || (!event.path.is_empty() && !valid_path(&event.path))
        {
            return Err(Error::Invalid(
                "unsupported policy trace version or path".into(),
            ));
        }
        if event.op == "read" && event.success && event.returned > 0 {
            reads += 1;
            if tracker
                .observe(
                    &policy,
                    &event.path,
                    &event.worker,
                    event.offset,
                    event.returned.min(usize::MAX as u64) as usize,
                    event.ts_ns / 1_000_000,
                )
                .sequential
            {
                sequential += 1;
            }
        }
    }
    policy.prefetch.enabled = reads > 0 && sequential as f64 / reads as f64 >= 0.2;
    policy.validate()?;
    Ok(policy)
}

#[derive(Debug, Clone, Copy)]
pub struct Observation {
    pub sequential: bool,
    pub priority: u32,
    /// Unique forward-covered bytes in the current contiguous/overlapping run.
    pub forward_bytes: u64,
}
struct Sequence {
    start: u64,
    end: u64,
    confidence: u32,
    at_ms: u64,
    forward_bytes: u64,
}
struct Heat {
    score: u32,
    epoch: u64,
}
pub struct Tracker {
    sequences: lru::LruCache<String, Sequence>,
    files: lru::LruCache<String, Heat>,
    epoch: u64,
}
impl Default for Tracker {
    fn default() -> Self {
        Self {
            sequences: lru::LruCache::new(NonZeroUsize::new(MAX_TRACKED).unwrap()),
            files: lru::LruCache::new(NonZeroUsize::new(MAX_TRACKED).unwrap()),
            epoch: 0,
        }
    }
}
impl Tracker {
    pub fn observe(
        &mut self,
        policy: &Policy,
        path: &str,
        _worker: &str,
        offset: u64,
        returned: usize,
        at_ms: u64,
    ) -> Observation {
        self.epoch = self.epoch.saturating_add(1);
        let epoch = self.epoch;
        let heat = self.files.get(path).map_or(0, |heat| {
            decay(
                heat.score,
                epoch.saturating_sub(heat.epoch),
                policy.decay_accesses,
            )
        });
        let score = heat.saturating_add(1).min(1024);
        self.files.put(path.into(), Heat { score, epoch });
        // Filesystem dispatcher threads are not application stream identities:
        // a single kernel read-ahead stream migrates between callback workers.
        let key = path.to_string();
        let end = offset.saturating_add(returned as u64);
        let previous = self.sequences.get(&key).filter(|sequence| {
            offset > sequence.start
                && offset <= sequence.end
                && end > sequence.end
                && at_ms.saturating_sub(sequence.at_ms) <= policy.prefetch.ttl_ms
        });
        let (confidence, forward_bytes) = previous.map_or((1, returned as u64), |sequence| {
            (
                sequence.confidence.saturating_add(1),
                sequence.forward_bytes.saturating_add(end - sequence.end),
            )
        });
        self.sequences.put(
            key,
            Sequence {
                start: offset,
                end,
                confidence,
                at_ms,
                forward_bytes,
            },
        );
        Observation {
            sequential: returned > 0 && confidence >= policy.prefetch.sequential_reads,
            priority: score
                .saturating_add(policy.files.get(path).copied().unwrap_or(0))
                .min(2048),
            forward_bytes,
        }
    }
}

pub fn decay(score: u32, elapsed: u64, interval: u64) -> u32 {
    score >> (elapsed / interval.max(1)).min(31)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forward_coverage_counts_unique_bytes_and_resets_for_tiny_bursts_and_gaps() {
        let policy = Policy::default();
        let mut tracker = Tracker::default();
        assert_eq!(
            tracker.observe(&policy, "file", "w", 0, 8, 0).forward_bytes,
            8
        );
        assert_eq!(
            tracker.observe(&policy, "file", "w", 4, 8, 1).forward_bytes,
            12
        );
        let observation = tracker.observe(&policy, "file", "w", 8, 8, 2);
        assert!(observation.sequential);
        assert_eq!(observation.forward_bytes, 16);
        let repeated = tracker.observe(&policy, "file", "w", 8, 8, 3);
        assert!(!repeated.sequential);
        assert_eq!(repeated.forward_bytes, 8);
        assert_eq!(
            tracker
                .observe(&policy, "file", "w", 100, 1, 4)
                .forward_bytes,
            1
        );
        tracker.observe(&policy, "tiny", "w", 0, 1, 0);
        tracker.observe(&policy, "tiny", "w", 1, 1, 1);
        let tiny = tracker.observe(&policy, "tiny", "w", 2, 1, 2);
        assert!(tiny.sequential);
        assert_eq!(tiny.forward_bytes, 3);
    }
    #[test]
    fn sequence_confidence_random_rejection_and_expiry() {
        let policy = Policy::default();
        let mut tracker = Tracker::default();
        assert!(
            !tracker
                .observe(&policy, "file", "worker", 0, 4096, 0)
                .sequential
        );
        assert!(
            !tracker
                .observe(&policy, "file", "worker", 4096, 4096, 1)
                .sequential
        );
        assert!(
            tracker
                .observe(&policy, "file", "worker", 8192, 4096, 2)
                .sequential
        );
        assert!(
            !tracker
                .observe(&policy, "file", "worker", 500, 4096, 3)
                .sequential
        );
        assert!(
            !tracker
                .observe(&policy, "file", "other", 4596, 4096, 4)
                .sequential
        );
        assert!(
            !tracker
                .observe(&policy, "file", "worker", 4596, 4096, 1000)
                .sequential
        );
    }
    #[test]
    fn hotness_decays_and_tracking_is_bounded() {
        assert_eq!(decay(64, 128, 64), 16);
        let policy = Policy::default();
        let mut tracker = Tracker::default();
        let first = tracker.observe(&policy, "hot", "w", 0, 1, 0).priority;
        let hot = tracker.observe(&policy, "hot", "w", 0, 1, 1).priority;
        assert!(hot > first);
        for i in 0..10_000 {
            tracker.observe(&policy, &format!("file-{i}"), "w", 0, 1, 2);
        }
        assert!(tracker.files.len() <= MAX_TRACKED);
        assert!(tracker.sequences.len() <= MAX_TRACKED);
        assert_eq!(
            tracker.observe(&policy, "hot", "w", 0, 1, 3).priority,
            first
        );
    }
    #[test]
    fn migrating_dispatch_workers_and_forward_overlap_are_sequential() {
        let policy = Policy::default();
        let mut tracker = Tracker::default();
        assert!(
            !tracker
                .observe(&policy, "file", "worker-1", 0, 131072, 0)
                .sequential
        );
        assert!(
            !tracker
                .observe(&policy, "file", "worker-2", 65536, 131072, 1)
                .sequential
        );
        assert!(
            tracker
                .observe(&policy, "file", "worker-3", 131072, 131072, 2)
                .sequential
        );
        assert!(
            !tracker
                .observe(&policy, "file", "worker-1", 131072, 131072, 3)
                .sequential
        ); // repeated window
        assert!(
            !tracker
                .observe(&policy, "file", "worker-2", 0, 131072, 4)
                .sequential
        ); // backward
        assert!(
            !tracker
                .observe(&policy, "file", "worker-3", 500000, 131072, 5)
                .sequential
        ); // gap
        assert!(
            !tracker
                .observe(&policy, "other-file", "worker-1", 631072, 131072, 6)
                .sequential
        );
    }
    #[test]
    fn strict_invalid_configuration_and_roundtrip() {
        let mut policy = Policy::default();
        policy.validate().unwrap();
        policy.prefetch.queue_depth = 257;
        assert!(policy.validate().is_err());
        let mut value = serde_json::to_value(Policy::default()).unwrap();
        value["unknown"] = 1.into();
        assert!(serde_json::from_value::<Policy>(value).is_err());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("policy");
        std::fs::write(&path, serde_json::to_vec(&Policy::default()).unwrap()).unwrap();
        Policy::load(&path).unwrap();
    }
    #[test]
    fn optimizer_uses_forward_overlap_confidence_and_rejects_random_stream() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace");
        let trace = playsparse_trace::TraceWriter::open(&path).unwrap();
        for offset in [0, 65536, 131072] {
            trace.record(
                "read",
                "data/file",
                offset,
                131072,
                131072,
                1,
                "miss",
                "primary-local",
                true,
            );
        }
        trace.shutdown();
        assert!(optimize_trace(&path).unwrap().prefetch.enabled);
        let path = tmp.path().join("random");
        let trace = playsparse_trace::TraceWriter::open(&path).unwrap();
        for offset in [0, 6553600, 128, 450000, 128] {
            trace.record(
                "read",
                "data/file",
                offset,
                4096,
                4096,
                1,
                "miss",
                "primary-local",
                true,
            );
        }
        trace.shutdown();
        assert!(!optimize_trace(&path).unwrap().prefetch.enabled);
    }
}
