//! Narrow ZIP boundary reader. Never inflates entries or interprets assets.
use crate::invalid;
use playsparse_core::{Result, valid_path};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

pub const MAX_ENTRIES: usize = 4096;
const MAX_CENTRAL_BYTES: u64 = 8 * 1024 * 1024;
fn u16_at(b: &[u8], p: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        b.get(p..p + 2)
            .ok_or_else(|| invalid("short ZIP field"))?
            .try_into()
            .map_err(|_| invalid("ZIP u16"))?,
    ))
}
fn u32_at(b: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or_else(|| invalid("short ZIP field"))?
            .try_into()
            .map_err(|_| invalid("ZIP u32"))?,
    ))
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| invalid("ZIP offset overflow"))
}
fn read(input: &mut File, off: u64, len: usize) -> Result<Vec<u8>> {
    let mut b = vec![0; len];
    input.seek(SeekFrom::Start(off))?;
    input.read_exact(&mut b)?;
    Ok(b)
}
fn extra(b: &[u8]) -> Result<()> {
    let mut p = 0;
    while p < b.len() {
        let kind = u16_at(b, p)?;
        let len = u16_at(b, p + 2)? as usize;
        p += 4;
        if kind == 1 || p.checked_add(len).is_none_or(|end| end > b.len()) {
            return Err(invalid("ZIP64 or malformed ZIP extra field"));
        }
        p += len;
    }
    Ok(())
}
struct Entry {
    offset: u64,
    end: u64,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: u32,
    size: u32,
    name: Vec<u8>,
}
pub fn boundaries(input: &mut File) -> Result<Vec<u64>> {
    let size = input.metadata()?.len();
    if size < 22 {
        return Err(invalid("short ZIP"));
    }
    let tail_len = size.min(65557) as usize;
    let tail = read(input, size - tail_len as u64, tail_len)?;
    let p = (0..=tail.len() - 22)
        .rev()
        .find(|p| {
            tail[*p..*p + 4] == *b"PK\x05\x06"
                && u16_at(&tail, *p + 20).is_ok_and(|n| *p + 22 + n as usize == tail.len())
        })
        .ok_or_else(|| invalid("ZIP EOCD missing or trailing data"))?;
    let e = &tail[p..];
    let eocd = size - tail_len as u64 + p as u64;
    let count = u16_at(e, 10)? as usize;
    let cd_size = u32_at(e, 12)? as u64;
    let cd_off = u32_at(e, 16)? as u64;
    if u16_at(e, 4)? != 0
        || u16_at(e, 6)? != 0
        || u16_at(e, 8)? as usize != count
        || count == 0
        || count > MAX_ENTRIES
        || count == 65535
        || cd_size > MAX_CENTRAL_BYTES
        || cd_size == u32::MAX as u64
        || cd_off == u32::MAX as u64
        || add(cd_off, cd_size)? != eocd
    {
        return Err(invalid(
            "unsupported ZIP count, volume, central bounds or ZIP64",
        ));
    }
    let central = read(input, cd_off, cd_size as usize)?;
    let mut at = 0usize;
    let mut entries = Vec::new();
    for _ in 0..count {
        let h = central
            .get(at..at + 46)
            .ok_or_else(|| invalid("short ZIP central entry"))?;
        if &h[..4] != b"PK\x01\x02" || u16_at(h, 34)? != 0 {
            return Err(invalid("invalid ZIP central entry"));
        }
        let flags = u16_at(h, 8)?;
        let method = u16_at(h, 10)?;
        let compressed = u32_at(h, 20)?;
        let raw = u32_at(h, 24)?;
        let offset = u32_at(h, 42)? as u64;
        let name_len = u16_at(h, 28)? as usize;
        let extra_len = u16_at(h, 30)? as usize;
        let comment_len = u16_at(h, 32)? as usize;
        // Support UTF-8 flag and normal deflate option bits only. No descriptors/encryption.
        if flags & !0x0806 != 0
            || !matches!(method, 0 | 8)
            || compressed == u32::MAX
            || raw == u32::MAX
            || offset == u32::MAX as u64
            || name_len == 0
        {
            return Err(invalid("unsupported ZIP flags, codec or ZIP64"));
        }
        let end = at
            .checked_add(46 + name_len + extra_len + comment_len)
            .ok_or_else(|| invalid("ZIP count overflow"))?;
        if end > central.len() {
            return Err(invalid("ZIP central lengths"));
        }
        let name = central[at + 46..at + 46 + name_len].to_vec();
        let path = std::str::from_utf8(&name).map_err(|_| invalid("non-UTF8 ZIP name"))?;
        if !valid_path(path.trim_end_matches('/')) {
            return Err(invalid("unsafe ZIP entry name"));
        }
        extra(&central[at + 46 + name_len..at + 46 + name_len + extra_len])?;
        entries.push(Entry {
            offset,
            end: 0,
            flags,
            method,
            crc: u32_at(h, 16)?,
            compressed,
            size: raw,
            name,
        });
        at = end;
    }
    if at != central.len() {
        return Err(invalid("unaccounted ZIP central bytes"));
    }
    entries.sort_by_key(|e| e.offset);
    let mut end = 0;
    for entry in &mut entries {
        if entry.offset != end {
            return Err(invalid("ZIP local entries overlap, gap or embedded prefix"));
        }
        let local = read(input, entry.offset, 30)?;
        if &local[..4] != b"PK\x03\x04"
            || u16_at(&local, 6)? != entry.flags
            || u16_at(&local, 8)? != entry.method
            || u32_at(&local, 14)? != entry.crc
            || u32_at(&local, 18)? != entry.compressed
            || u32_at(&local, 22)? != entry.size
        {
            return Err(invalid("ZIP local and central mismatch"));
        }
        let name_len = u16_at(&local, 26)? as usize;
        let extra_len = u16_at(&local, 28)? as usize;
        let data_off = add(entry.offset, 30 + (name_len + extra_len) as u64)?;
        end = add(data_off, entry.compressed as u64)?;
        if end > cd_off {
            return Err(invalid("ZIP payload exceeds local region"));
        }
        let fields = read(input, add(entry.offset, 30)?, name_len + extra_len)?;
        if fields[..name_len] != entry.name {
            return Err(invalid("ZIP name mismatch"));
        }
        extra(&fields[name_len..])?;
        if entry.method == 0 && entry.compressed != entry.size {
            return Err(invalid("stored ZIP size mismatch"));
        }
        entry.end = end;
    }
    if end != cd_off {
        return Err(invalid("ZIP local coverage mismatch"));
    }
    let mut result: Vec<_> = entries.iter().map(|e| e.end).collect();
    result.push(size);
    Ok(result)
}
