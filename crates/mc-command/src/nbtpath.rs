//! NBT paths (`Items[0].count`, `list[{id:1}]`, `{a:1}`): parsing, with
//! the errors vanilla gives, and reading and changing tags along them.

use mc_nbt::{Compound, Tag};

use crate::CommandError;
use crate::error::builtin;
use crate::reader::Reader;

type Result<T> = std::result::Result<T, CommandError>;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// `name`
    Key(String),
    /// `{...}` at the start: the root, if it matches.
    MatchRoot(Compound),
    /// `name{...}`: the child, if it matches.
    MatchKey(String, Compound),
    /// `[]`
    All,
    /// `[i]` (negative counts from the end)
    Index(i32),
    /// `[{...}]`: the elements that match.
    MatchElement(Compound),
}

#[derive(Clone, Debug, PartialEq)]
pub struct NbtPath {
    /// As typed, for messages.
    pub text: String,
    pub nodes: Vec<Node>,
}

fn unquoted(c: char) -> bool {
    !matches!(c, ' ' | '"' | '\'' | '[' | ']' | '.' | '{' | '}')
}

fn compound(r: &mut Reader) -> Result<Compound> {
    match crate::args::read_nbt(r)? {
        Tag::Compound(c) => Ok(c),
        _ => Err(r.error(builtin::expected("argument.nbt.expected.compound"))),
    }
}

fn invalid(r: &Reader) -> CommandError {
    r.error(builtin::expected("arguments.nbtpath.node.invalid"))
}

fn node(r: &mut Reader, first: bool) -> Result<Node> {
    match r.peek() {
        '{' => {
            if !first {
                return Err(invalid(r));
            }
            Ok(Node::MatchRoot(compound(r)?))
        }
        '[' => {
            r.skip();
            if r.can_read() && r.peek() == '{' {
                let c = compound(r)?;
                r.expect(']')?;
                Ok(Node::MatchElement(c))
            } else if r.can_read() && r.peek() == ']' {
                r.skip();
                Ok(Node::All)
            } else {
                let i = r.read_int()?;
                r.expect(']')?;
                Ok(Node::Index(i))
            }
        }
        '"' | '\'' => {
            let name = r.read_quoted()?;
            object(r, name)
        }
        _ => {
            let start = r.cursor;
            while r.can_read() && unquoted(r.peek()) {
                r.skip();
            }
            if r.cursor == start {
                return Err(invalid(r));
            }
            let name = r.input[start..r.cursor].to_owned();
            object(r, name)
        }
    }
}

fn object(r: &mut Reader, name: String) -> Result<Node> {
    if r.can_read() && r.peek() == '{' {
        Ok(Node::MatchKey(name, compound(r)?))
    } else {
        Ok(Node::Key(name))
    }
}

/// Reads a path up to the next space.
pub fn parse(r: &mut Reader) -> Result<NbtPath> {
    let start = r.cursor;
    let mut nodes = Vec::new();
    let mut first = true;
    while r.can_read() && r.peek() != ' ' {
        nodes.push(node(r, first)?);
        first = false;
        if r.can_read() && !matches!(r.peek(), ' ' | '[' | '{') {
            r.expect('.')?;
        }
    }
    Ok(NbtPath {
        text: r.input[start..r.cursor].to_owned(),
        nodes,
    })
}

/// Whether `value` has everything `pattern` has (lists: each pattern
/// element matches some element).
pub fn matches(pattern: &Tag, value: &Tag) -> bool {
    match (pattern, value) {
        (Tag::Compound(p), Tag::Compound(v)) => p
            .iter()
            .all(|(k, pv)| v.get(k).is_some_and(|vv| matches(pv, vv))),
        (Tag::List(p), Tag::List(v)) => {
            if p.is_empty() {
                v.is_empty()
            } else {
                p.iter().all(|pe| v.iter().any(|ve| matches(pe, ve)))
            }
        }
        _ => pattern == value,
    }
}

fn matches_compound(pattern: &Compound, value: &Tag) -> bool {
    matches(&Tag::Compound(pattern.clone()), value)
}

/// A numeric tag as an `i64`/`f64`, for array elements.
fn number(t: &Tag) -> Option<f64> {
    Some(match t {
        Tag::Byte(v) => f64::from(*v),
        Tag::Short(v) => f64::from(*v),
        Tag::Int(v) => f64::from(*v),
        Tag::Long(v) => *v as f64,
        Tag::Float(v) => f64::from(*v),
        Tag::Double(v) => *v,
        _ => return None,
    })
}

/// A list or array's elements as tags.
fn elements(t: &Tag) -> Option<Vec<Tag>> {
    Some(match t {
        Tag::List(l) => l.clone(),
        Tag::ByteArray(a) => a.iter().map(|v| Tag::Byte(*v)).collect(),
        Tag::IntArray(a) => a.iter().map(|v| Tag::Int(*v)).collect(),
        Tag::LongArray(a) => a.iter().map(|v| Tag::Long(*v)).collect(),
        _ => return None,
    })
}

fn len(t: &Tag) -> Option<usize> {
    Some(match t {
        Tag::List(l) => l.len(),
        Tag::ByteArray(a) => a.len(),
        Tag::IntArray(a) => a.len(),
        Tag::LongArray(a) => a.len(),
        _ => return None,
    })
}

fn index(i: i32, len: usize) -> Option<usize> {
    let i = if i < 0 {
        len as i64 + i64::from(i)
    } else {
        i64::from(i)
    };
    (0..len as i64).contains(&i).then_some(i as usize)
}

/// Puts `value` at `i` of a list or array; whether it changed. Arrays take
/// numbers only.
fn put_at(t: &mut Tag, i: usize, value: &Tag) -> bool {
    match t {
        Tag::List(l) => {
            let changed = l[i] != *value;
            l[i] = value.clone();
            changed
        }
        Tag::ByteArray(a) => number(value).is_some_and(|n| {
            let changed = a[i] != n as i8;
            a[i] = n as i8;
            changed
        }),
        Tag::IntArray(a) => number(value).is_some_and(|n| {
            let changed = a[i] != n as i32;
            a[i] = n as i32;
            changed
        }),
        Tag::LongArray(a) => number(value).is_some_and(|n| {
            let changed = a[i] != n as i64;
            a[i] = n as i64;
            changed
        }),
        _ => false,
    }
}

/// Inserts `value` at `i` of a list or array; whether it went in.
pub fn insert_at(t: &mut Tag, i: usize, value: &Tag) -> bool {
    match t {
        Tag::List(l) => {
            l.insert(i, value.clone());
            true
        }
        Tag::ByteArray(a) => number(value).map(|n| a.insert(i, n as i8)).is_some(),
        Tag::IntArray(a) => number(value).map(|n| a.insert(i, n as i32)).is_some(),
        Tag::LongArray(a) => number(value).map(|n| a.insert(i, n as i64)).is_some(),
        _ => false,
    }
}

fn remove_at(t: &mut Tag, i: usize) {
    match t {
        Tag::List(l) => {
            l.remove(i);
        }
        Tag::ByteArray(a) => {
            a.remove(i);
        }
        Tag::IntArray(a) => {
            a.remove(i);
        }
        Tag::LongArray(a) => {
            a.remove(i);
        }
        _ => {}
    }
}

/// The empty tag a node is found in: a compound for keys, a list for
/// indexes.
fn parent_for(node: &Node) -> Tag {
    match node {
        Node::Key(_) | Node::MatchKey(..) | Node::MatchRoot(_) => Tag::Compound(Compound::new()),
        Node::All | Node::Index(_) | Node::MatchElement(_) => Tag::List(Vec::new()),
    }
}

/// Applies `f` to each tag `node` finds in `t`, making what is missing
/// when `create` gives the tag to make.
fn each_mut(t: &mut Tag, node: &Node, create: Option<&Tag>, f: &mut dyn FnMut(&mut Tag)) {
    match node {
        Node::Key(name) => {
            if let Tag::Compound(c) = t {
                if c.get(name).is_none()
                    && let Some(new) = create
                {
                    c.insert(name.as_str(), new.clone());
                }
                if let Some(child) = c.get_mut(name) {
                    f(child);
                }
            }
        }
        Node::MatchRoot(p) => {
            if matches_compound(p, t) {
                f(t);
            }
        }
        Node::MatchKey(name, p) => {
            if let Tag::Compound(c) = t {
                if c.get(name).is_none() && create.is_some() {
                    c.insert(name.as_str(), Tag::Compound(p.clone()));
                }
                if let Some(child) = c.get_mut(name)
                    && matches_compound(p, child)
                {
                    f(child);
                }
            }
        }
        Node::All => {
            if let Tag::List(l) = t {
                if l.is_empty()
                    && let Some(new) = create
                {
                    l.push(new.clone());
                }
                for e in l.iter_mut() {
                    f(e);
                }
            }
        }
        Node::Index(i) => {
            if let Tag::List(l) = t
                && let Some(i) = index(*i, l.len())
            {
                f(&mut l[i]);
            }
        }
        Node::MatchElement(p) => {
            if let Tag::List(l) = t {
                if create.is_some() && !l.iter().any(|e| matches_compound(p, e)) {
                    l.push(Tag::Compound(p.clone()));
                }
                for e in l.iter_mut().filter(|e| matches_compound(p, e)) {
                    f(e);
                }
            }
        }
    }
}

fn found(t: &Tag, node: &Node) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut copy = t.clone();
    // Arrays index as lists do when read.
    if let (Node::Index(_) | Node::All, Some(e)) = (node, elements(t))
        && !matches!(t, Tag::List(_))
    {
        copy = Tag::List(e);
    }
    each_mut(&mut copy, node, None, &mut |x| out.push(x.clone()));
    out
}

fn nothing(path: &NbtPath) -> CommandError {
    CommandError::new(
        "arguments.nbtpath.nothing_found",
        vec![Tag::String(path.text.clone())],
    )
}

impl NbtPath {
    /// The tags the path finds; vanilla's error when none.
    pub fn get(&self, root: &Tag) -> Result<Vec<Tag>> {
        let mut now = vec![root.clone()];
        for node in &self.nodes {
            now = now.iter().flat_map(|t| found(t, node)).collect();
            if now.is_empty() {
                return Err(nothing(self));
            }
        }
        Ok(now)
    }

    /// Runs `f` on each tag the path's parents find (made where missing),
    /// with the last node; vanilla's error when there are none.
    fn parents(
        &self,
        root: &mut Tag,
        create: bool,
        f: &mut dyn FnMut(&mut Tag, &Node),
    ) -> Result<()> {
        let (last, parents) = self.nodes.split_last().ok_or_else(|| nothing(self))?;
        let mut hit = false;
        fn walk(
            t: &mut Tag,
            nodes: &[Node],
            last: &Node,
            create: bool,
            hit: &mut bool,
            f: &mut dyn FnMut(&mut Tag, &Node),
        ) {
            let Some((node, rest)) = nodes.split_first() else {
                *hit = true;
                f(t, last);
                return;
            };
            let next = rest.first().unwrap_or(last);
            let made = parent_for(next);
            each_mut(t, node, create.then_some(&made), &mut |child| {
                walk(child, rest, last, create, hit, f)
            });
        }
        walk(root, parents, last, create, &mut hit, f);
        if hit { Ok(()) } else { Err(nothing(self)) }
    }

    /// Sets what the path finds to `value`, making missing parents; how
    /// many changed.
    pub fn set(&self, root: &mut Tag, value: &Tag) -> Result<i32> {
        let mut changed = 0;
        self.parents(root, true, &mut |parent, last| {
            changed += set_in(parent, last, value);
        })?;
        Ok(changed)
    }

    /// The tags at the path, made (as `make`) where missing.
    pub fn get_or_create(
        &self,
        root: &mut Tag,
        make: &Tag,
        f: &mut dyn FnMut(&mut Tag),
    ) -> Result<()> {
        let mut any = false;
        self.parents(root, true, &mut |parent, last| {
            each_mut(parent, last, Some(make), &mut |t| {
                any = true;
                f(t);
            });
        })?;
        if any { Ok(()) } else { Err(nothing(self)) }
    }

    /// Removes what the path finds; how many went.
    pub fn remove(&self, root: &mut Tag) -> Result<i32> {
        let mut removed = 0;
        self.parents(root, false, &mut |parent, last| {
            removed += remove_in(parent, last);
        })?;
        Ok(removed)
    }
}

fn set_in(parent: &mut Tag, node: &Node, value: &Tag) -> i32 {
    match node {
        Node::Key(name) => match parent {
            Tag::Compound(c) => {
                let changed = c.get(name) != Some(value);
                c.insert(name.as_str(), value.clone());
                i32::from(changed)
            }
            _ => 0,
        },
        Node::MatchKey(name, p) => match parent {
            Tag::Compound(c) => match c.get(name) {
                Some(old) if matches_compound(p, old) => {
                    let changed = old != value;
                    c.insert(name.as_str(), value.clone());
                    i32::from(changed)
                }
                _ => 0,
            },
            _ => 0,
        },
        Node::MatchRoot(_) => 0,
        Node::Index(i) => match len(parent).and_then(|n| index(*i, n)) {
            Some(i) => i32::from(put_at(parent, i, value)),
            None => 0,
        },
        Node::All => {
            let n = len(parent).unwrap_or(0);
            if n == 0 {
                return i32::from(insert_at(parent, 0, value));
            }
            (0..n).map(|i| i32::from(put_at(parent, i, value))).sum()
        }
        Node::MatchElement(p) => match parent {
            Tag::List(l) => {
                let mut changed = 0;
                for e in l.iter_mut().filter(|e| matches_compound(p, e)) {
                    if e != value {
                        *e = value.clone();
                        changed += 1;
                    }
                }
                changed
            }
            _ => 0,
        },
    }
}

fn remove_in(parent: &mut Tag, node: &Node) -> i32 {
    match node {
        Node::Key(name) => match parent {
            Tag::Compound(c) => i32::from(c.remove(name).is_some()),
            _ => 0,
        },
        Node::MatchKey(name, p) => match parent {
            Tag::Compound(c) if c.get(name).is_some_and(|v| matches_compound(p, v)) => {
                c.remove(name);
                1
            }
            _ => 0,
        },
        Node::MatchRoot(_) => 0,
        Node::Index(i) => match len(parent).and_then(|n| index(*i, n)) {
            Some(i) => {
                remove_at(parent, i);
                1
            }
            None => 0,
        },
        Node::All => {
            let n = len(parent).unwrap_or(0);
            for _ in 0..n {
                remove_at(parent, 0);
            }
            n as i32
        }
        Node::MatchElement(p) => match parent {
            Tag::List(l) => {
                let before = l.len();
                l.retain(|e| !matches_compound(p, e));
                (before - l.len()) as i32
            }
            _ => 0,
        },
    }
}

/// Merges `from` into `into`: compounds merge key by key, anything else
/// is replaced.
pub fn merge(into: &mut Compound, from: &Compound) {
    for (k, v) in from.iter() {
        match (into.get_mut(k), v) {
            (Some(Tag::Compound(old)), Tag::Compound(new)) => merge(old, new),
            _ => {
                into.insert(k, v.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> NbtPath {
        parse(&mut Reader::new(s)).unwrap()
    }

    #[test]
    fn reads_and_writes_as_vanilla() {
        let mut root = mc_nbt::parse_snbt("{list:[1,2,3],objs:[{id:1,v:a},{id:2,v:b}]}").unwrap();
        assert_eq!(path("list[-1]").get(&root).unwrap(), [Tag::Int(3)]);
        assert_eq!(path("objs[{id:2}].v").get(&root).unwrap(), [Tag::from("b")]);
        assert!(path("list[10]").get(&root).is_err());
        assert_eq!(
            path("deep.new.path").set(&mut root, &Tag::Int(1)).unwrap(),
            1
        );
        assert_eq!(path("deep.new.path").get(&root).unwrap(), [Tag::Int(1)]);
        assert_eq!(path("list[]").remove(&mut root).unwrap(), 3);
        assert!(parse(&mut Reader::new("a..b")).is_err());
        assert_eq!(path("obj.").nodes, [Node::Key("obj".into())]);
    }
}
