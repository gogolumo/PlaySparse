//! Exact external aggregation of read-range identities, with fixed-size sort runs.
use playsparse_core::{Error, Result};
use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
};

type Key = (u64, u64, u64);
const RUN_KEYS: usize = 16384;
const MAX_RUNS: usize = 128; // 2M events => at most 123 runs; bounded descriptors.

pub struct Ranges {
    work: tempfile::TempDir,
    pending: Vec<Key>,
    runs: usize,
}
impl Ranges {
    pub fn new() -> Result<Self> {
        Ok(Self {
            work: tempfile::Builder::new()
                .prefix("playsparse-trace-")
                .tempdir()?,
            pending: Vec::with_capacity(RUN_KEYS),
            runs: 0,
        })
    }
    pub fn add(&mut self, key: Key) -> Result<()> {
        self.pending.push(key);
        if self.pending.len() == RUN_KEYS {
            self.flush()?;
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if self.runs >= MAX_RUNS {
            return Err(Error::Invalid("trace range sort exceeds run limit".into()));
        }
        self.pending.sort_unstable();
        let mut output =
            BufWriter::new(File::create(self.work.path().join(self.runs.to_string()))?);
        for &(file, offset, length) in &self.pending {
            for value in [file, offset, length] {
                output.write_all(&value.to_le_bytes())?;
            }
        }
        output.flush()?;
        self.pending.clear();
        self.runs += 1;
        Ok(())
    }
    pub fn finish(mut self) -> Result<(u64, Vec<(Key, u64)>)> {
        self.flush()?;
        let mut readers = (0..self.runs)
            .map(|i| File::open(self.work.path().join(i.to_string())).map(BufReader::new))
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut heap = BinaryHeap::new();
        for (i, reader) in readers.iter_mut().enumerate() {
            if let Some(key) = read_key(reader)? {
                heap.push(Reverse((key, i)));
            }
        }
        let mut unique = 0u64;
        let mut top = Vec::new();
        let mut current = None;
        let mut count = 0u64;
        while let Some(Reverse((key, i))) = heap.pop() {
            if current == Some(key) {
                count += 1;
            } else {
                if let Some(previous) = current {
                    keep_top(&mut top, previous, count);
                }
                unique += 1;
                current = Some(key);
                count = 1;
            }
            if let Some(next) = read_key(&mut readers[i])? {
                heap.push(Reverse((next, i)));
            }
        }
        if let Some(key) = current {
            keep_top(&mut top, key, count);
        }
        Ok((unique, top))
    }
}
fn read_key(reader: &mut impl Read) -> Result<Option<Key>> {
    let mut bytes = [0u8; 24];
    // Distinguish clean EOF from truncated internal scratch data.
    if reader.read(&mut bytes[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut bytes[1..])?;
    let number = |start| u64::from_le_bytes(bytes[start..start + 8].try_into().unwrap());
    Ok(Some((number(0), number(8), number(16))))
}
fn keep_top(top: &mut Vec<(Key, u64)>, key: Key, count: u64) {
    top.push((key, count));
    top.sort_unstable_by_key(|(key, count)| (Reverse(*count), *key));
    top.truncate(20);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_counts_merge_across_runs_beyond_old_range_limit() {
        let mut ranges = Ranges::new().unwrap();
        for offset in 0..110000 {
            ranges.add((0, offset, 4)).unwrap();
        }
        for _ in 0..7 {
            ranges.add((0, 90000, 4)).unwrap();
        }
        let (unique, top) = ranges.finish().unwrap();
        assert_eq!(unique, 110000);
        assert_eq!(top[0], ((0, 90000, 4), 8));
    }
    #[test]
    fn empty_and_truncated_scratch() {
        assert_eq!(Ranges::new().unwrap().finish().unwrap(), (0, vec![]));
        assert!(read_key(&mut &b"incomplete"[..]).is_err());
    }
}
