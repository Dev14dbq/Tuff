//! Packet framing: `VarInt length | VarInt packet id | body`, optionally zlib
//! compressed after Set Compression. Encryption comes with online mode.

use std::io::{Read, Write as _};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::codec::{DecodeError, Reader, Result, Write, varint_len};

/// Largest frame length the protocol allows (fits a 3-byte VarInt).
pub const MAX_FRAME_LEN: usize = 2_097_151;
/// Largest uncompressed serverbound packet the vanilla server accepts.
pub const MAX_UNCOMPRESSED_LEN: usize = 8_388_608;

/// Looks for a complete frame at the start of `buf`.
///
/// Returns `Some((header_len, body_len))` when the whole frame is buffered,
/// `None` when more bytes are needed.
pub fn split_frame(buf: &[u8]) -> Result<Option<(usize, usize)>> {
    let mut len: usize = 0;
    for i in 0..3 {
        let Some(&byte) = buf.get(i) else {
            return Ok(None);
        };
        len |= usize::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            let header = i + 1;
            if len == 0 {
                return Err(DecodeError::Invalid("empty frame"));
            }
            return Ok((buf.len() >= header + len).then_some((header, len)));
        }
    }
    Err(DecodeError::Invalid("frame length longer than 3 bytes"))
}

/// Appends one frame with `id` and `body`. With `compression = Some(threshold)`
/// the compressed format is used, and bodies of at least `threshold` bytes are
/// zlib-compressed.
pub fn write_frame(out: &mut Vec<u8>, id: i32, body: &[u8], compression: Option<usize>) {
    let inner_len = varint_len(id) + body.len();
    match compression {
        None => {
            assert!(
                inner_len <= MAX_FRAME_LEN,
                "packet of {inner_len} bytes is too large"
            );
            out.put_len(inner_len);
            out.put_varint(id);
            out.put_slice(body);
        }
        Some(threshold) if inner_len < threshold => {
            // Data Length 0 marks an uncompressed packet.
            out.put_len(1 + inner_len);
            out.put_u8(0);
            out.put_varint(id);
            out.put_slice(body);
        }
        Some(_) => {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            let mut id_bytes = Vec::with_capacity(5);
            id_bytes.put_varint(id);
            encoder
                .write_all(&id_bytes)
                .and_then(|()| encoder.write_all(body))
                .expect("writing to a Vec cannot fail");
            let compressed = encoder.finish().expect("writing to a Vec cannot fail");
            let len = varint_len(inner_len as i32) + compressed.len();
            assert!(
                len <= MAX_FRAME_LEN,
                "compressed packet of {len} bytes is too large"
            );
            out.put_len(len);
            out.put_len(inner_len);
            out.put_slice(&compressed);
        }
    }
}

/// Turns a frame body (after the length prefix) into `packet id + data`,
/// undoing compression when it is enabled.
pub fn decode_frame_body(frame: &[u8], compression: Option<usize>) -> Result<Vec<u8>> {
    let Some(threshold) = compression else {
        return Ok(frame.to_vec());
    };
    let mut reader = Reader::new(frame);
    let data_len = reader.length_prefix()?;
    let rest = reader.rest();
    if data_len == 0 {
        return Ok(rest.to_vec());
    }
    if data_len < threshold {
        return Err(DecodeError::Invalid(
            "compressed packet below the threshold",
        ));
    }
    if data_len > MAX_UNCOMPRESSED_LEN {
        return Err(DecodeError::Invalid("uncompressed packet too large"));
    }
    let mut out = Vec::with_capacity(data_len);
    ZlibDecoder::new(rest)
        .take(data_len as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| DecodeError::Invalid("zlib data"))?;
    if out.len() != data_len {
        return Err(DecodeError::Invalid("decompressed length mismatch"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_across_reads() {
        let mut stream = Vec::new();
        write_frame(&mut stream, 0x2a, &[1, 2, 3], None);
        write_frame(&mut stream, 0x00, &[9; 300], None);

        for cut in 0..4 {
            assert_eq!(split_frame(&stream[..cut]).unwrap(), None);
        }
        assert_eq!(split_frame(&stream).unwrap(), Some((1, 4)));
        let second = &stream[5..];
        assert_eq!(split_frame(&second[..2]).unwrap(), None);
        assert_eq!(split_frame(second).unwrap(), Some((2, 301)));
    }

    #[test]
    fn oversized_length_is_rejected() {
        assert!(split_frame(&[0x80, 0x80, 0x80, 0x01]).is_err());
        assert!(split_frame(&[0x00]).is_err());
    }

    #[test]
    fn compressed_roundtrip() {
        for (body, threshold) in [(vec![7u8; 10], 256), (vec![7u8; 5000], 256)] {
            let mut stream = Vec::new();
            write_frame(&mut stream, 0x2c, &body, Some(threshold));
            let (header, len) = split_frame(&stream).unwrap().unwrap();
            assert_eq!(header + len, stream.len());
            let inner = decode_frame_body(&stream[header..], Some(threshold)).unwrap();
            let mut reader = Reader::new(&inner);
            assert_eq!(reader.varint().unwrap(), 0x2c);
            assert_eq!(reader.rest(), &body[..]);
        }
        // Small packets stay small; big repetitive ones shrink.
        let mut big = Vec::new();
        write_frame(&mut big, 1, &[0xff; 50_000], Some(256));
        assert!(big.len() < 500, "{} bytes", big.len());
    }
}
