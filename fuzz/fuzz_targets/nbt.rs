//! NBT from a client (network form, with its quota), from files (named
//! form) and as SNBT text; whatever decodes must encode again.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok((tag, _)) = mc_nbt::read_network(data) {
        let mut out = Vec::new();
        mc_nbt::write_network(&tag, &mut out).expect("a decoded tag encodes");
        let (again, used) = mc_nbt::read_network_with(&out, mc_nbt::Limits::TRUSTED).unwrap();
        assert_eq!(used, out.len());
        // Bytes, not tags: a NaN float is not equal to itself.
        let mut twice = Vec::new();
        mc_nbt::write_network(&again, &mut twice).unwrap();
        assert_eq!(twice, out);
    }
    let _ = mc_nbt::read_network_optional(data);
    if let Ok((name, tag, _)) = mc_nbt::read_named(data) {
        let mut out = Vec::new();
        mc_nbt::write_named(&name, &tag, &mut out).expect("a decoded tag encodes");
    }
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = mc_nbt::parse_snbt(text);
        let _ = mc_nbt::parse_snbt_prefix(text);
    }
});
