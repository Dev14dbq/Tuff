//! Argument types (`brigadier:*` and `minecraft:*` parsers), read from the
//! command line as the game reads them, with its error messages.
//!
//! Types that commands here do not look inside yet are read as raw text
//! up to the next space outside brackets and quotes.

use mc_nbt::Tag;
use serde_json::Value;

use crate::error::{CommandError, builtin};
use crate::reader::{Reader, Result, is_unquoted};
use crate::text::{int, string};

/// What the arguments are checked against: the game's registries.
pub trait Env {
    /// Whether `id` (`minecraft:stone`) is in `registry` (`minecraft:block`).
    fn exists(&self, registry: &str, id: &str) -> bool;
    /// Whether the tag (without `#`) exists in `registry`.
    fn tag_exists(&self, registry: &str, tag: &str) -> bool;
    /// The properties of a block and their values; `None` for no such block.
    fn block_properties(&self, block: &str) -> Option<Vec<(String, Vec<String>)>>;
}

/// An environment that accepts everything (for tests and the client-side
/// shape of a line).
#[derive(Debug, Default)]
pub struct AnyEnv;

impl Env for AnyEnv {
    fn exists(&self, _: &str, _: &str) -> bool {
        true
    }
    fn tag_exists(&self, _: &str, _: &str) -> bool {
        true
    }
    fn block_properties(&self, _: &str) -> Option<Vec<(String, Vec<String>)>> {
        Some(Vec::new())
    }
}

/// One coordinate: `5`, `~2` (relative) or `^1` (local).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coord {
    pub value: f64,
    pub relative: bool,
    /// 0.5 was added to a whole number (block middle) by the parser.
    pub centered: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pos {
    World([Coord; 3]),
    Local([f64; 3]),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Range {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl Range {
    pub fn contains(&self, v: f64) -> bool {
        self.min.is_none_or(|m| v >= m) && self.max.is_none_or(|m| v <= m)
    }
}

/// A target selector or a player name / UUID.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Selector {
    /// `p`, `a`, `r`, `s`, `e` or `n`; `None` for a name or UUID.
    pub kind: Option<char>,
    pub player: Option<String>,
    pub uuid: Option<u128>,
    pub limit: Option<i32>,
    pub sort: Option<String>,
    /// (negated, value) filters.
    pub types: Vec<(bool, String)>,
    pub names: Vec<(bool, String)>,
    pub tags: Vec<(bool, String)>,
    pub teams: Vec<(bool, String)>,
    pub gamemodes: Vec<(bool, String)>,
    pub predicates: Vec<(bool, String)>,
    pub nbt: Vec<(bool, Tag)>,
    pub distance: Option<Range>,
    pub level: Option<Range>,
    pub x_rotation: Option<Range>,
    pub y_rotation: Option<Range>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub dx: Option<f64>,
    pub dy: Option<f64>,
    pub dz: Option<f64>,
    pub scores: Vec<(String, Range)>,
    pub advancements: Option<String>,
}

impl Selector {
    /// Whether it can only ever mean one entity.
    pub fn single(&self) -> bool {
        match self.kind {
            None => true,
            Some('s' | 'p' | 'r' | 'n') => {
                self.limit.is_none_or(|l| l <= 1) || self.kind == Some('s')
            }
            _ => self.limit == Some(1),
        }
    }

    /// Whether it can only mean players.
    pub fn players_only(&self) -> bool {
        match self.kind {
            // The source itself is allowed even where only players are.
            None | Some('p' | 'a' | 'r' | 's') => true,
            _ => self
                .types
                .iter()
                .any(|(neg, t)| !neg && (t == "minecraft:player" || t == "player")),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub id: String,
    pub tag: bool,
    pub properties: Vec<(String, String)>,
    pub nbt: Option<Tag>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub tag: bool,
    /// The `[...]` part as written (components or predicates).
    pub components: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Bool(bool),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Str(String),
    Entity(Box<Selector>),
    Pos(Pos),
    /// Two coordinates (x z, or yaw pitch).
    Pos2([Coord; 2]),
    Id(String),
    /// An id or (`#`) a tag.
    IdOrTag {
        id: String,
        tag: bool,
    },
    Block(Block),
    Item(Item),
    Nbt(Tag),
    /// An NBT path.
    NbtPath(crate::nbtpath::NbtPath),
    /// A particle type and its options.
    Particle {
        id: String,
        options: mc_nbt::Compound,
    },
    Range(Range),
    Time(i32),
    /// Text as written (messages, paths, providers...).
    Raw(String),
}

/// The slots a slot range (`container.*`, `armor.head`...) names, by
/// vanilla's slot numbers: 0-53 container, 98-99 hands, 100-103 armor,
/// 105 body, 200-226 ender chest, 300-307 villager, 400 saddle, 401 chest,
/// 499 cursor, 500-514 horse, 1000 contents.
pub fn slot_range(name: &str) -> Option<Vec<i32>> {
    let ranges: &[(&str, i32, i32)] = &[
        ("container", 0, 54),
        ("hotbar", 0, 9),
        ("inventory", 9, 27),
        ("enderchest", 200, 27),
        ("horse", 500, 15),
        ("player.crafting", 500, 4),
    ];
    let single: &[(&str, i32)] = &[
        ("weapon", 98),
        ("weapon.mainhand", 98),
        ("weapon.offhand", 99),
        ("armor.feet", 100),
        ("armor.legs", 101),
        ("armor.chest", 102),
        ("armor.head", 103),
        ("armor.body", 105),
        ("saddle", 400),
        ("horse.chest", 401),
        ("player.cursor", 499),
        ("contents", 1000),
    ];
    if let Some((_, n)) = single.iter().find(|(k, _)| *k == name) {
        return Some(vec![*n]);
    }
    match name {
        "weapon.*" => return Some(vec![98, 99]),
        "armor.*" => return Some(vec![100, 101, 102, 103, 105]),
        _ => {}
    }
    let (base, index) = name.rsplit_once('.')?;
    let &(_, first, len) = ranges.iter().find(|(k, _, _)| *k == base)?;
    if index == "*" {
        return Some((first..first + len).collect());
    }
    let i: i32 = index.parse().ok().filter(|i: &i32| (0..len).contains(i))?;
    if index.starts_with('+') || (index.len() > 1 && index.starts_with('0')) {
        return None;
    }
    Some(vec![first + i])
}

/// `namespace:path` with `minecraft:` added when there is no namespace.
pub fn full_id(s: &str) -> String {
    if s.contains(':') {
        s.to_owned()
    } else {
        format!("minecraft:{s}")
    }
}

fn id_char(c: char) -> bool {
    c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | ':' | '/' | '.' | '-')
}

fn valid_id(s: &str) -> bool {
    let (ns, path) = s.split_once(':').unwrap_or(("minecraft", s));
    !path.contains(':')
        && ns
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | '.' | '-'))
        && path.chars().all(|c| {
            c.is_ascii_digit() || c.is_ascii_lowercase() || matches!(c, '_' | '/' | '.' | '-')
        })
}

/// A resource location.
pub fn read_id(r: &mut Reader) -> Result<String> {
    let start = r.cursor;
    while r.can_read() && id_char(r.peek()) {
        r.skip();
    }
    let s = &r.input[start..r.cursor];
    if !valid_id(s) {
        r.cursor = start;
        return Err(r.error(builtin::expected("argument.id.invalid")));
    }
    Ok(full_id(s))
}

/// Text up to the next space outside brackets and quotes.
pub fn read_raw<'a>(r: &mut Reader<'a>) -> &'a str {
    let start = r.cursor;
    let (mut depth, mut quote) = (0i32, None::<char>);
    while r.can_read() {
        let c = r.peek();
        match quote {
            Some(q) if c == '\\' => {
                let _ = q;
                r.skip();
            }
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '{' | '[' | '(' => depth += 1,
                '}' | ']' | ')' => depth -= 1,
                ' ' if depth <= 0 => break,
                _ => {}
            },
        }
        r.skip();
    }
    &r.input[start..r.cursor]
}

fn num_bounds<T: PartialOrd + Copy>(
    r: &mut Reader,
    start: usize,
    v: T,
    min: Option<T>,
    max: Option<T>,
    kind: &str,
    tag: impl Fn(T) -> Tag,
) -> Result<T> {
    if let Some(m) = min
        && v < m
    {
        r.cursor = start;
        return Err(r.error(builtin::bound(
            &format!("argument.{kind}.low"),
            tag(m),
            tag(v),
        )));
    }
    if let Some(m) = max
        && v > m
    {
        r.cursor = start;
        return Err(r.error(builtin::bound(
            &format!("argument.{kind}.big"),
            tag(m),
            tag(v),
        )));
    }
    Ok(v)
}

fn coord(r: &mut Reader, int_only: bool, center: bool) -> Result<Coord> {
    if r.can_read() && r.peek() == '^' {
        return Err(r.error(builtin::expected("argument.pos.mixed")));
    }
    if !r.can_read() {
        let key = if int_only {
            "argument.pos.missing.int"
        } else {
            "argument.pos.missing.double"
        };
        return Err(r.error(builtin::expected(key)));
    }
    let relative = r.peek() == '~';
    if relative {
        r.skip();
    }
    if r.can_read() && r.peek() != ' ' {
        let start = r.cursor;
        let value = if relative || !int_only {
            r.read_double()?
        } else {
            f64::from(r.read_int()?)
        };
        let written = &r.input[start..r.cursor];
        let centered = !relative && center && !int_only && !written.contains('.');
        let value = if centered { value + 0.5 } else { value };
        return Ok(Coord {
            value,
            relative,
            centered,
        });
    }
    Ok(Coord {
        value: 0.0,
        relative,
        centered: false,
    })
}

fn local(r: &mut Reader) -> Result<f64> {
    if !r.can_read() {
        return Err(r.error(builtin::expected("argument.pos.missing.double")));
    }
    if r.peek() != '^' {
        return Err(r.error(builtin::expected("argument.pos.mixed")));
    }
    r.skip();
    if r.can_read() && r.peek() != ' ' {
        r.read_double()
    } else {
        Ok(0.0)
    }
}

fn pos3(r: &mut Reader, int_only: bool, center: bool) -> Result<Pos> {
    let start = r.cursor;
    let incomplete = |r: &mut Reader| {
        r.cursor = start;
        Err(r.error(builtin::expected("argument.pos3d.incomplete")))
    };
    if r.can_read() && r.peek() == '^' {
        let x = local(r)?;
        if !r.can_read() || r.peek() != ' ' {
            return incomplete(r);
        }
        r.skip();
        let y = local(r)?;
        if !r.can_read() || r.peek() != ' ' {
            return incomplete(r);
        }
        r.skip();
        let z = local(r)?;
        return Ok(Pos::Local([x, y, z]));
    }
    let x = coord(r, int_only, center)?;
    if !r.can_read() || r.peek() != ' ' {
        return incomplete(r);
    }
    r.skip();
    let y = coord(r, int_only, false)?;
    if !r.can_read() || r.peek() != ' ' {
        return incomplete(r);
    }
    r.skip();
    let z = coord(r, int_only, center)?;
    Ok(Pos::World([x, y, z]))
}

fn pos2(r: &mut Reader, int_only: bool, center: bool, key: &str) -> Result<[Coord; 2]> {
    let start = r.cursor;
    if !r.can_read() {
        let k = if int_only {
            "argument.pos.missing.int"
        } else {
            "argument.pos.missing.double"
        };
        return Err(r.error(builtin::expected(k)));
    }
    let a = coord(r, int_only, center)?;
    if !r.can_read() || r.peek() != ' ' {
        r.cursor = start;
        return Err(r.error(builtin::expected(key)));
    }
    r.skip();
    let b = coord(r, int_only, center)?;
    Ok([a, b])
}

/// `1`, `1..5`, `..5`, `1..`.
pub fn read_range(r: &mut Reader, ints: bool) -> Result<Range> {
    let start = r.cursor;
    let number = |r: &mut Reader| -> Result<Option<f64>> {
        let s = r.cursor;
        while r.can_read()
            && (r.peek().is_ascii_digit()
                || r.peek() == '-'
                || (r.peek() == '.' && r.peek_at(1) != '.'))
        {
            r.skip();
        }
        let t = &r.input[s..r.cursor];
        if t.is_empty() {
            return Ok(None);
        }
        if ints && t.contains('.') {
            r.cursor = s;
            return Err(r.error(builtin::expected("argument.range.ints")));
        }
        t.parse::<f64>().map(Some).map_err(|_| {
            r.cursor = s;
            let key = if ints {
                "parsing.int.invalid"
            } else {
                "parsing.double.invalid"
            };
            r.error(builtin::invalid(key, t))
        })
    };
    if !r.can_read() {
        return Err(r.error(builtin::expected("argument.range.empty")));
    }
    let min = number(r)?;
    let max = if r.can_read_n(2) && r.remaining().starts_with("..") {
        r.skip();
        r.skip();
        number(r)?
    } else {
        min
    };
    if min.is_none() && max.is_none() {
        r.cursor = start;
        return Err(r.error(builtin::expected("argument.range.empty")));
    }
    if let (Some(a), Some(b)) = (min, max)
        && a > b
    {
        r.cursor = start;
        return Err(r.error(builtin::expected("argument.range.swapped")));
    }
    Ok(Range { min, max })
}

/// An SNBT value.
pub fn read_nbt(r: &mut Reader) -> Result<Tag> {
    let start = r.cursor;
    match mc_nbt::parse_snbt_prefix(r.remaining()) {
        Ok((tag, used)) => {
            r.cursor += used;
            Ok(tag)
        }
        Err(_) => {
            r.cursor = start;
            Err(r.error(builtin::expected("argument.nbt.expected.value")))
        }
    }
}

fn negated(r: &mut Reader) -> bool {
    r.skip_whitespace();
    if r.can_read() && r.peek() == '!' {
        r.skip();
        r.skip_whitespace();
        true
    } else {
        false
    }
}

const OPTIONS: &[&str] = &[
    "name",
    "distance",
    "level",
    "x",
    "y",
    "z",
    "dx",
    "dy",
    "dz",
    "x_rotation",
    "y_rotation",
    "limit",
    "sort",
    "gamemode",
    "team",
    "type",
    "tag",
    "nbt",
    "scores",
    "advancements",
    "predicate",
];

fn selector_options(r: &mut Reader, s: &mut Selector, env: &dyn Env) -> Result<()> {
    r.skip();
    r.skip_whitespace();
    while r.can_read() && r.peek() != ']' {
        r.skip_whitespace();
        let start = r.cursor;
        let name = r.read_string()?;
        if !OPTIONS.contains(&name.as_str()) {
            r.cursor = start;
            return Err(r.error(builtin::invalid("argument.entity.options.unknown", &name)));
        }
        // Once a team is required, no other team test may follow.
        if name == "team" && s.teams.iter().any(|(neg, _)| !neg) {
            r.cursor = start;
            return Err(r.error(builtin::invalid(
                "argument.entity.options.inapplicable",
                &name,
            )));
        }
        r.skip_whitespace();
        if !r.can_read() || r.peek() != '=' {
            r.cursor = start;
            return Err(r.error(builtin::invalid("argument.entity.options.valueless", &name)));
        }
        r.skip();
        r.skip_whitespace();
        let value_start = r.cursor;
        match name.as_str() {
            "name" => {
                let neg = negated(r);
                let v = r.read_string()?;
                s.names.push((neg, v));
            }
            "distance" => {
                let range = read_range(r, false)?;
                if range.min.is_some_and(|m| m < 0.0) || range.max.is_some_and(|m| m < 0.0) {
                    r.cursor = value_start;
                    return Err(r.error(builtin::expected(
                        "argument.entity.options.distance.negative",
                    )));
                }
                s.distance = Some(range);
            }
            "level" => {
                let range = read_range(r, true)?;
                if range.min.is_some_and(|m| m < 0.0) || range.max.is_some_and(|m| m < 0.0) {
                    r.cursor = value_start;
                    return Err(
                        r.error(builtin::expected("argument.entity.options.level.negative"))
                    );
                }
                s.level = Some(range);
            }
            "x" => s.x = Some(r.read_double()?),
            "y" => s.y = Some(r.read_double()?),
            "z" => s.z = Some(r.read_double()?),
            "dx" => s.dx = Some(r.read_double()?),
            "dy" => s.dy = Some(r.read_double()?),
            "dz" => s.dz = Some(r.read_double()?),
            "x_rotation" => s.x_rotation = Some(read_range(r, false)?),
            "y_rotation" => s.y_rotation = Some(read_range(r, false)?),
            "limit" => {
                let n = r.read_int()?;
                if n < 1 {
                    r.cursor = value_start;
                    return Err(
                        r.error(builtin::expected("argument.entity.options.limit.toosmall"))
                    );
                }
                s.limit = Some(n);
            }
            "sort" => {
                let v = r.read_unquoted();
                if !matches!(v, "nearest" | "furthest" | "random" | "arbitrary") {
                    r.cursor = value_start;
                    return Err(r.error(builtin::invalid(
                        "argument.entity.options.sort.irreversible",
                        v,
                    )));
                }
                s.sort = Some(v.to_owned());
            }
            "gamemode" => {
                let neg = negated(r);
                let at = r.cursor;
                let v = r.read_unquoted();
                if !matches!(v, "survival" | "creative" | "adventure" | "spectator") {
                    r.cursor = at;
                    return Err(
                        r.error(builtin::invalid("argument.entity.options.mode.invalid", v))
                    );
                }
                s.gamemodes.push((neg, v.to_owned()));
            }
            "team" => {
                let neg = negated(r);
                let v = r.read_unquoted().to_owned();
                s.teams.push((neg, v));
            }
            "type" => {
                let neg = negated(r);
                let at = r.cursor;
                let tag = r.can_read() && r.peek() == '#';
                if tag {
                    r.skip();
                }
                let id = read_id(r)?;
                let ok = if tag {
                    env.tag_exists("minecraft:entity_type", &id)
                } else {
                    env.exists("minecraft:entity_type", &id)
                };
                if !ok {
                    r.cursor = at;
                    let shown = if tag { format!("#{id}") } else { id };
                    return Err(r.error(builtin::invalid(
                        "argument.entity.options.type.invalid",
                        &shown,
                    )));
                }
                s.types.push((neg, if tag { format!("#{id}") } else { id }));
            }
            "tag" => {
                let neg = negated(r);
                let v = r.read_unquoted().to_owned();
                s.tags.push((neg, v));
            }
            "nbt" => {
                let neg = negated(r);
                let v = read_nbt(r)?;
                s.nbt.push((neg, v));
            }
            "scores" => {
                r.expect('{')?;
                r.skip_whitespace();
                while r.can_read() && r.peek() != '}' {
                    r.skip_whitespace();
                    let obj = r.read_unquoted().to_owned();
                    r.skip_whitespace();
                    r.expect('=')?;
                    r.skip_whitespace();
                    let range = read_range(r, true)?;
                    s.scores.push((obj, range));
                    r.skip_whitespace();
                    if r.can_read() && r.peek() == ',' {
                        r.skip();
                    }
                }
                r.expect('}')?;
            }
            "advancements" => {
                let v = read_raw_braced(r)?;
                s.advancements = Some(v);
            }
            _ => {
                let neg = negated(r);
                let v = read_id(r)?;
                s.predicates.push((neg, v));
            }
        }
        r.skip_whitespace();
        if r.can_read() {
            if r.peek() == ',' {
                r.skip();
                continue;
            }
            if r.peek() != ']' {
                return Err(r.error(builtin::expected("argument.entity.options.unterminated")));
            }
        }
    }
    if r.can_read() {
        r.skip();
        Ok(())
    } else {
        Err(r.error(builtin::expected("argument.entity.options.unterminated")))
    }
}

fn read_raw_braced(r: &mut Reader) -> Result<String> {
    if !r.can_read() || r.peek() != '{' {
        return Err(r.error(builtin::expected_char('{')));
    }
    let start = r.cursor;
    let mut depth = 0;
    while r.can_read() {
        let c = r.read_char();
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                return Ok(r.input[start..r.cursor].to_owned());
            }
        }
    }
    Err(r.error(builtin::expected_char('}')))
}

/// A selector, player name or UUID.
pub fn read_selector(r: &mut Reader, env: &dyn Env) -> Result<Selector> {
    let mut s = Selector::default();
    if r.can_read() && r.peek() == '@' {
        let start = r.cursor;
        r.skip();
        if !r.can_read() {
            return Err(r.error(builtin::expected("argument.entity.selector.missing")));
        }
        let c = r.read_char();
        if !matches!(c, 'p' | 'a' | 'r' | 's' | 'e' | 'n') {
            r.cursor = start + 1;
            return Err(r.error(builtin::invalid(
                "argument.entity.selector.unknown",
                &format!("@{c}"),
            )));
        }
        s.kind = Some(c);
        match c {
            'p' | 'r' | 'n' => s.limit = Some(1),
            's' => s.limit = Some(1),
            _ => {}
        }
        if r.can_read() && r.peek() == '[' {
            selector_options(r, &mut s, env)?;
        }
        return Ok(s);
    }
    let start = r.cursor;
    let name = r.read_string()?;
    if let Some(u) = parse_uuid(&name) {
        s.uuid = Some(u);
        return Ok(s);
    }
    if name.is_empty() || name.chars().count() > 16 {
        r.cursor = start;
        return Err(r.error(builtin::expected("argument.entity.invalid")));
    }
    s.player = Some(name);
    Ok(s)
}

/// A UUID written `8-4-4-4-12` (hex groups may be shorter).
pub fn parse_uuid(s: &str) -> Option<u128> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 || parts.iter().any(|p| p.is_empty() || p.len() > 16) {
        return None;
    }
    let n: Vec<u64> = parts
        .iter()
        .map(|p| u64::from_str_radix(p, 16).ok())
        .collect::<Option<_>>()?;
    let hi = (n[0] << 32) | ((n[1] & 0xffff) << 16) | (n[2] & 0xffff);
    let lo = ((n[3] & 0xffff) << 48) | (n[4] & 0xffff_ffff_ffff);
    Some((u128::from(hi) << 64) | u128::from(lo))
}

fn block_arg(r: &mut Reader, env: &dyn Env, tags: bool) -> Result<Block> {
    let start = r.cursor;
    let tag = tags && r.can_read() && r.peek() == '#';
    if r.can_read() && r.peek() == '#' && !tags {
        return Err(r.error(builtin::expected("argument.block.tag.disallowed")));
    }
    if tag {
        r.skip();
    }
    let id = read_id(r)?;
    let props = if tag {
        if !env.tag_exists("minecraft:block", &id) {
            r.cursor = start;
            return Err(r.error(builtin::invalid("arguments.block.tag.unknown", &id)));
        }
        None
    } else {
        match env.block_properties(&id) {
            Some(p) => Some(p),
            None => {
                r.cursor = start;
                return Err(r.error(builtin::invalid("argument.block.id.invalid", &id)));
            }
        }
    };
    let mut properties = Vec::new();
    if r.can_read() && r.peek() == '[' {
        r.skip();
        r.skip_whitespace();
        while r.can_read() && r.peek() != ']' {
            r.skip_whitespace();
            let at = r.cursor;
            let key = r.read_string()?;
            if let Some(p) = &props
                && !p.iter().any(|(k, _)| *k == key)
            {
                r.cursor = at;
                return Err(r.error(CommandError::new(
                    "argument.block.property.unknown",
                    vec![string(&id), string(&key)],
                )));
            }
            if properties.iter().any(|(k, _): &(String, String)| *k == key) {
                r.cursor = at;
                return Err(r.error(CommandError::new(
                    "argument.block.property.duplicate",
                    vec![string(&key), string(&id)],
                )));
            }
            r.skip_whitespace();
            if !r.can_read() || r.peek() != '=' {
                return Err(r.error(CommandError::new(
                    "argument.block.property.novalue",
                    vec![string(&key), string(&id)],
                )));
            }
            r.skip();
            r.skip_whitespace();
            let vat = r.cursor;
            let value = r.read_string()?;
            if let Some(p) = &props
                && let Some((_, values)) = p.iter().find(|(k, _)| *k == key)
                && !values.contains(&value)
            {
                r.cursor = vat;
                return Err(r.error(CommandError::new(
                    "argument.block.property.invalid",
                    vec![string(&id), string(&value), string(&key)],
                )));
            }
            properties.push((key, value));
            r.skip_whitespace();
            if r.can_read() && r.peek() == ',' {
                r.skip();
                continue;
            }
            if r.can_read() && r.peek() != ']' {
                return Err(r.error(builtin::expected("argument.block.property.unclosed")));
            }
        }
        if !r.can_read() {
            return Err(r.error(builtin::expected("argument.block.property.unclosed")));
        }
        r.skip();
    }
    let nbt = if r.can_read() && r.peek() == '{' {
        Some(read_nbt(r)?)
    } else {
        None
    };
    Ok(Block {
        id,
        tag,
        properties,
        nbt,
    })
}

fn item_arg(r: &mut Reader, env: &dyn Env, tags: bool) -> Result<Item> {
    let start = r.cursor;
    let tag = tags && r.can_read() && r.peek() == '#';
    if tag {
        r.skip();
    }
    let id = if tags && r.can_read() && r.peek() == '*' {
        r.skip();
        "*".to_owned()
    } else {
        read_id(r)?
    };
    let ok = id == "*"
        || if tag {
            env.tag_exists("minecraft:item", &id)
        } else {
            env.exists("minecraft:item", &id)
        };
    if !ok {
        r.cursor = start;
        let key = if tag {
            "arguments.item.tag.unknown"
        } else {
            "argument.item.id.invalid"
        };
        return Err(r.error(builtin::invalid(key, &id)));
    }
    let components = if r.can_read() && r.peek() == '[' {
        let s = r.cursor;
        read_raw(r);
        Some(r.input[s..r.cursor].to_owned())
    } else {
        None
    };
    Ok(Item {
        id,
        tag,
        components,
    })
}

fn time(r: &mut Reader, min: i32) -> Result<i32> {
    let f = r.read_float()?;
    let unit = r.read_unquoted();
    let mult = match unit {
        "" | "t" => 1.0,
        "s" => 20.0,
        "d" => 24000.0,
        _ => return Err(r.error(builtin::expected("argument.time.invalid_unit"))),
    };
    let ticks = (f * mult).round() as i32;
    if ticks < min {
        return Err(r.error(CommandError::new(
            "argument.time.tick_count_too_low",
            vec![int(i64::from(min)), int(i64::from(ticks))],
        )));
    }
    Ok(ticks)
}

/// Reads an argument of `parser` with its `properties` (as in the tree).
pub fn parse(parser: &str, props: &Value, r: &mut Reader, env: &dyn Env) -> Result<Arg> {
    let start = r.cursor;
    let fnum = |k: &str| props[k].as_f64();
    let inum = |k: &str| props[k].as_i64();
    Ok(match parser {
        "brigadier:bool" => Arg::Bool(r.read_bool()?),
        "brigadier:integer" => {
            let v = r.read_int()?;
            let (min, max) = (inum("min").map(|v| v as i32), inum("max").map(|v| v as i32));
            Arg::Int(num_bounds(r, start, v, min, max, "integer", |v| {
                Tag::Int(v)
            })?)
        }
        "brigadier:long" => {
            let v = r.read_long()?;
            Arg::Long(num_bounds(
                r,
                start,
                v,
                inum("min"),
                inum("max"),
                "long",
                Tag::Long,
            )?)
        }
        "brigadier:float" => {
            let v = r.read_float()?;
            let (min, max) = (fnum("min").map(|v| v as f32), fnum("max").map(|v| v as f32));
            Arg::Float(num_bounds(r, start, v, min, max, "float", Tag::Float)?)
        }
        "brigadier:double" => {
            let v = r.read_double()?;
            Arg::Double(num_bounds(
                r,
                start,
                v,
                fnum("min"),
                fnum("max"),
                "double",
                Tag::Double,
            )?)
        }
        "brigadier:string" => Arg::Str(match props["type"].as_i64() {
            Some(0) => r.read_unquoted().to_owned(),
            Some(2) => {
                let s = r.remaining().to_owned();
                r.cursor = r.input.len();
                s
            }
            _ => r.read_string()?,
        }),
        "minecraft:entity" | "minecraft:game_profile" => {
            let s = read_selector(r, env)?;
            let flags = props["flags"].as_u64().unwrap_or(0);
            // Vanilla points at the start of the line for these.
            if parser == "minecraft:entity" && flags & 1 != 0 && !s.single() {
                r.cursor = 0;
                let key = if flags & 2 != 0 {
                    "argument.player.toomany"
                } else {
                    "argument.entity.toomany"
                };
                return Err(r.error(builtin::expected(key)));
            }
            if parser == "minecraft:entity" && flags & 2 != 0 && !s.players_only() {
                r.cursor = 0;
                return Err(r.error(builtin::expected("argument.player.entities")));
            }
            Arg::Entity(Box::new(s))
        }
        "minecraft:score_holder" => {
            if r.can_read() && r.peek() == '@' {
                let s = read_selector(r, env)?;
                if props["flags"].as_u64().unwrap_or(0) & 1 == 0 && !s.single() {
                    return Err(r.error(builtin::expected("argument.entity.toomany")));
                }
                Arg::Entity(Box::new(s))
            } else {
                let s = r.cursor;
                while r.can_read() && r.peek() != ' ' {
                    r.skip();
                }
                Arg::Str(r.input[s..r.cursor].to_owned())
            }
        }
        "minecraft:block_pos" => Arg::Pos(pos3(r, true, false)?),
        "minecraft:vec3" => Arg::Pos(pos3(r, false, true)?),
        "minecraft:column_pos" => Arg::Pos2(pos2(r, true, false, "argument.pos2d.incomplete")?),
        "minecraft:vec2" => Arg::Pos2(pos2(r, false, true, "argument.pos2d.incomplete")?),
        "minecraft:rotation" => Arg::Pos2(pos2(r, false, false, "argument.rotation.incomplete")?),
        "minecraft:angle" => {
            if !r.can_read() {
                return Err(r.error(builtin::expected("argument.angle.incomplete")));
            }
            let c = coord(r, false, false)
                .map_err(|_| r.error(builtin::expected("argument.angle.invalid")))?;
            Arg::Pos2([c, c])
        }
        "minecraft:block_state" => Arg::Block(block_arg(r, env, false)?),
        "minecraft:block_predicate" => Arg::Block(block_arg(r, env, true)?),
        "minecraft:item_stack" => Arg::Item(item_arg(r, env, false)?),
        "minecraft:item_predicate" => Arg::Item(item_arg(r, env, true)?),
        "minecraft:resource_location" | "minecraft:function_id" => Arg::Id(read_id(r)?),
        "minecraft:function" => {
            let tag = r.can_read() && r.peek() == '#';
            if tag {
                r.skip();
            }
            Arg::IdOrTag {
                id: read_id(r)?,
                tag,
            }
        }
        "minecraft:particle" => {
            let id = read_id(r)?;
            if !env.exists("minecraft:particle_type", &id) {
                return Err(r.error(CommandError::new("particle.notFound", vec![string(&id)])));
            }
            let options = if r.can_read() && r.peek() == '{' {
                match read_nbt(r)? {
                    Tag::Compound(c) => c,
                    _ => mc_nbt::Compound::new(),
                }
            } else {
                mc_nbt::Compound::new()
            };
            if let Err(why) = crate::particle::check(&id, &options) {
                return Err(CommandError::new(
                    "particle.invalidOptions",
                    vec![string(&why)],
                ));
            }
            Arg::Particle { id, options }
        }
        "minecraft:loot_predicate" => {
            if r.can_read() && matches!(r.peek(), '{' | '[') {
                let t = read_nbt(r)?;
                if !matches!(t, Tag::Compound(_)) {
                    let why = format!("Not a map: {}", crate::particle::snbt(&t));
                    return Err(r.error_at(
                        CommandError::new(
                            "argument.resource_or_id.failed_to_parse",
                            vec![string(&why)],
                        ),
                        r.input.len(),
                    ));
                }
                Arg::Nbt(t)
            } else {
                Arg::Id(read_id(r)?)
            }
        }
        "minecraft:context_int_provider" | "minecraft:context_float_provider" => {
            if r.can_read() && matches!(r.peek(), '{' | '[') {
                Arg::Nbt(read_nbt(r)?)
            } else {
                let id = read_id(r)?;
                if !env.exists(parser, &full_id(&id)) {
                    return Err(r.error(CommandError::new(
                        "argument.resource_or_id.no_such_element",
                        vec![string(&full_id(&id)), string(parser)],
                    )));
                }
                Arg::Id(id)
            }
        }
        "minecraft:loot_modifier" | "minecraft:loot_table" => {
            while r.can_read() && r.peek() == ' ' {
                r.skip();
            }
            if r.can_read() && matches!(r.peek(), '{' | '[') {
                Arg::Nbt(read_nbt(r)?)
            } else {
                let id = read_id(r)?;
                let registry = if parser == "minecraft:loot_modifier" {
                    "minecraft:item_modifier"
                } else {
                    "minecraft:loot_table"
                };
                if !env.exists(registry, &full_id(&id)) {
                    return Err(r.error(CommandError::new(
                        "argument.resource_or_id.no_such_element",
                        vec![string(&full_id(&id)), string(registry)],
                    )));
                }
                Arg::Id(id)
            }
        }
        "minecraft:item_slot" => {
            let s = r.cursor;
            while r.can_read() && (id_char(r.peek()) || matches!(r.peek(), '*' | '-')) {
                r.skip();
            }
            let name = &r.input[s..r.cursor];
            match slot_range(name) {
                None => {
                    return Err(r.error(CommandError::new(
                        "slot.unknown",
                        vec![string(name)],
                    )));
                }
                Some(v) if v.len() != 1 => {
                    return Err(r.error(CommandError::new(
                        "slot.only_single_allowed",
                        vec![string(name)],
                    )));
                }
                Some(_) => Arg::Str(name.to_owned()),
            }
        }
        "minecraft:slot_source" => {
            let s = r.cursor;
            while r.can_read() && (id_char(r.peek()) || r.peek() == '*') {
                r.skip();
            }
            let name = &r.input[s..r.cursor];
            if slot_range(name).is_none() {
                return Err(r.error(CommandError::new(
                    "argument.resource_or_id.no_such_element",
                    vec![string(&full_id(name)), string("minecraft:slot_source")],
                )));
            }
            Arg::Str(name.to_owned())
        }
        "minecraft:dimension" => {
            let id = read_id(r)?;
            // Checked when run (vanilla's error has no context).
            Arg::Id(id)
        }
        "minecraft:resource" | "minecraft:resource_key" => {
            let id = read_id(r)?;
            let reg = props["registry"].as_str().unwrap_or("");
            if parser == "minecraft:resource" && !env.exists(reg, &id) {
                return Err(r.error(CommandError::new(
                    "argument.resource.not_found",
                    vec![string(&id), string(reg)],
                )));
            }
            Arg::Id(id)
        }
        "minecraft:resource_or_tag" | "minecraft:resource_or_tag_key" => {
            let tag = r.can_read() && r.peek() == '#';
            if tag {
                r.skip();
            }
            let id = read_id(r)?;
            let reg = props["registry"].as_str().unwrap_or("");
            if parser == "minecraft:resource_or_tag" {
                if tag && !env.tag_exists(reg, &id) {
                    return Err(r.error(CommandError::new(
                        "argument.resource_tag.not_found",
                        vec![string(&id), string(reg)],
                    )));
                }
                if !tag && !env.exists(reg, &id) {
                    return Err(r.error(CommandError::new(
                        "argument.resource.not_found",
                        vec![string(&id), string(reg)],
                    )));
                }
            }
            Arg::IdOrTag { id, tag }
        }
        "minecraft:gamemode" => {
            let v = r.read_unquoted();
            if !matches!(v, "survival" | "creative" | "adventure" | "spectator") {
                return Err(r.error(builtin::invalid("argument.gamemode.invalid", v)));
            }
            Arg::Str(v.to_owned())
        }
        "minecraft:time" => Arg::Time(time(r, inum("min").unwrap_or(0) as i32)?),
        "minecraft:int_range" => Arg::Range(read_range(r, true)?),
        "minecraft:float_range" => Arg::Range(read_range(r, false)?),
        "minecraft:nbt_compound_tag" => {
            let t = read_nbt(r)?;
            if !matches!(t, Tag::Compound(_)) {
                r.cursor = start;
                return Err(r.error(builtin::expected("argument.nbt.expected.compound")));
            }
            Arg::Nbt(t)
        }
        "minecraft:component" => Arg::Nbt(plain_text(read_nbt(r)?)),
        "minecraft:nbt_path" => Arg::NbtPath(crate::nbtpath::parse(r)?),
        "minecraft:nbt_tag" | "minecraft:style" => Arg::Nbt(read_nbt(r)?),
        "minecraft:message" => {
            let s = r.remaining().to_owned();
            r.cursor = r.input.len();
            Arg::Raw(s)
        }
        "minecraft:entity_anchor" => {
            let v = r.read_unquoted();
            if !matches!(v, "eyes" | "feet") {
                r.cursor = start;
                return Err(r.error(builtin::invalid("argument.anchor.invalid", v)));
            }
            Arg::Str(v.to_owned())
        }
        "minecraft:swizzle" => {
            let v = r.read_unquoted();
            let mut seen = Vec::new();
            for c in v.chars() {
                if !matches!(c, 'x' | 'y' | 'z') || seen.contains(&c) {
                    r.cursor = start;
                    return Err(r.error(builtin::expected("arguments.swizzle.invalid")));
                }
                seen.push(c);
            }
            Arg::Str(v.to_owned())
        }
        "minecraft:objective" | "minecraft:team" => Arg::Str(r.read_unquoted().to_owned()),
        "minecraft:team_color" => {
            let name = r.read_unquoted().to_owned();
            if !COLORS.contains(&name.as_str()) {
                return Err(r.error(CommandError::new(
                    "argument.color.invalid",
                    vec![string(&name)],
                )));
            }
            Arg::Str(name)
        }
        "minecraft:scoreboard_slot" => {
            let name = r.read_unquoted().to_owned();
            let ok = matches!(name.as_str(), "list" | "sidebar" | "below_name")
                || name
                    .strip_prefix("sidebar.team.")
                    .is_some_and(|c| COLORS.contains(&c));
            if !ok {
                return Err(r.error(CommandError::new(
                    "argument.scoreboardDisplaySlot.invalid",
                    vec![string(&name)],
                )));
            }
            Arg::Str(name)
        }
        "minecraft:operation" => {
            let s = r.cursor;
            while r.can_read() && r.peek() != ' ' {
                r.skip();
            }
            let op = &r.input[s..r.cursor];
            if !matches!(
                op,
                "+=" | "-=" | "*=" | "/=" | "%=" | "=" | "<" | ">" | "><"
            ) {
                return Err(CommandError::new("arguments.operation.invalid", Vec::new()));
            }
            Arg::Str(op.to_owned())
        }
        "minecraft:objective_criteria" => {
            let s = r.cursor;
            while r.can_read() && r.peek() != ' ' {
                r.skip();
            }
            let name = &r.input[s..r.cursor];
            if !criteria_exist(env, name) {
                r.cursor = s;
                return Err(r.error(CommandError::new(
                    "argument.criteria.invalid",
                    vec![string(name)],
                )));
            }
            Arg::Str(name.to_owned())
        }
        "minecraft:uuid" => {
            let s = r.cursor;
            while r.can_read() && (r.peek().is_ascii_hexdigit() || r.peek() == '-') {
                r.skip();
            }
            let t = &r.input[s..r.cursor];
            match parse_uuid(t) {
                Some(_) => Arg::Str(t.to_owned()),
                None => {
                    r.cursor = start;
                    return Err(r.error(builtin::expected("argument.uuid.invalid")));
                }
            }
        }
        _ => {
            let raw = read_raw(r);
            if raw.is_empty() {
                return Err(r.error(builtin::expected("argument.id.invalid")));
            }
            Arg::Raw(raw.to_owned())
        }
    })
}

/// Whether a character may follow an unquoted word (for literal matching).
pub fn word_char(c: char) -> bool {
    is_unquoted(c)
}

/// Chat colours, as team colours and display slots name them.
pub const COLORS: [&str; 16] = [
    "black",
    "dark_blue",
    "dark_green",
    "dark_aqua",
    "dark_red",
    "dark_purple",
    "gold",
    "gray",
    "dark_gray",
    "blue",
    "green",
    "aqua",
    "red",
    "light_purple",
    "yellow",
    "white",
];

/// Whether an objective may count `name`: a simple criterion, a team kill
/// count, or a statistic (`minecraft.mined:minecraft.stone`).
fn criteria_exist(env: &dyn Env, name: &str) -> bool {
    const SIMPLE: [&str; 11] = [
        "dummy",
        "trigger",
        "deathCount",
        "playerKillCount",
        "totalKillCount",
        "health",
        "xp",
        "level",
        "food",
        "air",
        "armor",
    ];
    if SIMPLE.contains(&name) {
        return true;
    }
    for prefix in ["teamkill.", "killedByTeam."] {
        if let Some(c) = name.strip_prefix(prefix) {
            return COLORS.contains(&c);
        }
    }
    let Some((kind, id)) = name.split_once(':') else {
        return false;
    };
    let dotted = |s: &str| match s.split_once('.') {
        Some((ns, path)) => format!("{ns}:{path}"),
        None => format!("minecraft:{s}"),
    };
    let registry = match dotted(kind).as_str() {
        "minecraft:mined" => "minecraft:block",
        "minecraft:crafted"
        | "minecraft:used"
        | "minecraft:broken"
        | "minecraft:picked_up"
        | "minecraft:dropped" => "minecraft:item",
        "minecraft:killed" | "minecraft:killed_by" => "minecraft:entity_type",
        "minecraft:custom" => "minecraft:custom_stat",
        _ => return false,
    };
    env.exists(registry, &dotted(id))
}

/// A text component as vanilla stores it: one that is only `{text: ...}`
/// becomes the string, in it and in its `extra` and `with` parts.
pub fn plain_text(t: Tag) -> Tag {
    match t {
        Tag::Compound(mut c) => {
            if c.len() == 1
                && let Some(Tag::String(s)) = c.get("text")
            {
                return Tag::String(s.clone());
            }
            for key in ["extra", "with"] {
                if let Some(Tag::List(l)) = c.get(key) {
                    let parts: Vec<Tag> = l.iter().cloned().map(plain_text).collect();
                    c.insert(key, crate::text::list_of(parts));
                }
            }
            Tag::Compound(c)
        }
        Tag::List(l) => crate::text::list_of(l.into_iter().map(plain_text).collect()),
        other => other,
    }
}
