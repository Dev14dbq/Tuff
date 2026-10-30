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

/// Bytes read from a connection, cut into frames. Taking a frame only moves
/// a cursor; the unread rest is moved to the front when more is read, which
/// happens only once no whole frame is left. Dropping each frame from the
/// front of a `Vec` instead costs a copy of everything behind it, and a
/// buffer full of tiny frames makes that quadratic.
#[derive(Debug, Default)]
pub struct FrameBuffer {
    buf: Vec<u8>,
    start: usize,
}

impl FrameBuffer {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(4096),
            start: 0,
        }
    }

    /// The bytes not yet taken as frames.
    pub fn unread(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    /// [`FrameBuffer::unread`], to decrypt in place.
    pub fn unread_mut(&mut self) -> &mut [u8] {
        &mut self.buf[self.start..]
    }

    /// The buffer to append newly read bytes to, with the taken frames
    /// dropped from its front.
    pub fn tail(&mut self) -> &mut Vec<u8> {
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        &mut self.buf
    }

    /// The next whole frame's body (after its length), if one is buffered.
    pub fn next_frame(&mut self) -> Result<Option<&[u8]>> {
        let Some((header, len)) = split_frame(self.unread())? else {
            return Ok(None);
        };
        let body = self.start + header;
        self.start = body + len;
        Ok(Some(&self.buf[body..body + len]))
    }
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
    // The claimed length is only a promise; grow towards it as data comes.
    let mut out = Vec::with_capacity(data_len.min(64 * 1024));
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
    use crate::codec::Write;

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
    fn hostile_bytes_do_not_panic() {
        for b in 0..=255u8 {
            let _ = split_frame(&[b, b, b, b, b]);
            let _ = decode_frame_body(&[b; 32], None);
            let _ = decode_frame_body(&[b; 32], Some(1));
        }
        let mut claimed = Vec::new();
        claimed.put_len(MAX_UNCOMPRESSED_LEN.saturating_add(1));
        claimed.extend_from_slice(&[0x78, 0x9c, 0x01, 0x00, 0x00]);
        assert!(decode_frame_body(&claimed, Some(1)).is_err());
    }

    #[test]
    fn frame_buffer_takes_frames_in_order() {
        let mut stream = Vec::new();
        write_frame(&mut stream, 1, &[1], None);
        write_frame(&mut stream, 2, &[2; 200], None);
        let mut frames = FrameBuffer::new();
        // Fed one byte at a time, as a slow client would.
        let mut seen = Vec::new();
        for &b in &stream {
            frames.tail().push(b);
            while let Some(frame) = frames.next_frame().unwrap() {
                seen.push(frame.to_vec());
            }
        }
        assert_eq!(seen, [vec![1, 1], [&[2u8][..], &[2; 200]].concat()]);
        assert!(frames.unread().is_empty());
    }

    #[test]
    fn many_tiny_frames_take_linear_time() {
        // A full 2 MiB read of one-byte frames.
        let mut frames = FrameBuffer::new();
        for _ in 0..MAX_FRAME_LEN / 2 {
            frames.tail().extend_from_slice(&[1, 0]);
        }
        let started = std::time::Instant::now();
        let mut n = 0;
        while frames.next_frame().unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, MAX_FRAME_LEN / 2);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn zlib_bomb_is_cut_off() {
        // 8 MiB of zeros claimed as 1000 bytes: inflating stops right after
        // the claimed length and the packet is refused.
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&vec![0; MAX_UNCOMPRESSED_LEN]).unwrap();
        let bomb = encoder.finish().unwrap();
        assert!(bomb.len() < 64 * 1024);
        let mut frame = Vec::new();
        frame.put_len(1000);
        frame.extend_from_slice(&bomb);
        assert_eq!(
            decode_frame_body(&frame, Some(256)),
            Err(DecodeError::Invalid("decompressed length mismatch"))
        );
        // Claiming more than vanilla's 8 MiB is refused before inflating.
        let mut frame = Vec::new();
        frame.put_len(MAX_UNCOMPRESSED_LEN + 1);
        frame.extend_from_slice(&bomb);
        assert_eq!(
            decode_frame_body(&frame, Some(256)),
            Err(DecodeError::Invalid("uncompressed packet too large"))
        );
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
