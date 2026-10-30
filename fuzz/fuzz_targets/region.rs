//! A region file's bytes: every chunk it points at is read, decompressed
//! and parsed.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Oversized chunks would be looked for next to this path; there are none.
    let path = std::path::Path::new("/nonexistent/r.0.0.mca");
    let Ok(region) = mc_storage::region::RegionFile::from_bytes(path, data.to_vec()) else {
        return;
    };
    for (x, z) in region.chunks() {
        let _ = region.timestamp(x, z);
        if let Ok(Some(raw)) = region.raw_chunk(x, z) {
            let _ = raw.parse();
        }
    }
});
