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
mod snbt;

pub use mutf8::{decode_mutf8, encode_mutf8};
pub use snbt::{parse_snbt, parse_snbt_prefix};

/// How many lists and compounds may be open at once while decoding, the
/// vanilla limit.
pub const MAX_DEPTH: usize = 512;

/// What a decode may cost before it is refused: a budget in vanilla's
/// estimate of the decoded tags' memory, and the nesting depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of estimated memory the decoded tags may take.
    pub quota: u64,
    /// Lists and compounds that may be open at once.
    pub depth: usize,
}

impl Limits {
    /// NBT read from a connection: vanilla's default quota of 2 MiB.
    pub const NETWORK: Limits = Limits {
        quota: 2 * 1024 * 1024,
        depth: MAX_DEPTH,
    };
    /// Files and other trusted data: depth only.
    pub const TRUSTED: Limits = Limits {
        quota: u64::MAX,
        depth: MAX_DEPTH,
    };
}

/// Vanilla's memory estimate for each tag, the unit of [`Limits::quota`].
mod cost {
    pub const BYTE: u64 = 9;
    pub const SHORT: u64 = 10;
    pub const INT: u64 = 12;
    pub const LONG: u64 = 16;
    pub const FLOAT: u64 = 12;
    pub const DOUBLE: u64 = 16;
    /// An array tag, before its elements.
    pub const ARRAY: u64 = 24;
    /// A string tag, before its two bytes per UTF-16 unit.
    pub const STRING: u64 = 36;
    /// A list, before its four bytes per element slot.
    pub const LIST: u64 = 36;
    pub const COMPOUND: u64 = 48;
    /// A compound key, before its two bytes per UTF-16 unit.
    pub const KEY: u64 = 28;
    /// A new compound entry.
    pub const ENTRY: u64 = 36;
}

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
    pub fn insert<K>(&mut self, key: K, value: impl Into<Tag>) -> &mut Self
    where
        K: AsRef<str> + Into<String>,
    {
        let value = value.into();
        // The key is only copied when it is new.
        match self.0.iter_mut().find(|(k, _)| k == key.as_ref()) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key.into(), value)),
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

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut Tag)> {
        self.0.iter_mut().map(|(k, v)| (k.as_str(), v))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NbtError {
    UnexpectedEof,
    InvalidTagType(u8),
    MixedList {
        expected: u8,
        found: u8,
    },
    NegativeLength(i32),
    TooDeep,
    /// The decoded tags would go over [`Limits::quota`].
    TooBig,
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
            NbtError::TooBig => write!(f, "NBT tag too big for its quota"),
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
    write_payload(tag, out, true)
}

/// Named form: tag type + root name + payload.
pub fn write_named(name: &str, tag: &Tag, out: &mut Vec<u8>) -> Result<()> {
    out.push(tag.type_id());
    write_string(name, out)?;
    write_payload(tag, out, false)
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

/// Compound entries in the order vanilla sends them: its compounds are
/// Java `HashMap`s, so entries go by hash bucket, then as they were added.
/// A compound's entries in the order vanilla lists and sends them.
pub fn java_order(c: &Compound) -> Vec<(&str, &Tag)> {
    java_entries(&c.0)
        .into_iter()
        .map(|(k, v)| (k.as_str(), v))
        .collect()
}

fn java_entries<K: AsRef<str>, V>(entries: &[(K, V)]) -> Vec<&(K, V)> {
    let mut cap = 16;
    while entries.len() * 4 > cap * 3 {
        cap *= 2;
    }
    let bucket = |k: &str| {
        let h = k
            .encode_utf16()
            .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
            as u32;
        ((h ^ (h >> 16)) as usize) & (cap - 1)
    };
    let mut sorted: Vec<&(K, V)> = entries.iter().collect();
    sorted.sort_by_key(|(k, _)| bucket(k.as_ref()));
    sorted
}

fn write_payload(tag: &Tag, out: &mut Vec<u8>, java: bool) -> Result<()> {
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
                write_payload(item, out, java)?;
            }
        }
        Tag::Compound(compound) => {
            let entries: Vec<&(String, Tag)> = if java {
                java_entries(&compound.0)
            } else {
                compound.0.iter().collect()
            };
            for (key, value) in entries {
                out.push(value.type_id());
                write_string(key, out)?;
                write_payload(value, out, java)?;
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

/// Reads a network-form tag under [`Limits::NETWORK`], as the server reads
/// what a client sends. Returns the tag and the number of bytes consumed.
pub fn read_network(bytes: &[u8]) -> Result<(Tag, usize)> {
    read_network_with(bytes, Limits::NETWORK)
}

/// [`read_network`] with other limits.
pub fn read_network_with(bytes: &[u8], limits: Limits) -> Result<(Tag, usize)> {
    let mut reader = Reader::new(bytes, limits);
    let type_id = reader.u8()?;
    if type_id == tag_type::END {
        return Err(NbtError::EndAsRoot);
    }
    let tag = reader.payload(type_id)?;
    Ok((tag, reader.pos))
}

/// Like [`read_network`], but a lone TAG_End (one zero byte) is accepted and
/// means "no data", as the protocol uses it for empty optional NBT.
pub fn read_network_optional(bytes: &[u8]) -> Result<(Option<Tag>, usize)> {
    match bytes.first() {
        Some(&tag_type::END) => Ok((None, 1)),
        _ => read_network(bytes).map(|(tag, used)| (Some(tag), used)),
    }
}

/// Reads a named-form tag (files) under [`Limits::TRUSTED`]. Returns the
/// root name, the tag and the bytes consumed.
pub fn read_named(bytes: &[u8]) -> Result<(String, Tag, usize)> {
    let mut reader = Reader::new(bytes, Limits::TRUSTED);
    let type_id = reader.u8()?;
    if type_id == tag_type::END {
        return Err(NbtError::EndAsRoot);
    }
    let name = reader.string()?;
    let tag = reader.payload(type_id)?;
    Ok((name, tag, reader.pos))
}

/// Compounds with more keys than this look duplicates up in a hash map while
/// decoding, so a hostile compound cannot make decoding quadratic.
const INDEXED_KEYS: usize = 16;

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    limits: Limits,
    /// Quota used so far.
    used: u64,
    /// Lists and compounds open now.
    depth: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], limits: Limits) -> Self {
        Self {
            bytes,
            pos: 0,
            limits,
            used: 0,
            depth: 0,
        }
    }

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

    fn account(&mut self, cost: u64) -> Result<()> {
        match self.used.checked_add(cost) {
            Some(used) if used <= self.limits.quota => {
                self.used = used;
                Ok(())
            }
            _ => Err(NbtError::TooBig),
        }
    }

    /// The cost of a string: two bytes per UTF-16 unit on top of `base`.
    fn account_text(&mut self, base: u64, text: &str) -> Result<()> {
        if self.limits.quota == u64::MAX {
            return Ok(());
        }
        let units = text.encode_utf16().count() as u64;
        self.account(base + units * 2)
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

    /// Opens a list or compound.
    fn push(&mut self) -> Result<()> {
        if self.depth >= self.limits.depth {
            return Err(NbtError::TooDeep);
        }
        self.depth += 1;
        Ok(())
    }

    fn payload(&mut self, type_id: u8) -> Result<Tag> {
        use tag_type::*;
        Ok(match type_id {
            BYTE => {
                self.account(cost::BYTE)?;
                Tag::Byte(self.u8()? as i8)
            }
            SHORT => {
                self.account(cost::SHORT)?;
                Tag::Short(i16::from_be_bytes(self.array()?))
            }
            INT => {
                self.account(cost::INT)?;
                Tag::Int(i32::from_be_bytes(self.array()?))
            }
            LONG => {
                self.account(cost::LONG)?;
                Tag::Long(i64::from_be_bytes(self.array()?))
            }
            FLOAT => {
                self.account(cost::FLOAT)?;
                Tag::Float(f32::from_be_bytes(self.array()?))
            }
            DOUBLE => {
                self.account(cost::DOUBLE)?;
                Tag::Double(f64::from_be_bytes(self.array()?))
            }
            BYTE_ARRAY => {
                self.account(cost::ARRAY)?;
                let len = self.len(1)?;
                self.account(len as u64)?;
                Tag::ByteArray(self.take(len)?.iter().map(|&b| b as i8).collect())
            }
            STRING => {
                let s = self.string()?;
                self.account_text(cost::STRING, &s)?;
                Tag::String(s)
            }
            LIST => {
                self.push()?;
                self.account(cost::LIST)?;
                let element = self.u8()?;
                // An empty list may carry any element type, including END.
                let len = self.len(if element == END { 0 } else { 1 })?;
                if len > 0 && element == END {
                    return Err(NbtError::InvalidTagType(END));
                }
                self.account(4 * len as u64)?;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.payload(element)?);
                }
                self.depth -= 1;
                Tag::List(items)
            }
            COMPOUND => {
                self.push()?;
                self.account(cost::COMPOUND)?;
                let compound = self.compound()?;
                self.depth -= 1;
                Tag::Compound(compound)
            }
            INT_ARRAY => {
                self.account(cost::ARRAY)?;
                let len = self.len(4)?;
                self.account(4 * len as u64)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(i32::from_be_bytes(self.array()?));
                }
                Tag::IntArray(values)
            }
            LONG_ARRAY => {
                self.account(cost::ARRAY)?;
                let len = self.len(8)?;
                self.account(8 * len as u64)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(i64::from_be_bytes(self.array()?));
                }
                Tag::LongArray(values)
            }
            other => return Err(NbtError::InvalidTagType(other)),
        })
    }

    /// Compound entries up to TAG_End. A repeated key replaces the earlier
    /// value in its place.
    fn compound(&mut self) -> Result<Compound> {
        let mut entries: Vec<(String, Tag)> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = Default::default();
        loop {
            let child = self.u8()?;
            if child == tag_type::END {
                return Ok(Compound(entries));
            }
            let key = self.string()?;
            self.account_text(cost::KEY, &key)?;
            let value = self.payload(child)?;
            let earlier = if entries.len() < INDEXED_KEYS {
                entries.iter().position(|(k, _)| *k == key)
            } else {
                if index.is_empty() {
                    index = (entries.iter().enumerate())
                        .map(|(i, (k, _))| (k.clone(), i))
                        .collect();
                }
                index.get(&key).copied()
            };
            match earlier {
                Some(i) => entries[i].1 = value,
                None => {
                    self.account(cost::ENTRY)?;
                    if !index.is_empty() {
                        index.insert(key.clone(), entries.len());
                    }
                    entries.push((key, value));
                }
            }
        }
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
        // Entries come back in vanilla's (hash) order.
        let (back, len) = read_network(&out).unwrap();
        assert_eq!(len, out.len());
        let (Tag::Compound(a), Tag::Compound(b)) = (&root, &back) else {
            panic!("not a compound");
        };
        assert_eq!(a.len(), b.len());
        for (k, v) in a.iter() {
            assert_eq!(b.get(k), Some(v), "{k}");
        }
    }

    #[test]
    fn compounds_are_written_in_java_order() {
        let mut c = Compound::new();
        c.insert("text", "x").insert("color", "red");
        let mut out = Vec::new();
        write_network(&Tag::Compound(c), &mut out).unwrap();
        // As vanilla writes `{"text":"x","color":"red"}`.
        assert_eq!(out[1..9], *b"\x08\x00\x05color");
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

    /// `lists` nested lists, the innermost empty.
    fn nested_lists(lists: usize) -> Vec<u8> {
        let mut bytes = vec![tag_type::LIST];
        for _ in 1..lists {
            bytes.extend_from_slice(&[tag_type::LIST, 0, 0, 0, 1]);
        }
        bytes.extend_from_slice(&[tag_type::END, 0, 0, 0, 0]);
        bytes
    }

    #[test]
    fn depth_is_limited() {
        // Vanilla opens at most 512 lists and compounds at once.
        assert!(read_network(&nested_lists(MAX_DEPTH)).is_ok());
        assert_eq!(
            read_network(&nested_lists(MAX_DEPTH + 1)),
            Err(NbtError::TooDeep)
        );
        let mut named = vec![tag_type::LIST, 0, 0];
        named.extend_from_slice(&nested_lists(MAX_DEPTH + 1)[1..]);
        assert_eq!(read_named(&named), Err(NbtError::TooDeep));
        // Scalars inside the deepest list do not count.
        let mut bytes = nested_lists(MAX_DEPTH);
        let tail = bytes.len() - 5;
        bytes.truncate(tail);
        bytes.extend_from_slice(&[tag_type::INT, 0, 0, 0, 1, 0, 0, 0, 7]);
        assert!(read_network(&bytes).is_ok());
    }

    /// A byte array of `len` bytes as a network root.
    fn byte_array(len: usize) -> Vec<u8> {
        let mut bytes = vec![tag_type::BYTE_ARRAY];
        bytes.extend_from_slice(&(len as i32).to_be_bytes());
        bytes.resize(bytes.len() + len, 0);
        bytes
    }

    #[test]
    fn network_reads_have_vanilla_quota() {
        // 24 bytes for the tag, one per element: exactly 2 MiB fits.
        let quota = Limits::NETWORK.quota as usize;
        assert!(read_network(&byte_array(quota - 24)).is_ok());
        assert_eq!(read_network(&byte_array(quota - 23)), Err(NbtError::TooBig));
        // Files are not limited.
        let mut named = vec![tag_type::BYTE_ARRAY, 0, 0];
        named.extend_from_slice(&byte_array(quota)[1..]);
        assert!(read_named(&named).is_ok());

        // One byte on the wire, 48 in the quota: an empty compound per
        // element is how small input would otherwise blow up in memory.
        let count = 50_000usize;
        let mut bytes = vec![tag_type::LIST, tag_type::COMPOUND];
        bytes.extend_from_slice(&(count as i32).to_be_bytes());
        bytes.resize(bytes.len() + count, tag_type::END);
        assert_eq!(read_network(&bytes), Err(NbtError::TooBig));
        assert!(read_network_with(&bytes, Limits::TRUSTED).is_ok());
    }

    #[test]
    fn many_keys_decode_in_linear_time() {
        let keys = 200_000;
        let mut bytes = vec![tag_type::COMPOUND];
        for i in 0..keys {
            let key = format!("k{i}");
            bytes.push(tag_type::BYTE);
            bytes.extend_from_slice(&(key.len() as u16).to_be_bytes());
            bytes.extend_from_slice(key.as_bytes());
            bytes.push(1);
        }
        // A repeated key keeps its first place and takes the last value.
        bytes.extend_from_slice(&[tag_type::BYTE, 0, 2, b'k', b'7', 9]);
        bytes.push(tag_type::END);
        let started = std::time::Instant::now();
        let (tag, _) = read_network_with(&bytes, Limits::TRUSTED).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let Tag::Compound(c) = tag else { panic!() };
        assert_eq!(c.len(), keys);
        assert_eq!(c.get("k7"), Some(&Tag::Byte(9)));
        assert_eq!(c.iter().nth(7).map(|(k, _)| k), Some("k7"));
    }

    #[test]
    fn truncated_input_errors() {
        for cut in 0..HELLO_WORLD.len() {
            assert!(read_named(&HELLO_WORLD[..cut]).is_err(), "cut at {cut}");
        }
    }
}
