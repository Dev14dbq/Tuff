//! Bytes from a client, the way the connection reads them: frames cut from
//! the stream, decompressed, then decoded for the connection's state. The
//! first byte picks the state and whether compression is on; with its high
//! bit set, the second byte picks a packet and the rest is its body, so the
//! decoders are reached without valid framing.

#![no_main]

use libfuzzer_sys::fuzz_target;
use mc_protocol::frame::{FrameBuffer, decode_frame_body};

fuzz_target!(|data: &[u8]| {
    let states = tuff_fuzz::serverbound();
    let [mode, rest @ ..] = data else { return };
    let Some((state, names)) = states.get(usize::from(mode & 0x7) % states.len().max(1)) else {
        return;
    };
    if mode & 0x80 != 0 {
        let [pick, body @ ..] = rest else { return };
        if let Some(name) = names.get(usize::from(*pick) % names.len().max(1)) {
            tuff_fuzz::decode(*state, name, body);
        }
        return;
    }
    let compression = (mode & 0x08 != 0).then_some(256);
    let mut frames = FrameBuffer::new();
    // Arrives in pieces, as reads do.
    for piece in rest.chunks(97) {
        frames.tail().extend_from_slice(piece);
        loop {
            let frame = match frames.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(_) => return,
            };
            let Ok(body) = decode_frame_body(frame, compression) else {
                return;
            };
            let mut r = mc_protocol::Reader::new(&body);
            let Ok(id) = r.varint() else { return };
            let Some(name) = usize::try_from(id).ok().and_then(|id| names.get(id)) else {
                return;
            };
            tuff_fuzz::decode(*state, name, r.rest());
        }
    }
});
