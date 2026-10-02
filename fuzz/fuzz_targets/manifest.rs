#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(manifest) = serde_json::from_slice::<playsparse_core::Manifest>(data) {
        let _ = manifest.validate();
    }
});
