//! Item stacks and their data components (26.3).
//!
//! A stack keeps its component changes (the "patch" against the item's
//! defaults) as NBT in the form worlds save them: `minecraft:<type>` for an
//! added or changed component, `!minecraft:<type>` for one removed. That keeps
//! everything a world had, whether or not we know the component. The damage
//! lives on the stack itself, being by far the most used.
//!
//! On the network every known component has its own encoding (observed from
//! vanilla with `cargo xtask observe-commands`: stacks given with
//! `/item replace` and read back from Set Container Slot). Components without
//! a known encoding stay on the server and are not sent. The client describes
//! stacks in window clicks by a CRC32C hash of every component's value, which
//! [`hash_component`] works out from the saved form.
//!
//! Registry ids (items, enchantments, effects...) come from
//! [`install_registries`], which the server calls once its registries are
//! known.

use std::collections::HashMap;
use std::sync::OnceLock;

use mc_nbt::{Compound, Tag};

use crate::codec::{DecodeError, Position, Reader, Result, Write, put_nbt};

// ---------------------------------------------------------------------------
// Registries

struct Registry {
    ids: HashMap<String, i32>,
    names: Vec<String>,
}

static REGISTRIES: OnceLock<HashMap<String, Registry>> = OnceLock::new();

/// Sets the registries entries are looked up in: registry name → entry
/// name → network id. Only the first call counts (the ids are those of the
/// game version and the data packs, the same for every server in a process).
pub fn install_registries<'a>(
    registries: impl IntoIterator<Item = (&'a str, &'a HashMap<String, i32>)>,
) {
    if REGISTRIES.get().is_some() {
        return;
    }
    let mut all = HashMap::new();
    for (name, ids) in registries {
        let mut names =
            vec![String::new(); ids.values().map(|&i| i as usize + 1).max().unwrap_or(0)];
        for (entry, &id) in ids {
            if let Some(slot) = usize::try_from(id).ok().and_then(|i| names.get_mut(i)) {
                slot.clone_from(entry);
            }
        }
        all.insert(
            name.to_owned(),
            Registry {
                ids: ids.clone(),
                names,
            },
        );
    }
    let _ = REGISTRIES.set(all);
}

/// `name` with the `minecraft:` namespace when it has none.
pub fn full_name(name: &str) -> std::borrow::Cow<'_, str> {
    if name.contains(':') {
        name.into()
    } else {
        format!("minecraft:{name}").into()
    }
}

/// Network id of `name` in `registry`.
pub fn registry_id(registry: &str, name: &str) -> Option<i32> {
    let r = REGISTRIES.get()?.get(registry)?;
    r.ids.get(full_name(name).as_ref()).copied()
}

/// Entry of `registry` with network id `id`.
pub fn registry_name(registry: &str, id: i32) -> Option<&'static str> {
    let r = REGISTRIES.get()?.get(registry)?;
    let name = r.names.get(usize::try_from(id).ok()?)?;
    (!name.is_empty()).then_some(name.as_str())
}

const ITEMS: &str = "minecraft:item";
const COMPONENT_TYPES: &str = "minecraft:data_component_type";
pub const DAMAGE: &str = "minecraft:damage";

// ---------------------------------------------------------------------------
// The patch

/// A stack's component changes, damage aside. Kept sorted by type, so equal
/// patches compare equal whatever order they were built in.
#[derive(Clone, Debug, Default)]
pub struct Components(Vec<(String, Option<Tag>)>);

impl PartialEq for Components {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().zip(&other.0).all(|(a, b)| {
                a.0 == b.0
                    && match (&a.1, &b.1) {
                        (Some(x), Some(y)) => tag_eq(x, y),
                        (None, None) => true,
                        _ => false,
                    }
            })
    }
}

/// Tags equal as values: compounds whatever their key order.
pub fn tag_eq(a: &Tag, b: &Tag) -> bool {
    match (a, b) {
        (Tag::Compound(x), Tag::Compound(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| tag_eq(v, w)))
        }
        (Tag::List(x), Tag::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| tag_eq(v, w))
        }
        _ => a == b,
    }
}

impl Components {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    fn find(&self, name: &str) -> std::result::Result<usize, usize> {
        self.0.binary_search_by(|(k, _)| k.as_str().cmp(name))
    }

    /// An added component's value.
    pub fn get(&self, name: &str) -> Option<&Tag> {
        let name = full_name(name);
        let i = self.find(&name).ok()?;
        self.0[i].1.as_ref()
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Tag> {
        let name = full_name(name);
        let i = self.find(&name).ok()?;
        self.0[i].1.as_mut()
    }

    /// Whether the patch removes the item's default `name`.
    pub fn is_removed(&self, name: &str) -> bool {
        let name = full_name(name);
        self.find(&name).is_ok_and(|i| self.0[i].1.is_none())
    }

    /// Adds or replaces a component.
    pub fn set(&mut self, name: &str, value: Tag) {
        self.put(full_name(name).into_owned(), Some(value));
    }

    /// Removes the item's default `name` from the stack.
    pub fn set_removed(&mut self, name: &str) {
        self.put(full_name(name).into_owned(), None);
    }

    /// Drops any change to `name` (the item's default applies again).
    pub fn reset(&mut self, name: &str) -> Option<Tag> {
        let name = full_name(name);
        let i = self.find(&name).ok()?;
        self.0.remove(i).1
    }

    fn put(&mut self, name: String, value: Option<Tag>) {
        match self.find(&name) {
            Ok(i) => self.0[i].1 = value,
            Err(i) => self.0.insert(i, (name, value)),
        }
    }

    /// Every change: type and value (`None`: removed).
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<&Tag>)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_ref()))
    }

    /// Reads a saved `components` compound: the damage and the rest.
    pub fn from_nbt(saved: &Compound) -> (i32, Self) {
        let mut damage = 0;
        let mut patch = Self::new();
        for (key, value) in saved.iter() {
            if let Some(name) = key.strip_prefix('!') {
                patch.set_removed(name);
            } else if full_name(key) == DAMAGE {
                damage = match value {
                    Tag::Int(d) => *d,
                    Tag::Short(d) => i32::from(*d),
                    Tag::Byte(d) => i32::from(*d),
                    _ => 0,
                };
            } else {
                patch.set(key, value.clone());
            }
        }
        (damage, patch)
    }

    /// The saved `components` compound with `damage`; `None` when empty.
    pub fn to_nbt(&self, damage: i32) -> Option<Compound> {
        if damage == 0 && self.is_empty() {
            return None;
        }
        let mut c = Compound::new();
        if damage != 0 {
            c.insert(DAMAGE, Tag::Int(damage));
        }
        for (name, value) in &self.0 {
            match value {
                Some(v) => c.insert(name.clone(), v.clone()),
                None => c.insert(format!("!{name}"), Tag::Compound(Compound::new())),
            };
        }
        Some(c)
    }

    /// Writes the patch (added count, removed count, entries) with `damage`
    /// first. Components we cannot encode are left out.
    pub(crate) fn encode(&self, damage: i32, out: &mut Vec<u8>) {
        let mut added = Vec::new();
        let mut added_count = 0;
        let mut removed = Vec::new();
        let mut removed_count = 0;
        if damage != 0
            && let Some(id) = registry_id(COMPONENT_TYPES, DAMAGE).or(Some(3))
        {
            added.put_varint(id);
            added.put_varint(damage);
            added_count += 1;
        }
        for (name, value) in &self.0 {
            let Some(id) = registry_id(COMPONENT_TYPES, name) else {
                continue;
            };
            match value {
                None => {
                    removed.put_varint(id);
                    removed_count += 1;
                }
                Some(value) => {
                    let Some(net) = codec(name) else { continue };
                    let mut body = Vec::new();
                    if write(net, value, &mut body).is_some() {
                        added.put_varint(id);
                        added.extend_from_slice(&body);
                        added_count += 1;
                    }
                }
            }
        }
        out.put_varint(added_count);
        out.put_varint(removed_count);
        out.extend_from_slice(&added);
        out.extend_from_slice(&removed);
    }

    /// Reads a patch; reading stops (taking the rest) at a component we
    /// cannot decode. Returns the damage too.
    fn decode(r: &mut Reader) -> Result<(i32, Self)> {
        let (added, removed) = (r.varint()?, r.varint()?);
        if !(0..=256).contains(&added) || !(0..=256).contains(&removed) {
            return Err(DecodeError::Invalid("component count"));
        }
        let mut damage = 0;
        let mut patch = Self::new();
        for _ in 0..added {
            let id = r.varint()?;
            let name = registry_name(COMPONENT_TYPES, id).or((id == 3).then_some(DAMAGE));
            let Some(name) = name else {
                r.rest();
                return Ok((damage, patch));
            };
            if name == DAMAGE {
                damage = r.varint()?;
                continue;
            }
            let Some(net) = codec(name) else {
                r.rest();
                return Ok((damage, patch));
            };
            let value = read(net, r, 0)?;
            patch.set(name, value);
        }
        for _ in 0..removed {
            let id = r.varint()?;
            if let Some(name) = registry_name(COMPONENT_TYPES, id) {
                patch.set_removed(name);
            }
        }
        Ok((damage, patch))
    }

    /// A patch as clients send it in packets they are not trusted with
    /// (the creative inventory): each added value comes after its length,
    /// so one we cannot read is skipped.
    fn decode_delimited(r: &mut Reader) -> Result<(i32, Self)> {
        let (added, removed) = (r.varint()?, r.varint()?);
        if !(0..=256).contains(&added) || !(0..=256).contains(&removed) {
            return Err(DecodeError::Invalid("component count"));
        }
        let mut damage = 0;
        let mut patch = Self::new();
        for _ in 0..added {
            let id = r.varint()?;
            let len = r.length_prefix()?;
            let mut value = Reader::new(r.bytes(len)?);
            let name = registry_name(COMPONENT_TYPES, id).or((id == 3).then_some(DAMAGE));
            let Some(name) = name else {
                continue;
            };
            if name == DAMAGE {
                damage = value.varint()?;
                continue;
            }
            let Some(net) = codec(name) else {
                continue;
            };
            // A value we cannot make out is dropped, not the packet.
            if let Ok(v) = read(net, &mut value, 0) {
                patch.set(name, v);
            }
        }
        for _ in 0..removed {
            let id = r.varint()?;
            if let Some(name) = registry_name(COMPONENT_TYPES, id) {
                patch.set_removed(name);
            }
        }
        Ok((damage, patch))
    }
}

// ---------------------------------------------------------------------------
// Stacks

/// An item stack.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemStack {
    pub count: i32,
    /// Id in the `minecraft:item` registry.
    pub item: i32,
    /// `minecraft:damage`; 0 is the default and is not sent.
    pub damage: i32,
    /// Every other component change.
    pub components: Components,
}

impl ItemStack {
    /// `count` of `item` without component changes.
    pub fn new(item: i32, count: i32) -> Self {
        Self {
            count,
            item,
            damage: 0,
            components: Components::new(),
        }
    }

    /// Whether it has any component changes.
    pub fn has_changes(&self) -> bool {
        self.damage != 0 || !self.components.is_empty()
    }

    /// Whether the two stack together: same item and the same changes.
    pub fn same_kind(&self, other: &Self) -> bool {
        self.item == other.item
            && self.damage == other.damage
            && self.components == other.components
    }

    /// This stack with `count` items.
    pub fn with_count(&self, count: i32) -> Self {
        Self {
            count,
            ..self.clone()
        }
    }

    /// Slot data; `None` for an empty slot.
    pub fn decode(r: &mut Reader) -> Result<Option<Self>> {
        let count = r.varint()?;
        if count <= 0 {
            return Ok(None);
        }
        let item = r.varint()?;
        let (damage, components) = Components::decode(r)?;
        Ok(Some(Self {
            count,
            item,
            damage,
            components,
        }))
    }

    /// Slot data as the creative inventory sends it: the components after
    /// their lengths.
    pub fn decode_untrusted(r: &mut Reader) -> Result<Option<Self>> {
        let count = r.varint()?;
        if count <= 0 {
            return Ok(None);
        }
        let item = r.varint()?;
        let (damage, components) = Components::decode_delimited(r)?;
        Ok(Some(Self {
            count,
            item,
            damage,
            components,
        }))
    }

    /// Slot data.
    pub fn encode(stack: Option<&Self>, out: &mut Vec<u8>) {
        match stack {
            None => out.put_varint(0),
            Some(stack) => {
                out.put_varint(stack.count);
                out.put_varint(stack.item);
                stack.components.encode(stack.damage, out);
            }
        }
    }

    /// A saved stack (`id`, `count`, `components`), item ids from the
    /// installed registries.
    pub fn from_nbt(saved: &Compound) -> Option<Self> {
        let Some(Tag::String(id)) = saved.get("id") else {
            return None;
        };
        let item = registry_id(ITEMS, id)?;
        Some(Self::from_nbt_with(saved, item))
    }

    /// [`ItemStack::from_nbt`] with the item id already known.
    pub fn from_nbt_with(saved: &Compound, item: i32) -> Self {
        let count = match saved.get("count").or_else(|| saved.get("Count")) {
            Some(Tag::Int(c)) => *c,
            Some(Tag::Byte(c)) => i32::from(*c),
            Some(Tag::Short(c)) => i32::from(*c),
            _ => 1,
        };
        let (damage, components) = match saved.get("components") {
            Some(Tag::Compound(c)) => Components::from_nbt(c),
            _ => (0, Components::new()),
        };
        Self {
            count,
            item,
            damage,
            components,
        }
    }

    /// The stack as worlds save it; `None` if the item has no name.
    pub fn to_nbt(&self) -> Option<Compound> {
        Some(self.to_nbt_with(registry_name(ITEMS, self.item)?))
    }

    /// [`ItemStack::to_nbt`] with the item's name given.
    pub fn to_nbt_with(&self, id: &str) -> Compound {
        let mut c = Compound::new();
        self.write_nbt(id, &mut c);
        c
    }

    /// Adds the stack's `id`, `count` and `components` to `c`, after what
    /// it holds already (a container's `Slot`).
    pub fn write_nbt(&self, id: &str, c: &mut Compound) {
        c.insert("id", Tag::String(id.to_owned()));
        c.insert("count", Tag::Int(self.count));
        if let Some(components) = self.components.to_nbt(self.damage) {
            c.insert("components", Tag::Compound(components));
        }
    }

    /// The stack saved inside a component (a count of 1 left out).
    pub fn to_template_nbt(&self) -> Option<Compound> {
        let mut c = self.to_nbt()?;
        if self.count == 1 {
            c.remove("count");
        }
        Some(c)
    }

    /// Hashes of the added components as a window click carries them.
    pub fn hashes(&self) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        if self.damage != 0 {
            out.push((
                registry_id(COMPONENT_TYPES, DAMAGE).unwrap_or(3),
                int_component_hash(self.damage),
            ));
        }
        for (name, value) in self.components.iter() {
            if let (Some(value), Some(id)) = (value, registry_id(COMPONENT_TYPES, name)) {
                out.push((id, hash_component(name, value)));
            }
        }
        out
    }

    /// Ids of the components the stack removes.
    pub fn removed_ids(&self) -> Vec<i32> {
        self.components
            .iter()
            .filter(|(_, v)| v.is_none())
            .filter_map(|(n, _)| registry_id(COMPONENT_TYPES, n))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Network encodings

/// How a component (or a part of one) is encoded on the network, and the
/// type it has when saved.
#[derive(Debug)]
pub enum Net {
    /// Nothing on the network; `{}` when saved.
    Unit,
    /// Boolean; a byte when saved.
    Bool,
    /// VarInt on the network, a byte when saved (0–255).
    UByte,
    /// VarInt on the network, an int when saved.
    VarInt,
    /// Four bytes, an int when saved.
    Int,
    Long,
    Float,
    Double,
    Str,
    /// Network NBT of the saved value.
    Nbt,
    /// A text component (network NBT).
    Text,
    /// Registry entry: VarInt id; its name when saved.
    Holder(&'static str),
    /// Registry entry that could be inline: VarInt id + 1.
    HolderPlus(&'static str),
    /// One of a fixed list: VarInt index; the name when saved.
    Enum(&'static [&'static str]),
    List(&'static Net),
    /// Ints: VarInt count and four bytes each; an int array when saved.
    IntList,
    /// String keys when saved.
    Map(&'static Net, &'static Net),
    /// A compound when saved; fields on the network in this order.
    Fields(&'static [Field]),
    /// A stack inside a component: item, count, patch; `{id, count,
    /// components}` when saved.
    Template,
    /// Container contents: a slot list of optional templates; `[{slot,
    /// item}]` when saved.
    Container,
    /// Text kept with an optional filtered version: `raw`, optional
    /// `filtered`; the bare value when not filtered.
    Filterable(&'static Net),
    /// `potion_contents`: may be saved as just the potion's name.
    Potion,
    /// `profile`: a name, or a compound.
    Profile,
    /// `{id, ...}` data of a typed block entity or entity: VarInt type, then
    /// the rest as NBT.
    Typed(&'static str),
    /// Block position: a long on the network, `[x, y, z]` when saved.
    BlockPos,
    /// Registry entries: `#tag`, one name or a list.
    HolderSet(&'static str),
    /// How an attribute modifier shows in the tooltip.
    Display,
}

#[derive(Debug)]
pub struct Field {
    pub key: &'static str,
    pub net: Net,
    pub kind: Kind,
}

/// Whether a field may be left out.
#[derive(Debug)]
pub enum Kind {
    Required,
    /// Left out when saved if it has this value; always on the network.
    Default(Def),
    /// Always saved; this value when missing.
    Always(Def),
    /// May be missing: a presence boolean on the network.
    Optional,
}

#[derive(Debug)]
pub enum Def {
    Bool(bool),
    Int(i32),
    EmptyList,
    Str(&'static str),
    /// The value of another (boolean) field.
    SameAs(&'static str),
}

const fn req(key: &'static str, net: Net) -> Field {
    Field {
        key,
        net,
        kind: Kind::Required,
    }
}

const fn opt(key: &'static str, net: Net) -> Field {
    Field {
        key,
        net,
        kind: Kind::Optional,
    }
}

const fn always(key: &'static str, net: Net, value: Def) -> Field {
    Field {
        key,
        net,
        kind: Kind::Always(value),
    }
}

const fn def(key: &'static str, net: Net, value: Def) -> Field {
    Field {
        key,
        net,
        kind: Kind::Default(value),
    }
}

pub const DYES: &[&str] = &[
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

static EFFECT_DETAILS: [Field; 6] = [
    def("amplifier", Net::UByte, Def::Int(0)),
    def("duration", Net::VarInt, Def::Int(0)),
    def("ambient", Net::Bool, Def::Bool(false)),
    def("show_particles", Net::Bool, Def::Bool(true)),
    always("show_icon", Net::Bool, Def::SameAs("show_particles")),
    opt("hidden_effect", Net::Fields(&EFFECT_DETAILS)),
];

static EFFECT: [Field; 7] = [
    req("id", Net::Holder("minecraft:mob_effect")),
    def("amplifier", Net::UByte, Def::Int(0)),
    def("duration", Net::VarInt, Def::Int(0)),
    def("ambient", Net::Bool, Def::Bool(false)),
    def("show_particles", Net::Bool, Def::Bool(true)),
    always("show_icon", Net::Bool, Def::SameAs("show_particles")),
    opt("hidden_effect", Net::Fields(&EFFECT_DETAILS)),
];

static POTION: [Field; 4] = [
    opt("potion", Net::Holder("minecraft:potion")),
    opt("custom_color", Net::Int),
    def(
        "custom_effects",
        Net::List(&Net::Fields(&EFFECT)),
        Def::EmptyList,
    ),
    opt("custom_name", Net::Str),
];

static EXPLOSION: [Field; 5] = [
    req(
        "shape",
        Net::Enum(&["small_ball", "large_ball", "star", "creeper", "burst"]),
    ),
    def("colors", Net::IntList, Def::EmptyList),
    def("fade_colors", Net::IntList, Def::EmptyList),
    def("has_trail", Net::Bool, Def::Bool(false)),
    def("has_twinkle", Net::Bool, Def::Bool(false)),
];

static GLOBAL_POS: [Field; 2] = [req("dimension", Net::Str), req("pos", Net::BlockPos)];

static BEE: [Field; 3] = [
    req("entity_data", Net::Typed("minecraft:entity_type")),
    req("ticks_in_hive", Net::VarInt),
    req("min_ticks_in_hive", Net::VarInt),
];

static MODIFIER: [Field; 6] = [
    req("type", Net::Holder("minecraft:attribute")),
    req("id", Net::Str),
    req("amount", Net::Double),
    req(
        "operation",
        Net::Enum(&["add_value", "add_multiplied_base", "add_multiplied_total"]),
    ),
    def(
        "slot",
        Net::Enum(&[
            "any", "mainhand", "offhand", "hand", "feet", "legs", "chest", "head", "armor", "body",
            "saddle",
        ]),
        Def::Str("any"),
    ),
    opt("display", Net::Display),
];

static MAP_POST: Net = Net::Enum(&["lock", "scale"]);

static MAP_DECOR: [Field; 4] = [
    req("type", Net::Holder("minecraft:map_decoration_type")),
    req("x", Net::Double),
    req("z", Net::Double),
    req("rotation", Net::Float),
];
static MAP_DECORATIONS: Net = Net::Map(&Net::Str, &Net::Fields(&MAP_DECOR));

/// A component's value as it goes over the wire, if we know its encoding.
pub fn encode_value(name: &str, value: &Tag) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    write(codec(name)?, value, &mut out)?;
    Some(out)
}

/// The encoding of component `name`, if we know it.
pub fn codec(name: &str) -> Option<&'static Net> {
    static ENCHANTMENTS: Net = Net::Map(&Net::Holder("minecraft:enchantment"), &Net::VarInt);
    static TRIM: [Field; 2] = [
        req("material", Net::HolderPlus("minecraft:trim_material")),
        req("pattern", Net::HolderPlus("minecraft:trim_pattern")),
    ];
    static WRITABLE: [Field; 1] = [def(
        "pages",
        Net::List(&Net::Filterable(&Net::Str)),
        Def::EmptyList,
    )];
    static WRITTEN: [Field; 5] = [
        req("title", Net::Filterable(&Net::Str)),
        req("author", Net::Str),
        def("generation", Net::VarInt, Def::Int(0)),
        def(
            "pages",
            Net::List(&Net::Filterable(&Net::Text)),
            Def::EmptyList,
        ),
        def("resolved", Net::Bool, Def::Bool(false)),
    ];
    static FIREWORKS: [Field; 2] = [
        def("flight_duration", Net::UByte, Def::Int(0)),
        def(
            "explosions",
            Net::List(&Net::Fields(&EXPLOSION)),
            Def::EmptyList,
        ),
    ];
    static BANNER: Net = Net::List(&Net::Fields(&[
        req("pattern", Net::HolderPlus("minecraft:banner_pattern")),
        req("color", Net::Enum(DYES)),
    ]));
    static STEW: Net = Net::List(&Net::Fields(&[
        req("id", Net::Holder("minecraft:mob_effect")),
        def("duration", Net::VarInt, Def::Int(160)),
    ]));
    static TOOLTIP: [Field; 2] = [
        def("hide_tooltip", Net::Bool, Def::Bool(false)),
        def(
            "hidden_components",
            Net::List(&Net::Holder(COMPONENT_TYPES)),
            Def::EmptyList,
        ),
    ];
    static MODEL_DATA: [Field; 4] = [
        def("floats", Net::List(&Net::Float), Def::EmptyList),
        def("flags", Net::List(&Net::Bool), Def::EmptyList),
        def("strings", Net::List(&Net::Str), Def::EmptyList),
        def("colors", Net::List(&Net::Int), Def::EmptyList),
    ];
    static LODESTONE: [Field; 2] = [
        opt("target", Net::Fields(&GLOBAL_POS)),
        def("tracked", Net::Bool, Def::Bool(true)),
    ];
    static RESISTANT: [Field; 1] = [req("types", Net::HolderSet("minecraft:damage_type"))];
    static COOLDOWN: [Field; 2] = [req("seconds", Net::Float), opt("cooldown_group", Net::Str)];
    static ENCHANTABLE: [Field; 1] = [req("value", Net::VarInt)];
    static REPAIRABLE: [Field; 1] = [req("items", Net::HolderSet(ITEMS))];
    static POT_DECORATIONS: [Field; 4] = [
        opt("back", Net::Template),
        opt("left", Net::Template),
        opt("right", Net::Template),
        opt("front", Net::Template),
    ];
    let name = full_name(name);
    let short = name.strip_prefix("minecraft:")?;
    static N1: Net = Net::Nbt;
    static N2: Net = Net::VarInt;
    static N3: Net = Net::Unit;
    static N4: Net = Net::Text;
    static N5: Net = Net::Str;
    static N6: Net = Net::List(&Net::Text);
    static N7: Net = Net::Enum(&["common", "uncommon", "rare", "epic"]);
    static N8: Net = Net::Bool;
    static N9: Net = Net::Int;
    static N10: Net = Net::Potion;
    static N11: Net = Net::Fields(&TRIM);
    static N12: Net = Net::Fields(&WRITABLE);
    static N13: Net = Net::Fields(&WRITTEN);
    static N14: Net = Net::Container;
    static N15: Net = Net::List(&Net::Template);
    static N16: Net = Net::Fields(&FIREWORKS);
    static N17: Net = Net::Fields(&EXPLOSION);
    static N18: Net = Net::Enum(DYES);
    static N19: Net = Net::Profile;
    static N20: Net = Net::Fields(&TOOLTIP);
    static N21: Net = Net::Fields(&MODEL_DATA);
    static N22: Net = Net::Fields(&LODESTONE);
    static N23: Net = Net::Typed("minecraft:block_entity_type");
    static N24: Net = Net::Typed("minecraft:entity_type");
    static N25: Net = Net::HolderPlus("minecraft:jukebox_song");
    static N26: Net = Net::List(&Net::Fields(&BEE));
    static N27: Net = Net::Map(&Net::Str, &Net::Str);
    static N28: Net = Net::List(&Net::Fields(&MODIFIER));
    static N29: Net = Net::Fields(&RESISTANT);
    static N30: Net = Net::Fields(&COOLDOWN);
    static N31: Net = Net::Fields(&ENCHANTABLE);
    static N32: Net = Net::Fields(&REPAIRABLE);
    static N33: Net = Net::Fields(&POT_DECORATIONS);
    Some(match short {
        "custom_data"
        | "bucket_entity_data"
        | "lock"
        | "container_loot"
        | "recipes"
        | "debug_stick_state"
        | "intangible_projectile" => &N1,
        "max_stack_size"
        | "max_damage"
        | "damage"
        | "repair_cost"
        | "map_id"
        | "ominous_bottle_amplifier" => &N2,
        "unbreakable" | "glider" | "creative_slot_lock" => &N3,
        "custom_name" | "item_name" => &N4,
        "item_model" | "note_block_sound" | "tooltip_style" => &N5,
        "lore" => &N6,
        "rarity" => &N7,
        "enchantments" | "stored_enchantments" => &ENCHANTMENTS,
        "enchantment_glint_override" => &N8,
        "dyed_color" | "map_color" => &N9,
        "potion_contents" => &N10,
        "trim" => &N11,
        "writable_book_content" => &N12,
        "written_book_content" => &N13,
        "container" => &N14,
        "bundle_contents" | "charged_projectiles" => &N15,
        "fireworks" => &N16,
        "firework_explosion" => &N17,
        "banner_patterns" => &BANNER,
        "map_post_processing" => &MAP_POST,
        "map_decorations" => &MAP_DECORATIONS,
        "base_color" => &N18,
        "suspicious_stew_effects" => &STEW,
        "profile" => &N19,
        "tooltip_display" => &N20,
        "custom_model_data" => &N21,
        "lodestone_tracker" => &N22,
        "block_entity_data" => &N23,
        "entity_data" => &N24,
        "jukebox_playable" => &N25,
        "bees" => &N26,
        "block_state" => &N27,
        "attribute_modifiers" => &N28,
        "damage_resistant" => &N29,
        "use_cooldown" => &N30,
        "enchantable" => &N31,
        "repairable" => &N32,
        "pot_decorations" => &N33,
        _ => return None,
    })
}

fn int_of(tag: &Tag) -> Option<i32> {
    match tag {
        Tag::Byte(v) => Some(i32::from(*v)),
        Tag::Short(v) => Some(i32::from(*v)),
        Tag::Int(v) => Some(*v),
        Tag::Long(v) => Some(*v as i32),
        Tag::Float(v) => Some(*v as i32),
        Tag::Double(v) => Some(*v as i32),
        _ => None,
    }
}

fn f64_of(tag: &Tag) -> Option<f64> {
    match tag {
        Tag::Float(v) => Some(f64::from(*v)),
        Tag::Double(v) => Some(*v),
        _ => int_of(tag).map(f64::from),
    }
}

fn list_of(tag: &Tag) -> Option<Vec<Tag>> {
    Some(match tag {
        Tag::List(l) => l.clone(),
        Tag::IntArray(a) => a.iter().map(|&v| Tag::Int(v)).collect(),
        Tag::ByteArray(a) => a.iter().map(|&v| Tag::Byte(v)).collect(),
        Tag::LongArray(a) => a.iter().map(|&v| Tag::Long(v)).collect(),
        _ => return None,
    })
}

fn str_of(tag: &Tag) -> Option<&str> {
    match tag {
        Tag::String(s) => Some(s),
        _ => None,
    }
}

/// The saved value of a field's default.
fn default_tag(field: &Field, around: &Compound) -> Option<Tag> {
    let (Kind::Default(d) | Kind::Always(d)) = &field.kind else {
        return None;
    };
    Some(match d {
        Def::Bool(b) => Tag::Byte(i8::from(*b)),
        Def::Int(i) => match field.net {
            Net::UByte => Tag::Byte(*i as i8),
            _ => Tag::Int(*i),
        },
        Def::EmptyList => match field.net {
            Net::IntList => Tag::IntArray(Vec::new()),
            _ => Tag::List(Vec::new()),
        },
        Def::Str(s) => Tag::String((*s).to_owned()),
        Def::SameAs(other) => around.get(other).cloned().unwrap_or(Tag::Byte(1)),
    })
}

/// Encodes a saved value; `None` if it does not have the expected shape.
fn write(net: &Net, tag: &Tag, out: &mut Vec<u8>) -> Option<()> {
    match net {
        Net::Unit => {}
        Net::Bool => out.put_bool(int_of(tag)? != 0),
        Net::UByte => out.put_varint(int_of(tag)? & 0xFF),
        Net::VarInt => out.put_varint(int_of(tag)?),
        Net::Int => out.put_i32(int_of(tag)?),
        Net::Long => out.put_i64(match tag {
            Tag::Long(v) => *v,
            t => i64::from(int_of(t)?),
        }),
        Net::Float => out.put_f32(f64_of(tag)? as f32),
        Net::Double => out.put_f64(f64_of(tag)?),
        Net::Str => out.put_string(str_of(tag)?),
        Net::Nbt | Net::Text => put_nbt(out, tag),
        Net::Holder(registry) => out.put_varint(registry_id(registry, str_of(tag)?)?),
        Net::HolderPlus(registry) => out.put_varint(registry_id(registry, str_of(tag)?)? + 1),
        Net::Enum(names) => {
            let s = str_of(tag)?;
            let s = s.strip_prefix("minecraft:").unwrap_or(s);
            out.put_varint(names.iter().position(|n| *n == s)? as i32);
        }
        Net::List(inner) => {
            let list = list_of(tag)?;
            out.put_varint(list.len() as i32);
            for e in &list {
                write(inner, e, out)?;
            }
        }
        Net::IntList => {
            let list = list_of(tag)?;
            out.put_varint(list.len() as i32);
            for e in &list {
                out.put_i32(int_of(e)?);
            }
        }
        Net::Map(key, value) => {
            let Tag::Compound(c) = tag else { return None };
            out.put_varint(c.len() as i32);
            for (k, v) in c.iter() {
                write(key, &Tag::String(k.to_owned()), out)?;
                write(value, v, out)?;
            }
        }
        Net::Fields(fields) => {
            let Tag::Compound(c) = tag else { return None };
            write_fields(fields, c, out)?;
        }
        Net::Template => {
            let Tag::Compound(c) = tag else { return None };
            let stack = ItemStack::from_nbt(c)?;
            out.put_varint(stack.item);
            out.put_varint(stack.count);
            stack.components.encode(stack.damage, out);
        }
        Net::Container => {
            let list = list_of(tag)?;
            let mut slots: Vec<Option<ItemStack>> = Vec::new();
            for e in &list {
                let Tag::Compound(e) = e else { return None };
                let slot = usize::try_from(int_of(e.get("slot")?)?).ok()?;
                let Some(Tag::Compound(item)) = e.get("item") else {
                    return None;
                };
                if slot >= 256 {
                    return None;
                }
                if slots.len() <= slot {
                    slots.resize(slot + 1, None);
                }
                slots[slot] = Some(ItemStack::from_nbt(item)?);
            }
            out.put_varint(slots.len() as i32);
            for s in &slots {
                out.put_bool(s.is_some());
                if let Some(s) = s {
                    out.put_varint(s.item);
                    out.put_varint(s.count);
                    s.components.encode(s.damage, out);
                }
            }
        }
        Net::Filterable(inner) => match tag {
            Tag::Compound(c) if c.get("raw").is_some() => {
                write(inner, c.get("raw")?, out)?;
                match c.get("filtered") {
                    Some(f) => {
                        out.put_bool(true);
                        write(inner, f, out)?;
                    }
                    None => out.put_bool(false),
                }
            }
            t => {
                write(inner, t, out)?;
                out.put_bool(false);
            }
        },
        Net::Potion => {
            let c = match tag {
                Tag::String(s) => {
                    let mut c = Compound::new();
                    c.insert("potion", Tag::String(s.clone()));
                    c
                }
                Tag::Compound(c) => c.clone(),
                _ => return None,
            };
            write_fields(&POTION, &c, out)?;
        }
        Net::Profile => {
            out.put_varint(0);
            let (name, id, properties) = match tag {
                Tag::String(s) => (Some(s.as_str()), None, None),
                Tag::Compound(c) => (
                    c.get("name").and_then(str_of),
                    c.get("id"),
                    c.get("properties"),
                ),
                _ => return None,
            };
            out.put_bool(name.is_some());
            if let Some(n) = name {
                out.put_string(n);
            }
            match id {
                Some(Tag::IntArray(a)) if a.len() == 4 => {
                    out.put_bool(true);
                    let uuid = a
                        .iter()
                        .fold(0u128, |u, &p| (u << 32) | u128::from(p as u32));
                    out.put_uuid(uuid);
                }
                _ => out.put_bool(false),
            }
            let props = properties.and_then(list_of).unwrap_or_default();
            out.put_varint(props.len() as i32);
            for p in &props {
                let Tag::Compound(p) = p else { return None };
                out.put_string(p.get("name").and_then(str_of)?);
                out.put_string(p.get("value").and_then(str_of)?);
                match p.get("signature").and_then(str_of) {
                    Some(s) => {
                        out.put_bool(true);
                        out.put_string(s);
                    }
                    None => out.put_bool(false),
                }
            }
            // Skin texture, cape, elytra, model: not overridden.
            for _ in 0..4 {
                out.put_bool(false);
            }
        }
        Net::Typed(registry) => {
            let Tag::Compound(c) = tag else { return None };
            let id = c.get("id").and_then(str_of)?;
            out.put_varint(registry_id(registry, id)?);
            let mut rest = c.clone();
            rest.remove("id");
            put_nbt(out, &Tag::Compound(rest));
        }
        Net::BlockPos => {
            let list = list_of(tag)?;
            let [x, y, z] = [list.first()?, list.get(1)?, list.get(2)?].map(int_of);
            out.put_i64(Position::new(x?, y?, z?).pack());
        }
        Net::HolderSet(registry) => match tag {
            Tag::String(s) if s.starts_with('#') => {
                out.put_varint(0);
                out.put_string(&full_name(&s[1..]));
            }
            Tag::String(s) => {
                out.put_varint(2);
                out.put_varint(registry_id(registry, s)?);
            }
            t => {
                let list = list_of(t)?;
                out.put_varint(list.len() as i32 + 1);
                for e in &list {
                    out.put_varint(registry_id(registry, str_of(e)?)?);
                }
            }
        },
        Net::Display => {
            let Tag::Compound(c) = tag else { return None };
            match c.get("type").and_then(str_of).unwrap_or("default") {
                "default" => out.put_varint(0),
                "hidden" => out.put_varint(1),
                "override" => {
                    out.put_varint(2);
                    put_nbt(out, c.get("value")?);
                }
                _ => return None,
            }
        }
    }
    Some(())
}

fn write_fields(fields: &[Field], c: &Compound, out: &mut Vec<u8>) -> Option<()> {
    for f in fields {
        match &f.kind {
            Kind::Required => write(&f.net, c.get(f.key)?, out)?,
            Kind::Default(_) | Kind::Always(_) => match c.get(f.key) {
                Some(v) => write(&f.net, v, out)?,
                None => write(&f.net, &default_tag(f, c)?, out)?,
            },
            Kind::Optional => match c.get(f.key) {
                Some(v) => {
                    out.put_bool(true);
                    write(&f.net, v, out)?;
                }
                None => out.put_bool(false),
            },
        }
    }
    Some(())
}

const MAX_DEPTH: u32 = 16;
const MAX_LIST: usize = 1024;

fn bad(what: &'static str) -> DecodeError {
    DecodeError::Invalid(what)
}

fn read_len(r: &mut Reader) -> Result<usize> {
    let n = r.length_prefix()?;
    if n > MAX_LIST {
        return Err(bad("component list length"));
    }
    Ok(n)
}

/// Decodes a value into its saved form.
fn read(net: &Net, r: &mut Reader, depth: u32) -> Result<Tag> {
    if depth > MAX_DEPTH {
        return Err(bad("component nesting"));
    }
    let name_of = |registry: &str, id: i32| {
        registry_name(registry, id)
            .map(|n| Tag::String(n.to_owned()))
            .ok_or(bad("registry id"))
    };
    Ok(match net {
        Net::Unit => Tag::Compound(Compound::new()),
        Net::Bool => Tag::Byte(i8::from(r.bool()?)),
        Net::UByte => Tag::Byte(r.varint()? as i8),
        Net::VarInt => Tag::Int(r.varint()?),
        Net::Int => Tag::Int(r.i32()?),
        Net::Long => Tag::Long(r.i64()?),
        Net::Float => Tag::Float(r.f32()?),
        Net::Double => Tag::Double(r.f64()?),
        Net::Str => Tag::String(r.string(32767)?),
        Net::Nbt | Net::Text => r.nbt()?,
        Net::Holder(registry) => name_of(registry, r.varint()?)?,
        Net::HolderPlus(registry) => {
            let id = r.varint()?;
            if id == 0 {
                return Err(bad("inline registry entry"));
            }
            name_of(registry, id - 1)?
        }
        Net::Enum(names) => {
            let i = r.varint()?;
            let n = usize::try_from(i)
                .ok()
                .and_then(|i| names.get(i))
                .ok_or(bad("enum index"))?;
            Tag::String((*n).to_owned())
        }
        Net::List(inner) => {
            let n = read_len(r)?;
            let mut list = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                list.push(read(inner, r, depth + 1)?);
            }
            Tag::List(list)
        }
        Net::IntList => {
            let n = read_len(r)?;
            let mut list = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                list.push(r.i32()?);
            }
            Tag::IntArray(list)
        }
        Net::Map(key, value) => {
            let n = read_len(r)?;
            let mut c = Compound::new();
            for _ in 0..n {
                let Tag::String(k) = read(key, r, depth + 1)? else {
                    return Err(bad("map key"));
                };
                c.insert(k, read(value, r, depth + 1)?);
            }
            Tag::Compound(c)
        }
        Net::Fields(fields) => Tag::Compound(read_fields(fields, r, depth)?),
        Net::Template => Tag::Compound(read_template(r)?),
        Net::Container => {
            let n = read_len(r)?;
            let mut list = Vec::new();
            for slot in 0..n {
                if r.bool()? {
                    let mut e = Compound::new();
                    e.insert("slot", Tag::Int(slot as i32));
                    e.insert("item", Tag::Compound(read_template(r)?));
                    list.push(Tag::Compound(e));
                }
            }
            Tag::List(list)
        }
        Net::Filterable(inner) => {
            let mut c = Compound::new();
            c.insert("raw", read(inner, r, depth + 1)?);
            if r.bool()? {
                c.insert("filtered", read(inner, r, depth + 1)?);
            }
            Tag::Compound(c)
        }
        Net::Potion => Tag::Compound(read_fields(&POTION, r, depth)?),
        Net::Profile => {
            if r.varint()? != 0 {
                return Err(bad("resolved profile"));
            }
            let mut c = Compound::new();
            if r.bool()? {
                c.insert("name", Tag::String(r.string(16)?));
            }
            if r.bool()? {
                let uuid = r.uuid()?;
                c.insert(
                    "id",
                    Tag::IntArray(
                        (0..4)
                            .map(|i| (uuid >> (96 - 32 * i)) as u32 as i32)
                            .collect(),
                    ),
                );
            }
            let n = read_len(r)?;
            let mut props = Vec::new();
            for _ in 0..n {
                let mut p = Compound::new();
                p.insert("name", Tag::String(r.string(64)?));
                p.insert("value", Tag::String(r.string(32767)?));
                if r.bool()? {
                    p.insert("signature", Tag::String(r.string(1024)?));
                }
                props.push(Tag::Compound(p));
            }
            if !props.is_empty() {
                c.insert("properties", Tag::List(props));
            }
            for _ in 0..4 {
                if r.bool()? {
                    return Err(bad("profile skin override"));
                }
            }
            Tag::Compound(c)
        }
        Net::Typed(registry) => {
            let id = name_of(registry, r.varint()?)?;
            let mut c = Compound::new();
            c.insert("id", id);
            if let Tag::Compound(rest) = r.nbt()? {
                for (k, v) in rest.iter() {
                    c.insert(k, v.clone());
                }
            }
            Tag::Compound(c)
        }
        Net::BlockPos => {
            let p = r.position()?;
            Tag::IntArray(vec![p.x, p.y, p.z])
        }
        Net::HolderSet(registry) => {
            let n = r.varint()?;
            if n == 0 {
                Tag::String(format!("#{}", r.string(32767)?))
            } else {
                let mut list = Vec::new();
                for _ in 0..(n - 1).min(MAX_LIST as i32) {
                    list.push(name_of(registry, r.varint()?)?);
                }
                if list.len() == 1 {
                    list.pop().expect("one entry")
                } else {
                    Tag::List(list)
                }
            }
        }
        Net::Display => {
            let mut c = Compound::new();
            match r.varint()? {
                0 => c.insert("type", Tag::from("default")),
                1 => c.insert("type", Tag::from("hidden")),
                2 => {
                    c.insert("type", Tag::from("override"));
                    c.insert("value", r.nbt()?)
                }
                _ => return Err(bad("modifier display")),
            };
            Tag::Compound(c)
        }
    })
}

fn read_fields(fields: &[Field], r: &mut Reader, depth: u32) -> Result<Compound> {
    let mut c = Compound::new();
    for f in fields {
        match &f.kind {
            Kind::Required => {
                c.insert(f.key, read(&f.net, r, depth + 1)?);
            }
            Kind::Default(_) => {
                let v = read(&f.net, r, depth + 1)?;
                if !default_tag(f, &c).is_some_and(|d| tag_eq(&d, &v)) {
                    c.insert(f.key, v);
                }
            }
            Kind::Always(_) => {
                c.insert(f.key, read(&f.net, r, depth + 1)?);
            }
            Kind::Optional => {
                if r.bool()? {
                    c.insert(f.key, read(&f.net, r, depth + 1)?);
                }
            }
        }
    }
    Ok(c)
}

fn read_template(r: &mut Reader) -> Result<Compound> {
    let item = r.varint()?;
    let count = r.varint()?;
    let (damage, components) = Components::decode(r)?;
    let stack = ItemStack {
        count,
        item,
        damage,
        components,
    };
    stack.to_template_nbt().ok_or(bad("item id"))
}

// ---------------------------------------------------------------------------
// Hashes

/// CRC32C (Castagnoli), as the client hashes component data.
pub fn crc32c(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0x82F6_3B78 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    let mut c = !0u32;
    for &b in bytes {
        c = TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

// Markers the client puts before each kind of value it hashes.
const H_MAP: u8 = 2;
const H_MAP_END: u8 = 3;
const H_LIST: u8 = 4;
const H_LIST_END: u8 = 5;
const H_BYTE: u8 = 6;
const H_SHORT: u8 = 7;
const H_INT: u8 = 8;
const H_LONG: u8 = 9;
const H_FLOAT: u8 = 10;
const H_DOUBLE: u8 = 11;
const H_STRING: u8 = 12;
const H_BOOL: u8 = 13;
const H_BYTES: u8 = 14;
const H_BYTES_END: u8 = 15;
const H_INTS: u8 = 16;
const H_INTS_END: u8 = 17;
const H_LONGS: u8 = 18;
const H_LONGS_END: u8 = 19;

/// The hash a client sends for an integer component (the damage): the
/// int marker and the value, little-endian, through CRC32C.
pub fn int_component_hash(value: i32) -> i32 {
    let mut bytes = [H_INT; 5];
    bytes[1..].copy_from_slice(&value.to_le_bytes());
    crc32c(&bytes) as i32
}

/// The hash of a component's saved value, as clients send it.
pub fn hash_component(name: &str, value: &Tag) -> i32 {
    hash(codec(name), value) as i32
}

const TEXT_FLAGS: &[&str] = &[
    "bold",
    "italic",
    "underlined",
    "strikethrough",
    "obfuscated",
    "interpret",
];

fn hash_bool(b: bool) -> u32 {
    crc32c(&[H_BOOL, u8::from(b)])
}

fn hash_list(items: impl Iterator<Item = u32>) -> u32 {
    let mut bytes = vec![H_LIST];
    for h in items {
        bytes.extend_from_slice(&h.to_le_bytes());
    }
    bytes.push(H_LIST_END);
    crc32c(&bytes)
}

fn hash_map(mut entries: Vec<(u32, u32)>) -> u32 {
    entries.sort_unstable();
    let mut bytes = vec![H_MAP];
    for (k, v) in entries {
        bytes.extend_from_slice(&k.to_le_bytes());
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.push(H_MAP_END);
    crc32c(&bytes)
}

fn hash_str(s: &str) -> u32 {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut bytes = vec![H_STRING];
    bytes.extend_from_slice(&(units.len() as i32).to_le_bytes());
    for u in units {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    crc32c(&bytes)
}

/// Hash of a value, typed by `net` where it says more than the NBT does
/// (booleans saved as bytes, int lists saved as int arrays).
fn hash(net: Option<&Net>, tag: &Tag) -> u32 {
    match (net, tag) {
        (Some(Net::Bool), t) if int_of(t).is_some() => hash_bool(int_of(t) != Some(0)),
        (Some(Net::Text), Tag::Compound(c)) => hash_map(
            c.iter()
                .map(|(k, v)| {
                    let h = if TEXT_FLAGS.contains(&k) {
                        hash(Some(&Net::Bool), v)
                    } else {
                        hash(Some(&Net::Text), v)
                    };
                    (hash_str(k), h)
                })
                .collect(),
        ),
        (Some(Net::Text), Tag::List(l)) => hash_list(l.iter().map(|e| hash(Some(&Net::Text), e))),
        (Some(Net::List(inner)), t) if list_of(t).is_some() => hash_list(
            list_of(t)
                .unwrap_or_default()
                .iter()
                .map(|e| hash(Some(inner), e)),
        ),
        (Some(Net::IntList), t) if list_of(t).is_some() => {
            hash_list(list_of(t).unwrap_or_default().iter().map(|e| hash(None, e)))
        }
        (Some(Net::Map(_, value)), Tag::Compound(c)) => hash_map(
            c.iter()
                .map(|(k, v)| (hash_str(k), hash(Some(value), v)))
                .collect(),
        ),
        (Some(Net::Fields(fields)), Tag::Compound(c)) => hash_fields(fields, c),
        (Some(Net::Potion), Tag::String(s)) => {
            let mut c = Compound::new();
            c.insert("potion", Tag::String(s.clone()));
            hash_fields(&POTION, &c)
        }
        (Some(Net::Potion), Tag::Compound(c)) => hash_fields(&POTION, c),
        (Some(Net::Template), Tag::Compound(c)) => hash_map(
            c.iter()
                .filter(|(k, v)| !(*k == "count" && int_of(v) == Some(1)))
                .map(|(k, v)| {
                    let h = match (k, v) {
                        ("components", Tag::Compound(comps)) => hash_map(
                            comps
                                .iter()
                                .map(|(n, v)| (hash_str(n), hash(codec(n), v)))
                                .collect(),
                        ),
                        _ => hash(None, v),
                    };
                    (hash_str(k), h)
                })
                .collect(),
        ),
        (Some(Net::Container), Tag::List(l)) => hash_list(l.iter().map(|e| {
            match e {
                Tag::Compound(c) => hash_map(
                    c.iter()
                        .map(|(k, v)| {
                            let net = (k == "item").then_some(&Net::Template);
                            (hash_str(k), hash(net, v))
                        })
                        .collect(),
                ),
                t => hash(None, t),
            }
        })),
        (Some(Net::Filterable(inner)), Tag::Compound(c)) if c.get("raw").is_some() => hash_map(
            c.iter()
                .map(|(k, v)| (hash_str(k), hash(Some(inner), v)))
                .collect(),
        ),
        (Some(Net::Filterable(inner)), t) => {
            hash_map(vec![(hash_str("raw"), hash(Some(inner), t))])
        }
        (Some(Net::Display), Tag::Compound(c)) => hash_map(
            c.iter()
                .map(|(k, v)| {
                    let net = (k == "value").then_some(&Net::Text);
                    (hash_str(k), hash(net, v))
                })
                .collect(),
        ),
        (_, Tag::Byte(v)) => crc32c(&[H_BYTE, *v as u8]),
        (_, Tag::Short(v)) => {
            let mut b = vec![H_SHORT];
            b.extend_from_slice(&v.to_le_bytes());
            crc32c(&b)
        }
        (_, Tag::Int(v)) => int_component_hash(*v) as u32,
        (_, Tag::Long(v)) => {
            let mut b = vec![H_LONG];
            b.extend_from_slice(&v.to_le_bytes());
            crc32c(&b)
        }
        (_, Tag::Float(v)) => {
            let mut b = vec![H_FLOAT];
            b.extend_from_slice(&v.to_bits().to_le_bytes());
            crc32c(&b)
        }
        (_, Tag::Double(v)) => {
            let mut b = vec![H_DOUBLE];
            b.extend_from_slice(&v.to_bits().to_le_bytes());
            crc32c(&b)
        }
        (_, Tag::String(s)) => hash_str(s),
        (_, Tag::ByteArray(a)) => {
            let mut b = vec![H_BYTES];
            b.extend(a.iter().map(|&v| v as u8));
            b.push(H_BYTES_END);
            crc32c(&b)
        }
        (_, Tag::IntArray(a)) => {
            let mut b = vec![H_INTS];
            for v in a {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.push(H_INTS_END);
            crc32c(&b)
        }
        (_, Tag::LongArray(a)) => {
            let mut b = vec![H_LONGS];
            for v in a {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.push(H_LONGS_END);
            crc32c(&b)
        }
        (_, Tag::List(l)) => hash_list(l.iter().map(|e| hash(None, e))),
        (_, Tag::Compound(c)) => hash_map(
            c.iter()
                .map(|(k, v)| (hash_str(k), hash(None, v)))
                .collect(),
        ),
    }
}

fn hash_fields(fields: &[Field], c: &Compound) -> u32 {
    hash_map(
        c.iter()
            .map(|(k, v)| {
                let net = fields.iter().find(|f| f.key == k).map(|f| &f.net);
                (hash_str(k), hash(net, v))
            })
            .collect(),
    )
}

/// An item stack in a window click: components only as hashes.
#[derive(Clone, Debug, PartialEq)]
pub struct HashedStack {
    pub item: i32,
    pub count: i32,
    /// Added components: type id and data hash.
    pub added: Vec<(i32, i32)>,
    /// Removed (default) components by type id.
    pub removed: Vec<i32>,
}

impl HashedStack {
    pub fn decode(r: &mut Reader) -> Result<Option<Self>> {
        if !r.bool()? {
            return Ok(None);
        }
        let (item, count) = (r.varint()?, r.varint()?);
        let n = r.length_prefix()?;
        let mut added = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            added.push((r.varint()?, r.i32()?));
        }
        let n = r.length_prefix()?;
        let mut removed = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            removed.push(r.varint()?);
        }
        Ok(Some(Self {
            item,
            count,
            added,
            removed,
        }))
    }

    /// The hashed form of `stack`, as a client would send it.
    pub fn of(stack: &ItemStack) -> Self {
        Self {
            item: stack.item,
            count: stack.count,
            added: stack.hashes(),
            removed: stack.removed_ids(),
        }
    }

    fn sorted(mut v: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
        v.sort_unstable();
        v
    }

    /// Whether `stack` is what this describes, the count aside.
    pub fn matches(&self, stack: &ItemStack) -> bool {
        let mut removed = self.removed.clone();
        removed.sort_unstable();
        let mut theirs = stack.removed_ids();
        theirs.sort_unstable();
        self.item == stack.item
            && removed == theirs
            && Self::sorted(self.added.clone()) == Self::sorted(stack.hashes())
    }

    /// Whether `stack` changes the same components (values aside).
    fn alike(&self, stack: &ItemStack) -> bool {
        let mut mine: Vec<i32> = self.added.iter().map(|&(t, _)| t).collect();
        mine.sort_unstable();
        let mut theirs: Vec<i32> = stack.hashes().iter().map(|&(t, _)| t).collect();
        theirs.sort_unstable();
        self.item == stack.item && mine == theirs
    }

    /// The stack, its components taken from the first of `known` it
    /// matches (the client only sends hashes); failing that, from one with
    /// the same item and kinds of change.
    pub fn resolve<'a>(&self, known: impl IntoIterator<Item = &'a ItemStack>) -> ItemStack {
        let known: Vec<&ItemStack> = known.into_iter().collect();
        let plain = self.added.is_empty() && self.removed.is_empty();
        let found = if plain {
            None
        } else {
            known
                .iter()
                .find(|s| self.matches(s))
                .or_else(|| known.iter().find(|s| self.alike(s)))
        };
        match found {
            Some(s) => s.with_count(self.count),
            None => {
                let mut stack = ItemStack::new(self.item, self.count);
                // A damage we can still read back from its hash.
                if let Some(&(_, h)) = self
                    .added
                    .iter()
                    .find(|&&(t, _)| Some(t) == registry_id(COMPONENT_TYPES, DAMAGE).or(Some(3)))
                    && let Some(d) = known
                        .iter()
                        .map(|s| s.damage)
                        .find(|&d| int_component_hash(d) == h)
                {
                    stack.damage = d;
                }
                stack
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registries() {
        let mut items = HashMap::new();
        items.insert("minecraft:stone".to_owned(), 1);
        items.insert("minecraft:dirt".to_owned(), 55);
        items.insert("minecraft:diamond_sword".to_owned(), 1050);
        items.insert("minecraft:arrow".to_owned(), 1009);
        items.insert("minecraft:brick".to_owned(), 2);
        items.insert("minecraft:angler_pottery_sherd".to_owned(), 4);
        let mut types = HashMap::new();
        for (i, n) in [
            "custom_data",
            "max_stack_size",
            "max_damage",
            "damage",
            "unbreakable",
            "use_effects",
            "custom_name",
        ]
        .iter()
        .enumerate()
        {
            types.insert(format!("minecraft:{n}"), i as i32);
        }
        types.insert("minecraft:lore".to_owned(), 11);
        types.insert("minecraft:enchantments".to_owned(), 13);
        types.insert("minecraft:repair_cost".to_owned(), 19);
        types.insert("minecraft:tool".to_owned(), 28);
        types.insert("minecraft:potion_contents".to_owned(), 53);
        types.insert("minecraft:container".to_owned(), 77);
        types.insert("minecraft:charged_projectiles".to_owned(), 51);
        types.insert("minecraft:pot_decorations".to_owned(), 76);
        let mut ench = HashMap::new();
        ench.insert("minecraft:sharpness".to_owned(), 33);
        ench.insert("minecraft:unbreaking".to_owned(), 40);
        let mut effects = HashMap::new();
        effects.insert("minecraft:speed".to_owned(), 0);
        let mut potions = HashMap::new();
        potions.insert("minecraft:swiftness".to_owned(), 13);
        let all = [
            (ITEMS, items),
            (COMPONENT_TYPES, types),
            ("minecraft:enchantment", ench),
            ("minecraft:mob_effect", effects),
            ("minecraft:potion", potions),
        ];
        install_registries(all.iter().map(|(n, m)| (*n, m)));
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Decodes vanilla's bytes, and encodes them back the same.
    fn round_trip(bytes: &str) -> ItemStack {
        registries();
        let bytes = hex(bytes);
        let stack = ItemStack::decode(&mut Reader::new(&bytes))
            .unwrap()
            .unwrap();
        let mut out = Vec::new();
        ItemStack::encode(Some(&stack), &mut out);
        let again = ItemStack::decode(&mut Reader::new(&out)).unwrap().unwrap();
        assert_eq!(again, stack);
        // Vanilla's component order is its own; the bytes match otherwise.
        if stack.components.len() + usize::from(stack.damage != 0) <= 1 {
            assert_eq!(out, bytes, "{stack:?}");
        }
        stack
    }

    #[test]
    fn vanilla_slots() {
        let s = round_trip("019a0801000d0221052803");
        let Some(Tag::Compound(e)) = s.components.get("enchantments") else {
            panic!()
        };
        assert_eq!(e.get("minecraft:sharpness"), Some(&Tag::Int(5)));
        let s = round_trip("019a08010006080003416263");
        assert_eq!(s.components.get("custom_name"), Some(&Tag::from("Abc")));
        round_trip(
            "019a0801000b020800024c310a080005636f6c6f720004626c75650800047465787400024c3200",
        );
        let s = round_trip("019a0802001307030a");
        assert_eq!(
            (s.damage, s.components.get("repair_cost")),
            (10, Some(&Tag::Int(7)))
        );
        round_trip("019a08010004");
        round_trip("019a0800011c");
        let s = round_trip("01f7090100350001000000ff010001c80100000000010178");
        let Some(Tag::Compound(p)) = s.components.get("potion_contents") else {
            panic!()
        };
        assert_eq!(p.get("custom_color"), Some(&Tag::Int(255)));
        let Some(Tag::List(effects)) = p.get("custom_effects") else {
            panic!()
        };
        let Tag::Compound(e) = &effects[0] else {
            panic!()
        };
        assert_eq!(e.get("show_particles"), Some(&Tag::Byte(0)));
        assert_eq!(e.get("show_icon"), Some(&Tag::Byte(0)));
        round_trip("01f709010035010d000000");
        round_trip("01900501004d030101020000000137010000");
        round_trip("01d30b01003301f107010000");
    }

    #[test]
    fn saved_form() {
        registries();
        let mut saved = Compound::new();
        saved.insert("id", Tag::from("minecraft:diamond_sword"));
        saved.insert("count", Tag::Int(1));
        let mut c = Compound::new();
        c.insert("minecraft:damage", Tag::Int(4));
        c.insert("minecraft:custom_name", Tag::from("x"));
        c.insert("!minecraft:tool", Tag::Compound(Compound::new()));
        c.insert("minecraft:some_mod_thing", Tag::Int(1));
        saved.insert("components", Tag::Compound(c));
        let stack = ItemStack::from_nbt(&saved).unwrap();
        assert_eq!(stack.damage, 4);
        assert!(stack.components.is_removed("tool"));
        let back = stack.to_nbt().unwrap();
        assert!(tag_eq(&Tag::Compound(back), &Tag::Compound(saved)));
    }

    #[test]
    fn hashed_resolve() {
        registries();
        let mut a = ItemStack::new(1050, 1);
        a.components.set("custom_name", Tag::from("A"));
        let mut b = ItemStack::new(1050, 1);
        b.components.set("custom_name", Tag::from("B"));
        let h = HashedStack::of(&b);
        assert_eq!(h.resolve([&a, &b]), b);
        let plain = HashedStack::of(&ItemStack::new(1050, 3));
        assert_eq!(plain.resolve([&a]), ItemStack::new(1050, 3));
    }

    #[test]
    fn pot_decorations_round_trip() {
        registries();
        let side = |id: &str| {
            let mut c = Compound::new();
            c.insert("id", Tag::String(id.to_owned()));
            c.insert("count", Tag::Int(1));
            Tag::Compound(c)
        };
        let mut dec = Compound::new();
        dec.insert("back", side("minecraft:brick"));
        dec.insert("left", side("minecraft:angler_pottery_sherd"));
        let mut stack = ItemStack::new(1, 1);
        stack.components.set("pot_decorations", Tag::Compound(dec));
        let mut out = Vec::new();
        ItemStack::encode(Some(&stack), &mut out);
        let again = ItemStack::decode(&mut Reader::new(&out)).unwrap().unwrap();
        let Some(Tag::Compound(got)) = again.components.get("pot_decorations") else {
            panic!("missing decorations");
        };
        let Tag::Compound(left) = got.get("left").unwrap() else {
            panic!("left");
        };
        assert_eq!(
            left.get("id"),
            Some(&Tag::String("minecraft:angler_pottery_sherd".into()))
        );
        assert!(got.get("front").is_none());
        assert!(got.get("right").is_none());
    }

    #[test]
    fn known_int_hash() {
        // The damage hash the client sends for damage 1 (seen in clicks).
        assert_eq!(int_component_hash(0), crc32c(&[8, 0, 0, 0, 0]) as i32);
    }
}
