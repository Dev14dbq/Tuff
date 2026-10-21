//! Player data files (`players/data/<uuid>.dat`, gzip NBT), as vanilla 26.3
//! writes them (seen with `cargo xtask chunk-info`): `Pos`, `Rotation`,
//! `playerGameType`, `SelectedItemSlot`, `Inventory` (`{Slot, id, count,
//! components}`, slots 0–35 by player inventory index) and `equipment`
//! (`head`, `chest`, `legs`, `feet`, `offhand`).
//!
//! Saving patches the stored file, so fields we do not model yet (health,
//! food, experience, effects, ender chest) are kept.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::ItemStack;

use crate::context::Pose;
use crate::interact::Hands;
use crate::util::uuid_string;

/// What we keep about a player between sessions.
#[derive(Clone, Debug)]
pub struct Saved {
    pub pose: Pose,
    /// `minecraft:overworld`, `minecraft:the_nether` or `minecraft:the_end`.
    pub dimension: String,
    pub game_mode: i32,
    /// `abilities.flying`.
    pub flying: bool,
    pub hands: Hands,
    pub vitals: crate::vitals::Vitals,
    pub extra: Extra,
}

/// A respawn point (`respawn` of a player, `spawn` of level.dat).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Respawn {
    pub dimension: String,
    pub pos: (i32, i32, i32),
    pub yaw: f32,
    pub pitch: f32,
    /// Set by `/spawnpoint`: no bed or anchor needed.
    pub forced: bool,
}

impl Respawn {
    pub fn to_nbt(&self, with_forced: bool) -> Compound {
        let mut c = Compound::new();
        c.insert("dimension", Tag::String(self.dimension.clone()));
        c.insert(
            "pos",
            Tag::IntArray(vec![self.pos.0, self.pos.1, self.pos.2]),
        );
        c.insert("yaw", Tag::Float(self.yaw));
        c.insert("pitch", Tag::Float(self.pitch));
        if with_forced {
            c.insert("forced", Tag::Byte(i8::from(self.forced)));
        }
        c
    }

    pub fn from_nbt(c: &Compound) -> Option<Self> {
        let Some(Tag::IntArray(pos)) = c.get("pos") else {
            return None;
        };
        let [x, y, z] = pos[..] else { return None };
        let float = |k: &str| match c.get(k) {
            Some(Tag::Float(v)) => *v,
            _ => 0.0,
        };
        Some(Respawn {
            dimension: match c.get("dimension") {
                Some(Tag::String(d)) => d.clone(),
                _ => "minecraft:overworld".into(),
            },
            pos: (x, y, z),
            yaw: float("yaw"),
            pitch: float("pitch"),
            forced: matches!(c.get("forced"), Some(Tag::Byte(1))),
        })
    }
}

/// What a player keeps besides their body and items.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extra {
    pub respawn: Option<Respawn>,
    /// `/tag` (`Tags`).
    pub tags: Vec<String>,
}

/// Container slot of each equipment key.
const EQUIPMENT: [(&str, usize); 5] = [
    ("head", 5),
    ("chest", 6),
    ("legs", 7),
    ("feet", 8),
    ("offhand", 45),
];

/// Player inventory index (0–35) → container slot.
fn container_slot(index: i32) -> Option<usize> {
    match index {
        0..=8 => Some(36 + index as usize),
        9..=35 => Some(index as usize),
        _ => None,
    }
}

#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    item_ids: HashMap<String, i32>,
    item_names: HashMap<i32, String>,
}

impl Store {
    pub fn new(world: &Path, items: &HashMap<String, i32>) -> Self {
        Self {
            dir: world.join("players/data"),
            item_ids: items.clone(),
            item_names: items.iter().map(|(n, &i)| (i, n.clone())).collect(),
        }
    }

    fn path(&self, uuid: u128) -> PathBuf {
        self.dir.join(format!("{}.dat", uuid_string(uuid)))
    }

    fn read(&self, uuid: u128) -> Result<Option<Compound>> {
        let path = self.path(uuid);
        let raw = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut bytes)?;
        match mc_nbt::read_named(&bytes)? {
            (_, Tag::Compound(root), _) => Ok(Some(root)),
            _ => bail!("{}: root is not a compound", path.display()),
        }
    }

    fn stack(&self, entry: &Compound) -> Option<(ItemStack, Option<Tag>)> {
        let Some(Tag::String(id)) = entry.get("id") else {
            return None;
        };
        let count = match entry.get("count") {
            Some(Tag::Int(c)) => *c,
            Some(Tag::Byte(c)) => i32::from(*c),
            _ => 1,
        };
        let (damage, components) = crate::items::split_damage(entry.get("components"));
        Some((
            ItemStack {
                count,
                item: *self.item_ids.get(id)?,
                damage,
                has_component_changes: components.is_some(),
            },
            components,
        ))
    }

    /// The player's saved state, if they played here before.
    pub fn load(&self, uuid: u128) -> Result<Option<Saved>> {
        let Some(root) = self.read(uuid)? else {
            return Ok(None);
        };
        let doubles = |key: &str| match root.get(key) {
            Some(Tag::List(list)) => list
                .iter()
                .map(|t| match t {
                    Tag::Double(v) => *v,
                    Tag::Float(v) => f64::from(*v),
                    _ => 0.0,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let pos = doubles("Pos");
        let rot = doubles("Rotation");
        let [x, y, z] = pos[..] else {
            bail!("player file without a position");
        };
        let pose = Pose {
            x,
            y,
            z,
            yaw: rot.first().copied().unwrap_or(0.0) as f32,
            pitch: rot.get(1).copied().unwrap_or(0.0) as f32,
            on_ground: matches!(root.get("OnGround"), Some(Tag::Byte(1))),
        };
        let game_mode = match root.get("playerGameType") {
            Some(Tag::Int(m)) => *m,
            _ => 0,
        };
        let flying = match root.get("abilities") {
            Some(Tag::Compound(a)) => matches!(a.get("flying"), Some(Tag::Byte(1))),
            _ => false,
        };
        let mut hands = Hands::default();
        if let Some(Tag::Int(selected)) = root.get("SelectedItemSlot") {
            hands.selected = (*selected).clamp(0, 8) as usize;
        }
        if let Some(Tag::List(items)) = root.get("Inventory") {
            for item in items {
                let Tag::Compound(entry) = item else { continue };
                let index = match entry.get("Slot") {
                    Some(Tag::Byte(s)) => i32::from(*s),
                    _ => continue,
                };
                if let (Some(slot), Some((stack, components))) =
                    (container_slot(index), self.stack(entry))
                {
                    hands.slots[slot] = Some(stack);
                    if let Some(components) = components {
                        hands.components.insert(slot, components);
                    }
                }
            }
        }
        if let Some(Tag::Compound(equipment)) = root.get("equipment") {
            for (key, slot) in EQUIPMENT {
                if let Some(Tag::Compound(entry)) = equipment.get(key)
                    && let Some((stack, components)) = self.stack(entry)
                {
                    hands.slots[slot] = Some(stack);
                    if let Some(components) = components {
                        hands.components.insert(slot, components);
                    }
                }
            }
        }
        let mut vitals = crate::vitals::Vitals::default();
        if let Some(Tag::Float(h)) = root.get("Health") {
            vitals.health = *h;
        }
        if let Some(Tag::Int(f)) = root.get("foodLevel") {
            vitals.food = *f;
        }
        if let Some(Tag::Float(s)) = root.get("foodSaturationLevel") {
            vitals.saturation = *s;
        }
        if let Some(Tag::Float(e)) = root.get("foodExhaustionLevel") {
            vitals.exhaustion = *e;
        }
        if let Some(Tag::Int(l)) = root.get("XpLevel") {
            vitals.xp_level = *l;
        }
        if let Some(Tag::Float(p)) = root.get("XpP") {
            vitals.xp_progress = *p;
        }
        if let Some(Tag::Int(t)) = root.get("XpTotal") {
            vitals.xp_total = *t;
        }
        vitals.effects = crate::effects::Effects::from_nbt(root.get("active_effects"));
        if let Some(Tag::Float(a)) = root.get("AbsorptionAmount") {
            vitals.absorption = *a;
        }
        vitals.dead = vitals.health <= 0.0;
        let extra = Extra {
            respawn: match root.get("respawn") {
                Some(Tag::Compound(c)) => Respawn::from_nbt(c),
                _ => None,
            },
            tags: match root.get("Tags") {
                Some(Tag::List(l)) => l
                    .iter()
                    .filter_map(|t| match t {
                        Tag::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            },
        };
        let dimension = match root.get("Dimension") {
            Some(Tag::String(d)) => d.clone(),
            _ => "minecraft:overworld".to_owned(),
        };
        Ok(Some(Saved {
            dimension,
            pose,
            game_mode,
            flying,
            hands,
            vitals,
            extra,
        }))
    }

    fn entry(
        names: &dyn Fn(i32) -> Option<String>,
        hands: &Hands,
        slot: usize,
    ) -> Option<Compound> {
        let stack = hands.slots[slot].as_ref()?;
        let mut entry = Compound::new();
        entry.insert("count", Tag::Int(stack.count));
        entry.insert("id", Tag::String(names(stack.item)?));
        if let Some(components) =
            crate::items::join_damage(stack.damage, hands.components.get(&slot))
        {
            entry.insert("components", components);
        }
        Some(entry)
    }

    /// Puts a player's state into their NBT, as vanilla saves it;
    /// `names` gives item ids.
    pub fn fill(root: &mut Compound, saved: &Saved, names: &dyn Fn(i32) -> Option<String>) {
        let p = &saved.pose;
        root.insert(
            "Pos",
            Tag::List(vec![Tag::Double(p.x), Tag::Double(p.y), Tag::Double(p.z)]),
        );
        root.insert(
            "Rotation",
            Tag::List(vec![Tag::Float(p.yaw), Tag::Float(p.pitch)]),
        );
        root.insert("OnGround", Tag::Byte(i8::from(p.on_ground)));
        root.insert("Dimension", Tag::String(saved.dimension.clone()));
        root.insert("playerGameType", Tag::Int(saved.game_mode));
        // What the game mode allows, and whether the player was flying.
        let mut abilities = match root.get("abilities") {
            Some(Tag::Compound(a)) => a.clone(),
            _ => {
                let mut a = Compound::new();
                a.insert("flySpeed", Tag::Float(0.05));
                a.insert("walkSpeed", Tag::Float(0.1));
                a
            }
        };
        let (creative, spectator) = (saved.game_mode == 1, saved.game_mode == 3);
        abilities.insert("flying", Tag::Byte(i8::from(saved.flying)));
        abilities.insert("instabuild", Tag::Byte(i8::from(creative)));
        abilities.insert("invulnerable", Tag::Byte(i8::from(creative || spectator)));
        abilities.insert(
            "mayBuild",
            Tag::Byte(i8::from(saved.game_mode != 2 && !spectator)),
        );
        abilities.insert("mayfly", Tag::Byte(i8::from(creative || spectator)));
        root.insert("abilities", Tag::Compound(abilities));
        root.insert("Health", Tag::Float(saved.vitals.health));
        root.insert("foodLevel", Tag::Int(saved.vitals.food));
        root.insert("foodSaturationLevel", Tag::Float(saved.vitals.saturation));
        root.insert("foodExhaustionLevel", Tag::Float(saved.vitals.exhaustion));
        root.insert("XpLevel", Tag::Int(saved.vitals.xp_level));
        root.insert("XpP", Tag::Float(saved.vitals.xp_progress));
        root.insert("XpTotal", Tag::Int(saved.vitals.xp_total));
        root.insert("AbsorptionAmount", Tag::Float(saved.vitals.absorption));
        match &saved.extra.respawn {
            Some(r) => {
                root.insert("respawn", Tag::Compound(r.to_nbt(true)));
            }
            None => {
                root.remove("respawn");
            }
        }
        if saved.extra.tags.is_empty() {
            root.remove("Tags");
        } else {
            root.insert(
                "Tags",
                Tag::List(
                    saved
                        .extra
                        .tags
                        .iter()
                        .map(|t| Tag::String(t.clone()))
                        .collect(),
                ),
            );
        }
        if saved.vitals.effects.is_empty() {
            root.remove("active_effects");
        } else {
            root.insert("active_effects", saved.vitals.effects.to_nbt());
        }
        root.insert("SelectedItemSlot", Tag::Int(saved.hands.selected as i32));
        let mut inventory = Vec::new();
        for index in 0..36 {
            let slot = container_slot(index).expect("0..36");
            if let Some(mut entry) = Self::entry(names, &saved.hands, slot) {
                entry.insert("Slot", Tag::Byte(index as i8));
                inventory.push(Tag::Compound(entry));
            }
        }
        root.insert("Inventory", Tag::List(inventory));
        let mut equipment = Compound::new();
        for (key, slot) in EQUIPMENT {
            if let Some(entry) = Self::entry(names, &saved.hands, slot) {
                equipment.insert(key, Tag::Compound(entry));
            }
        }
        root.insert("equipment", Tag::Compound(equipment));
    }

    /// Writes the player's state over their stored file (or a new one).
    pub fn save(&self, uuid: u128, saved: &Saved, data_version: i32) -> Result<()> {
        let mut root = self.read(uuid)?.unwrap_or_else(|| {
            let mut root = Compound::new();
            root.insert("DataVersion", Tag::Int(data_version));
            root.insert("Dimension", Tag::String("minecraft:overworld".into()));
            root.insert("Health", Tag::Float(20.0));
            root.insert("foodLevel", Tag::Int(20));
            let mut uuid_ints = Vec::new();
            for shift in [96, 64, 32, 0] {
                uuid_ints.push((uuid >> shift) as u32 as i32);
            }
            root.insert("UUID", Tag::IntArray(uuid_ints));
            root
        });
        Self::fill(&mut root, saved, &|i| self.item_names.get(&i).cloned());
        std::fs::create_dir_all(&self.dir)?;
        let mut bytes = Vec::new();
        mc_nbt::write_named("", &Tag::Compound(root), &mut bytes)?;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&bytes)?;
        let path = self.path(uuid);
        if path.exists() {
            std::fs::copy(&path, path.with_extension("dat_old"))?;
        }
        let tmp = path.with_extension("dat_tmp");
        std::fs::write(&tmp, encoder.finish()?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}
