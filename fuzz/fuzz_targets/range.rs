#![no_main]
use libfuzzer_sys::fuzz_target;
use playsparse_core::{ChunkRef,FileEntry,intersecting_chunks};
fuzz_target!(|data: &[u8]| {
    if data.len()<16 {return;}
    let mut offset_bytes=[0;8];offset_bytes.copy_from_slice(&data[..8]);
    let offset=u64::from_le_bytes(offset_bytes);
    let length=u16::from_le_bytes([data[8],data[9]]) as usize;
    let mut end=0u64;let mut chunks=Vec::new();
    for &byte in &data[10..] {let n=byte as u32+1;chunks.push(ChunkRef {hash:String::new(),offset:end,raw_size:n});end+=n as u64;}
    let file=FileEntry {path:"fuzz".into(),size:end,mode:0o444,hash:String::new(),chunks};
    let found=intersecting_chunks(&file,offset,length);
    let expected:Vec<_>=file.chunks.iter().enumerate().filter(|(_,c)|length>0 && offset<end && c.offset<offset.saturating_add(length as u64).min(end) && c.offset+c.raw_size as u64>offset).map(|(i,_)|i).collect();
    assert_eq!(found.collect::<Vec<_>>(),expected);
});
