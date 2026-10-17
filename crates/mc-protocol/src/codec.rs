//! Primitive wire types: big-endian numbers, VarInt/VarLong, strings, UUIDs,
//! positions, bit sets and network NBT.

use std::fmt;

use mc_nbt::{NbtError, Tag};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    UnexpectedEof,
    VarIntTooLong,
    StringTooLong { max_chars: usize },
    InvalidUtf8,
    InvalidBool(u8),
    NegativeLength(i32),
    Nbt(NbtError),
    Invalid(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnexpectedEof => write!(f, "packet ended early"),
            DecodeError::VarIntTooLong => write!(f, "VarInt/VarLong is too long"),
            DecodeError::StringTooLong { max_chars } => {
                write!(f, "string longer than {max_chars} characters")
            }
            DecodeError::InvalidUtf8 => write!(f, "string is not valid UTF-8"),
            DecodeError::InvalidBool(b) => write!(f, "invalid boolean byte {b}"),
            DecodeError::NegativeLength(n) => write!(f, "negative length {n}"),
            DecodeError::Nbt(err) => write!(f, "NBT: {err}"),
            DecodeError::Invalid(what) => write!(f, "invalid {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<NbtError> for DecodeError {
    fn from(err: NbtError) -> Self {
        DecodeError::Nbt(err)
    }
}

pub type Result<T> = std::result::Result<T, DecodeError>;

/// Block position packed into 64 bits: x (26), z (26), y (12), all signed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Position {
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub fn pack(self) -> i64 {
        ((i64::from(self.x) & 0x3FF_FFFF) << 38)
            | ((i64::from(self.z) & 0x3FF_FFFF) << 12)
            | (i64::from(self.y) & 0xFFF)
    }

    pub fn unpack(value: i64) -> Self {
        Self {
            x: (value >> 38) as i32,
            y: (value << 52 >> 52) as i32,
            z: (value << 26 >> 38) as i32,
        }
    }
}

// ---------------------------------------------------------------------------
// Reading

/// Cursor over the body of one packet.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(DecodeError::UnexpectedEof);
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Everything that is left, for trailing "rest of packet" fields.
    pub fn rest(&mut self) -> &'a [u8] {
        let slice = &self.buf[self.pos..];
        self.pos = self.buf.len();
        slice
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("bytes returned N bytes"))
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    pub fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(DecodeError::InvalidBool(other)),
        }
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.array()?))
    }

    pub fn uuid(&mut self) -> Result<u128> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    pub fn varint(&mut self) -> Result<i32> {
        let mut value: u32 = 0;
        for i in 0..5 {
            let byte = self.u8()?;
            value |= u32::from(byte & 0x7F) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(DecodeError::VarIntTooLong)
    }

    pub fn varlong(&mut self) -> Result<i64> {
        let mut value: u64 = 0;
        for i in 0..10 {
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7F) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value as i64);
            }
        }
        Err(DecodeError::VarIntTooLong)
    }

    /// A VarInt length that must be non-negative.
    pub fn length_prefix(&mut self) -> Result<usize> {
        let len = self.varint()?;
        usize::try_from(len).map_err(|_| DecodeError::NegativeLength(len))
    }

    /// UTF-8 string of at most `max_chars` UTF-16 code units, like vanilla checks.
    pub fn string(&mut self, max_chars: usize) -> Result<String> {
        let len = self.length_prefix()?;
        if len > max_chars * 3 {
            return Err(DecodeError::StringTooLong { max_chars });
        }
        let s = std::str::from_utf8(self.bytes(len)?).map_err(|_| DecodeError::InvalidUtf8)?;
        if s.encode_utf16().count() > max_chars {
            return Err(DecodeError::StringTooLong { max_chars });
        }
        Ok(s.to_owned())
    }

    pub fn identifier(&mut self) -> Result<String> {
        self.string(32767)
    }

    pub fn position(&mut self) -> Result<Position> {
        Ok(Position::unpack(self.i64()?))
    }

    pub fn byte_array(&mut self, max: usize) -> Result<&'a [u8]> {
        let len = self.length_prefix()?;
        if len > max {
            return Err(DecodeError::Invalid("byte array length"));
        }
        self.bytes(len)
    }

    pub fn nbt(&mut self) -> Result<Tag> {
        let (tag, used) = mc_nbt::read_network(&self.buf[self.pos..])?;
        self.pos += used;
        Ok(tag)
    }
}

// ---------------------------------------------------------------------------
// Writing

/// Write helpers for packet bodies.
pub trait Write {
    fn put_u8(&mut self, v: u8);
    fn put_slice(&mut self, v: &[u8]);

    fn put_i8(&mut self, v: i8) {
        self.put_u8(v as u8);
    }
    fn put_bool(&mut self, v: bool) {
        self.put_u8(u8::from(v));
    }
    fn put_u16(&mut self, v: u16) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_i16(&mut self, v: i16) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_i32(&mut self, v: i32) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_i64(&mut self, v: i64) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_f32(&mut self, v: f32) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_f64(&mut self, v: f64) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_uuid(&mut self, v: u128) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_varint(&mut self, v: i32) {
        let mut v = v as u32;
        while v & !0x7F != 0 {
            self.put_u8((v & 0x7F) as u8 | 0x80);
            v >>= 7;
        }
        self.put_u8(v as u8);
    }
    fn put_varlong(&mut self, v: i64) {
        let mut v = v as u64;
        while v & !0x7F != 0 {
            self.put_u8((v & 0x7F) as u8 | 0x80);
            v >>= 7;
        }
        self.put_u8(v as u8);
    }
    fn put_len(&mut self, len: usize) {
        self.put_varint(i32::try_from(len).expect("length fits in a VarInt"));
    }
    fn put_string(&mut self, s: &str) {
        self.put_len(s.len());
        self.put_slice(s.as_bytes());
    }
    fn put_byte_array(&mut self, v: &[u8]) {
        self.put_len(v.len());
        self.put_slice(v);
    }
    /// Angle: a rotation in steps of 1/256 of a full turn.
    fn put_angle(&mut self, degrees: f32) {
        self.put_u8(angle_byte(degrees));
    }
    fn put_position(&mut self, p: Position) {
        self.put_i64(p.pack());
    }
    /// `BitSet`: VarInt count of longs, then the longs.
    fn put_bitset(&mut self, words: &[i64]) {
        self.put_len(words.len());
        for &w in words {
            self.put_i64(w);
        }
    }
    /// Bit set as a VarInt-prefixed byte array, bit `i` in byte `i / 8` (Java's
    /// `BitSet.toByteArray`, trailing zero bytes dropped). 26.3 sends light masks
    /// this way; the documentation still says longs.
    fn put_byte_bitset(&mut self, words: &[u64]) {
        let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        self.put_byte_array(&bytes);
    }
}

impl Write for Vec<u8> {
    fn put_u8(&mut self, v: u8) {
        self.push(v);
    }
    fn put_slice(&mut self, v: &[u8]) {
        self.extend_from_slice(v);
    }
}

/// Network NBT. Encoding only fails for malformed trees (mixed lists), which
/// our own code never builds.
pub fn put_nbt(out: &mut Vec<u8>, tag: &Tag) {
    mc_nbt::write_network(tag, out).expect("well-formed NBT");
}

/// Degrees → protocol Angle byte (wraps around like the vanilla encoding).
pub fn angle_byte(degrees: f32) -> u8 {
    (degrees * 256.0 / 360.0).floor() as i32 as u8
}

pub fn varint_len(v: i32) -> usize {
    let v = v as u32;
    match v {
        0..=0x7F => 1,
        0x80..=0x3FFF => 2,
        0x4000..=0x1F_FFFF => 3,
        0x20_0000..=0xFFF_FFFF => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample values from the protocol documentation's VarInt table.
    const VARINTS: &[(i32, &[u8])] = &[
        (0, &[0x00]),
        (1, &[0x01]),
        (2, &[0x02]),
        (127, &[0x7f]),
        (128, &[0x80, 0x01]),
        (255, &[0xff, 0x01]),
        (25565, &[0xdd, 0xc7, 0x01]),
        (2097151, &[0xff, 0xff, 0x7f]),
        (2147483647, &[0xff, 0xff, 0xff, 0xff, 0x07]),
        (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
        (-2147483648, &[0x80, 0x80, 0x80, 0x80, 0x08]),
    ];

    #[test]
    fn varint_matches_reference_table() {
        for &(value, bytes) in VARINTS {
            let mut out = Vec::new();
            out.put_varint(value);
            assert_eq!(out, bytes, "encode {value}");
            assert_eq!(varint_len(value), bytes.len(), "len {value}");
            assert_eq!(
                Reader::new(bytes).varint().unwrap(),
                value,
                "decode {value}"
            );
        }
    }

    #[test]
    fn varlong_roundtrip() {
        for value in [0, 1, -1, i64::MAX, i64::MIN, 1 << 35] {
            let mut out = Vec::new();
            out.put_varlong(value);
            assert_eq!(Reader::new(&out).varlong().unwrap(), value);
        }
        assert_eq!(
            Reader::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]).varlong(),
            Ok(-1)
        );
    }

    #[test]
    fn varint_too_long() {
        assert_eq!(
            Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x01]).varint(),
            Err(DecodeError::VarIntTooLong)
        );
    }

    #[test]
    fn position_matches_documentation_example() {
        // Example from the protocol docs: x=18357644, z=-20882616, y=831.
        // Digits are grouped as the x (26) / z (26) / y (12) bit fields.
        #[allow(clippy::unusual_byte_groupings)]
        let packed = 0b01000110000001110110001100_10110000010101101101001000_001100111111_i64;
        let pos = Position::unpack(packed);
        assert_eq!(pos, Position::new(18357644, 831, -20882616));
        assert_eq!(pos.pack(), packed);
        for p in [Position::new(-1, -64, -1), Position::new(0, 319, 0)] {
            assert_eq!(Position::unpack(p.pack()), p);
        }
    }

    #[test]
    fn byte_bitset_matches_vanilla_light_masks() {
        // Masks from a vanilla 26.3 flat-world chunk: bits {1, 2}, {}, {0}, {0, 1, 2}.
        let mut out = Vec::new();
        out.put_byte_bitset(&[0b110]);
        out.put_byte_bitset(&[]);
        out.put_byte_bitset(&[0b1]);
        out.put_byte_bitset(&[0b111]);
        assert_eq!(out, [0x01, 0x06, 0x00, 0x01, 0x01, 0x01, 0x07]);
        let mut wide = Vec::new();
        wide.put_byte_bitset(&[0, 1 << 1]);
        assert_eq!(wide, [9, 0, 0, 0, 0, 0, 0, 0, 0, 0x02]);
    }

    #[test]
    fn strings_are_length_checked() {
        let mut out = Vec::new();
        out.put_string("Привет");
        assert_eq!(Reader::new(&out).string(16).unwrap(), "Привет");
        assert_eq!(
            Reader::new(&out).string(5),
            Err(DecodeError::StringTooLong { max_chars: 5 })
        );
    }
}
