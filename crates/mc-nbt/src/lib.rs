//! NBT (Named Binary Tag) values and their Java Edition binary encoding.
//!
//! Two root forms are supported:
//! - **named** (files, region chunks): tag type, root name, payload;
//! - **network** (protocol since 1.20.2): tag type and payload, no root name.
//!
//! Strings use Java's modified UTF-8 (`\0` as two bytes, supplementary characters as
//! surrogate pairs). A streaming reader/writer for chunk storage comes later (ROADMAP Ф2).

use std::fmt;

mod mutf8;

pub use mutf8::{decode_mutf8, encode_mutf8};

/// Maximum nesting depth accepted when decoding, same as the vanilla limit.
pub const MAX_DEPTH: usize = 512;

/// Numeric tag type ids as they appear on the wire.
pub mod tag_type {
    pub const END: u8 = 0;
    pub const BYTE: u8 = 1;
    pub const SHORT: u8 = 2;
    pub const INT: u8 = 3;
    pub const LONG: u8 = 4;
    pub const FLOAT: u8 = 5;
    pub const DOUBLE: u8 = 6;
    pub const BYTE_ARRAY: u8 = 7;
    pub const STRING: u8 = 8;
    pub const LIST: u8 = 9;
    pub const COMPOUND: u8 = 10;
    pub const INT_ARRAY: u8 = 11;
    pub const LONG_ARRAY: u8 = 12;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    /// All elements must have the same tag type.
    List(Vec<Tag>),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn type_id(&self) -> u8 {
        use tag_type::*;
        match self {
            Tag::Byte(_) => BYTE,
            Tag::Short(_) => SHORT,
            Tag::Int(_) => INT,
            Tag::Long(_) => LONG,
            Tag::Float(_) => FLOAT,
            Tag::Double(_) => DOUBLE,
            Tag::ByteArray(_) => BYTE_ARRAY,
            Tag::String(_) => STRING,
            Tag::List(_) => LIST,
            Tag::Compound(_) => COMPOUND,
            Tag::IntArray(_) => INT_ARRAY,
            Tag::LongArray(_) => LONG_ARRAY,
        }
    }
}

impl From<Compound> for Tag {
    fn from(compound: Compound) -> Self {
        Tag::Compound(compound)
    }
}

impl From<&str> for Tag {
    fn from(value: &str) -> Self {
        Tag::String(value.to_owned())
    }
}

/// A compound tag. Keeps insertion order, so encoding is deterministic.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Compound(Vec<(String, Tag)>);

impl Compound {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces `key`.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Tag>) -> &mut Self {
        let key = key.into();
        let value = value.into();
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
        self
    }

    pub fn get(&self, key: &str) -> Option<&Tag> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Tag> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn remove(&mut self, key: &str) -> Option<Tag> {
        let index = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(index).1)
    }

    /// Keeps the entries for which `keep(key, value)` is true.
    pub fn retain(&mut self, mut keep: impl FnMut(&str, &Tag) -> bool) {
        self.0.retain(|(k, v)| keep(k, v));
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Tag)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NbtError {
    UnexpectedEof,
    InvalidTagType(u8),
    MixedList { expected: u8, found: u8 },
    NegativeLength(i32),
    TooDeep,
    StringTooLong(usize),
    InvalidMutf8,
    EndAsRoot,
}

impl fmt::Display for NbtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NbtError::UnexpectedEof => write!(f, "unexpected end of NBT data"),
            NbtError::InvalidTagType(t) => write!(f, "invalid NBT tag type {t}"),
            NbtError::MixedList { expected, found } => {
                write!(f, "list of tag type {expected} contains tag type {found}")
            }
            NbtError::NegativeLength(n) => write!(f, "negative NBT length {n}"),
            NbtError::TooDeep => write!(f, "NBT nested deeper than {MAX_DEPTH}"),
            NbtError::StringTooLong(n) => write!(f, "string of {n} bytes does not fit in NBT"),
            NbtError::InvalidMutf8 => write!(f, "invalid modified UTF-8 in NBT string"),
            NbtError::EndAsRoot => write!(f, "TAG_End cannot be a root tag"),
        }
    }
}

impl std::error::Error for NbtError {}

pub type Result<T> = std::result::Result<T, NbtError>;

// ---------------------------------------------------------------------------
// Encoding

/// Network form: tag type + payload, no root name.
pub fn write_network(tag: &Tag, out: &mut Vec<u8>) -> Result<()> {
    out.push(tag.type_id());
    write_payload(tag, out)
}

/// Named form: tag type + root name + payload.
pub fn write_named(name: &str, tag: &Tag, out: &mut Vec<u8>) -> Result<()> {
    out.push(tag.type_id());
    write_string(name, out)?;
    write_payload(tag, out)
}

fn write_string(s: &str, out: &mut Vec<u8>) -> Result<()> {
    let bytes = encode_mutf8(s);
    let len = u16::try_from(bytes.len()).map_err(|_| NbtError::StringTooLong(bytes.len()))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&bytes);
    Ok(())
}

fn write_len(len: usize, out: &mut Vec<u8>) {
    // Lengths above i32::MAX cannot exist in memory-sized arrays we produce.
    out.extend_from_slice(&(len as i32).to_be_bytes());
}

fn write_payload(tag: &Tag, out: &mut Vec<u8>) -> Result<()> {
    match tag {
        Tag::Byte(v) => out.push(*v as u8),
        Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::ByteArray(values) => {
            write_len(values.len(), out);
            out.extend(values.iter().map(|&v| v as u8));
        }
        Tag::String(s) => write_string(s, out)?,
        Tag::List(items) => {
            let element = items.first().map_or(tag_type::END, Tag::type_id);
            out.push(element);
            write_len(items.len(), out);
            for item in items {
                if item.type_id() != element {
                    return Err(NbtError::MixedList {
                        expected: element,
                        found: item.type_id(),
                    });
                }
                write_payload(item, out)?;
            }
        }
        Tag::Compound(compound) => {
            for (key, value) in &compound.0 {
                out.push(value.type_id());
                write_string(key, out)?;
                write_payload(value, out)?;
            }
            out.push(tag_type::END);
        }
        Tag::IntArray(values) => {
            write_len(values.len(), out);
            for v in values {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        Tag::LongArray(values) => {
            write_len(values.len(), out);
            for v in values {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Decoding

/// Reads a network-form tag. Returns the tag and the number of bytes consumed.
pub fn read_network(bytes: &[u8]) -> Result<(Tag, usize)> {
    let mut reader = Reader { bytes, pos: 0 };
    let type_id = reader.u8()?;
    if type_id == tag_type::END {
        return Err(NbtError::EndAsRoot);
    }
    let tag = reader.payload(type_id, 0)?;
    Ok((tag, reader.pos))
}

/// Reads a named-form tag. Returns the root name, the tag and the bytes consumed.
pub fn read_named(bytes: &[u8]) -> Result<(String, Tag, usize)> {
    let mut reader = Reader { bytes, pos: 0 };
    let type_id = reader.u8()?;
    if type_id == tag_type::END {
        return Err(NbtError::EndAsRoot);
    }
    let name = reader.string()?;
    let tag = reader.payload(type_id, 0)?;
    Ok((name, tag, reader.pos))
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(NbtError::UnexpectedEof)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(NbtError::UnexpectedEof)?;
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("take returned N bytes"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    /// A length prefix, checked against the remaining input so a hostile length
    /// cannot trigger a huge allocation.
    fn len(&mut self, element_size: usize) -> Result<usize> {
        let len = i32::from_be_bytes(self.array()?);
        let len = usize::try_from(len).map_err(|_| NbtError::NegativeLength(len))?;
        if len.saturating_mul(element_size) > self.bytes.len() - self.pos {
            return Err(NbtError::UnexpectedEof);
        }
        Ok(len)
    }

    fn string(&mut self) -> Result<String> {
        let len = u16::from_be_bytes(self.array()?) as usize;
        decode_mutf8(self.take(len)?).ok_or(NbtError::InvalidMutf8)
    }

    fn payload(&mut self, type_id: u8, depth: usize) -> Result<Tag> {
        use tag_type::*;
        if depth > MAX_DEPTH {
            return Err(NbtError::TooDeep);
        }
        Ok(match type_id {
            BYTE => Tag::Byte(self.u8()? as i8),
            SHORT => Tag::Short(i16::from_be_bytes(self.array()?)),
            INT => Tag::Int(i32::from_be_bytes(self.array()?)),
            LONG => Tag::Long(i64::from_be_bytes(self.array()?)),
            FLOAT => Tag::Float(f32::from_be_bytes(self.array()?)),
            DOUBLE => Tag::Double(f64::from_be_bytes(self.array()?)),
            BYTE_ARRAY => {
                let len = self.len(1)?;
                Tag::ByteArray(self.take(len)?.iter().map(|&b| b as i8).collect())
            }
            STRING => Tag::String(self.string()?),
            LIST => {
                let element = self.u8()?;
                // An empty list may carry any element type, including END.
                let len = self.len(if element == END { 0 } else { 1 })?;
                if len > 0 && element == END {
                    return Err(NbtError::InvalidTagType(END));
                }
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.payload(element, depth + 1)?);
                }
                Tag::List(items)
            }
            COMPOUND => {
                let mut compound = Compound::new();
                loop {
                    let child = self.u8()?;
                    if child == END {
                        break;
                    }
                    let key = self.string()?;
                    let value = self.payload(child, depth + 1)?;
                    compound.insert(key, value);
                }
                Tag::Compound(compound)
            }
            INT_ARRAY => {
                let len = self.len(4)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(i32::from_be_bytes(self.array()?));
                }
                Tag::IntArray(values)
            }
            LONG_ARRAY => {
                let len = self.len(8)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(i64::from_be_bytes(self.array()?));
                }
                Tag::LongArray(values)
            }
            other => return Err(NbtError::InvalidTagType(other)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classic `hello_world.nbt` example from the original NBT specification.
    const HELLO_WORLD: &[u8] = b"\x0a\x00\x0bhello world\x08\x00\x04name\x00\x09Bananrama\x00";

    #[test]
    fn hello_world_roundtrip() {
        let (name, tag, used) = read_named(HELLO_WORLD).unwrap();
        assert_eq!(name, "hello world");
        assert_eq!(used, HELLO_WORLD.len());
        let mut expected = Compound::new();
        expected.insert("name", "Bananrama");
        assert_eq!(tag, Tag::Compound(expected));

        let mut out = Vec::new();
        write_named(&name, &tag, &mut out).unwrap();
        assert_eq!(out, HELLO_WORLD);
    }

    #[test]
    fn network_string_root() {
        let mut out = Vec::new();
        write_network(&Tag::from("hi"), &mut out).unwrap();
        assert_eq!(out, b"\x08\x00\x02hi");
        assert_eq!(read_network(&out).unwrap(), (Tag::from("hi"), out.len()));
    }

    #[test]
    fn every_type_roundtrips() {
        let mut inner = Compound::new();
        inner.insert("x", Tag::Int(-5));
        let mut root = Compound::new();
        root.insert("byte", Tag::Byte(-1))
            .insert("short", Tag::Short(-300))
            .insert("int", Tag::Int(i32::MIN))
            .insert("long", Tag::Long(i64::MAX))
            .insert("float", Tag::Float(1.5))
            .insert("double", Tag::Double(-0.25))
            .insert("bytes", Tag::ByteArray(vec![1, -2, 3]))
            .insert("string", "héllo \0 😀")
            .insert("list", Tag::List(vec![Tag::Short(1), Tag::Short(2)]))
            .insert("empty_list", Tag::List(vec![]))
            .insert("nested", inner)
            .insert("ints", Tag::IntArray(vec![7, -7]))
            .insert("longs", Tag::LongArray(vec![1 << 40, -1]));
        let root = Tag::Compound(root);

        let mut out = Vec::new();
        write_network(&root, &mut out).unwrap();
        assert_eq!(read_network(&out).unwrap(), (root, out.len()));
    }

    #[test]
    fn mixed_list_is_rejected() {
        let list = Tag::List(vec![Tag::Int(1), Tag::Byte(2)]);
        assert!(matches!(
            write_network(&list, &mut Vec::new()),
            Err(NbtError::MixedList { .. })
        ));
    }

    #[test]
    fn hostile_lengths_do_not_allocate() {
        // Int array claiming 2^31-1 elements with no data behind it.
        let bytes = b"\x0b\x7f\xff\xff\xff";
        assert_eq!(read_network(bytes), Err(NbtError::UnexpectedEof));
        assert_eq!(
            read_network(b"\x07\xff\xff\xff\xff"),
            Err(NbtError::NegativeLength(-1))
        );
    }

    #[test]
    fn depth_is_limited() {
        // Root list, then MAX_DEPTH + 2 levels of "list of one list", then an empty list.
        let mut bytes = vec![tag_type::LIST];
        for _ in 0..MAX_DEPTH + 2 {
            bytes.extend_from_slice(&[tag_type::LIST, 0, 0, 0, 1]);
        }
        bytes.extend_from_slice(&[tag_type::END, 0, 0, 0, 0]);
        assert_eq!(read_network(&bytes), Err(NbtError::TooDeep));
    }

    #[test]
    fn truncated_input_errors() {
        for cut in 0..HELLO_WORLD.len() {
            assert!(read_named(&HELLO_WORLD[..cut]).is_err(), "cut at {cut}");
        }
    }
}
