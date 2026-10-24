//! `/summon` for the entities of `mc_entity::extra`: their kinds from an
//! entity type name and the NBT given.

use mc_entity::extra::{
    Arrow, Cart, Craft, Decor, Firework, Frame, Hung, Pickup, Projectile, Shot, Stand, Vehicle,
};
use mc_entity::{Entity, Kind};
use mc_nbt::{Compound, Tag};

use crate::context::ServerContext;

fn int(nbt: &Compound, key: &str) -> Option<i32> {
    match nbt.get(key)? {
        Tag::Byte(v) => Some(i32::from(*v)),
        Tag::Short(v) => Some(i32::from(*v)),
        Tag::Int(v) => Some(*v),
        _ => None,
    }
}

fn flag(nbt: &Compound, key: &str) -> bool {
    int(nbt, key).is_some_and(|v| v != 0)
}

fn angles(nbt: Option<&Tag>) -> Option<[f32; 3]> {
    let Some(Tag::List(l)) = nbt else {
        return None;
    };
    let f = |i: usize| match l.get(i) {
        Some(Tag::Float(v)) => Some(*v),
        Some(Tag::Double(v)) => Some(*v as f32),
        _ => None,
    };
    Some([f(0)?, f(1)?, f(2)?])
}

/// The item of an `Item` compound.
fn stack(ctx: &ServerContext, tag: Option<&Tag>) -> Option<mc_protocol::packets::play::ItemStack> {
    let Some(Tag::Compound(c)) = tag else {
        return None;
    };
    let Some(Tag::String(id)) = c.get("id") else {
        return None;
    };
    let item = ctx.items.id(&mc_command::args::full_id(id))?;
    Some(mc_protocol::packets::play::ItemStack {
        count: 1,
        ..mc_protocol::packets::play::ItemStack::from_nbt_with(c, item)
    })
}

fn triple(nbt: &Compound, key: &str) -> Option<[f64; 3]> {
    let Some(Tag::List(l)) = nbt.get(key) else {
        return None;
    };
    let d = |i: usize| match l.get(i) {
        Some(Tag::Double(v)) => Some(*v),
        Some(Tag::Float(v)) => Some(f64::from(*v)),
        _ => None,
    };
    Some([d(0)?, d(1)?, d(2)?])
}

/// What an entity of type `name` holds (`Items`, `LootTable`, its seed) is
/// kept apart from the entity, by id, for containers that are entities
/// (chest minecarts and boats...).
pub fn stock(ctx: &ServerContext, id: i32, name: &str, nbt: &Compound) {
    if crate::containers::entity_kind(name).is_none() {
        return;
    }
    let mut data = Compound::new();
    for key in ["Items", "LootTable", "LootTableSeed"] {
        if let Some(tag) = nbt.get(key) {
            data.insert(key, tag.clone());
        }
    }
    if !data.is_empty() {
        ctx.entity_items
            .lock()
            .expect("entity items lock")
            .insert(id, data);
    }
}

/// An entity world generation made (`Pos`, `id`, its own fields), ready to
/// be added: end crystals, chest minecarts... `None` for what is not
/// supported.
pub fn from_saved(ctx: &ServerContext, nbt: &Compound) -> Option<Entity> {
    let Some(Tag::String(name)) = nbt.get("id") else {
        return None;
    };
    if name == "minecraft:end_crystal" {
        return crate::bosses::crystal::from_nbt(ctx, nbt);
    }
    let pos = triple(nbt, "Pos")?;
    let yaw = match nbt.get("Rotation") {
        Some(Tag::List(l)) => match l.first() {
            Some(Tag::Float(y)) => *y,
            _ => 0.0,
        },
        _ => 0.0,
    };
    let (kind, place) = build(ctx, name, nbt, pos, yaw)?;
    let entity = Entity {
        id: crate::bosses::entity_id(ctx, &kind),
        uuid: match nbt.get("UUID") {
            Some(Tag::IntArray(u)) if u.len() == 4 => u
                .iter()
                .fold(0u128, |a, &p| (a << 32) | u128::from(p as u32)),
            _ => crate::util::random_uuid(),
        },
        kind,
        pos: place.unwrap_or(pos),
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    stock(ctx, entity.id, name, nbt);
    Some(entity)
}

/// What to summon for `name` (`minecraft:arrow`): its kind, and the place
/// it must be at if it hangs on a wall. `None` for the other entities.
pub fn build(
    ctx: &ServerContext,
    name: &str,
    nbt: &Compound,
    at: [f64; 3],
    yaw: f32,
) -> Option<(Kind, Option<[f64; 3]>)> {
    let n = name.trim_start_matches("minecraft:");
    if let Some(species) = mc_entity::Species::from_id(name) {
        let mut kind = crate::mobs::new_mob(ctx, species, at).kind;
        if let Kind::Mob(m) = &mut kind {
            crate::mobs::nbt::apply(ctx, m, nbt);
        }
        return Some((kind, None));
    }
    let shot = |shot: Shot| {
        Some((
            Kind::Projectile(Projectile {
                shot,
                owner: None,
                mob: None,
                yaw: 0.0,
                pitch: 0.0,
            }),
            None,
        ))
    };
    let potion = |lingering: bool| {
        let item = ctx.items.id(if lingering {
            "minecraft:lingering_potion"
        } else {
            "minecraft:splash_potion"
        })?;
        Some(Shot::Potion {
            item,
            lingering,
            effects: Vec::new(),
            color: 0x385dc6,
        })
    };
    match n {
        "arrow" | "spectral_arrow" => {
            let mut a = Arrow::new(ctx.items.id("minecraft:arrow")?);
            a.spectral = n == "spectral_arrow";
            a.crit = flag(nbt, "crit");
            a.pickup = match int(nbt, "pickup") {
                Some(0) => Pickup::No,
                Some(2) => Pickup::Creative,
                _ => Pickup::Yes,
            };
            shot(Shot::Arrow(a))
        }
        "wither_skull" => shot(Shot::WitherSkull {
            dangerous: flag(nbt, "dangerous"),
        }),
        "dragon_fireball" => shot(Shot::DragonFireball),
        "end_crystal" => {
            let mut c = mc_entity::boss::Crystal {
                show_bottom: true,
                ..Default::default()
            };
            crate::bosses::crystal::apply_nbt(&mut c, nbt);
            Some((Kind::Crystal(c), None))
        }
        "snowball" => shot(Shot::Snowball),
        "egg" => shot(Shot::Egg),
        "ender_pearl" => shot(Shot::Pearl),
        "eye_of_ender" => shot(Shot::Eye {
            target: at,
            survive: true,
        }),
        "splash_potion" => shot(potion(false)?),
        "lingering_potion" => shot(potion(true)?),
        "firework_rocket" => shot(Shot::Firework(Firework {
            life: int(nbt, "LifeTime").unwrap_or(30),
            boosts: None,
            stars: Vec::new(),
            shot_at_angle: false,
            stack: None,
        })),
        "armor_stand" => {
            let mut s = Stand {
                small: flag(nbt, "Small"),
                arms: flag(nbt, "ShowArms"),
                no_base: flag(nbt, "NoBasePlate"),
                marker: flag(nbt, "Marker"),
                invisible: flag(nbt, "Invisible"),
                ..Stand::default()
            };
            if let Some(Tag::Compound(p)) = nbt.get("Pose") {
                for (i, key) in ["Head", "Body", "LeftArm", "RightArm", "LeftLeg", "RightLeg"]
                    .iter()
                    .enumerate()
                {
                    if let Some(a) = angles(p.get(key)) {
                        s.pose[i] = a;
                    }
                }
            }
            Some((
                Kind::Hung(Hung {
                    decor: Decor::Stand(Box::new(s)),
                    facing: 0,
                    yaw,
                    anchor: at.map(|v| v.floor() as i32),
                }),
                None,
            ))
        }
        "item_frame" | "glow_item_frame" | "painting" | "leash_knot" => {
            // Paintings keep their facing as a horizontal one (`facing`:
            // south, west, north, east).
            let flat = (n == "painting")
                .then(|| int(nbt, "facing"))
                .flatten()
                .map(|f| [3u8, 4, 2, 5][(f & 3) as usize]);
            let facing = flat
                .or_else(|| int(nbt, "Facing").map(|f| f.clamp(0, 5) as u8))
                .unwrap_or(3);
            let cell = at.map(|v| v.floor() as i32);
            let (dx, dy, dz) = crate::blocks::FACES[usize::from(facing)];
            let anchor = [cell[0] - dx, cell[1] - dy, cell[2] - dz];
            let decor = match n {
                "painting" => {
                    let name = match nbt.get("variant") {
                        Some(Tag::String(v)) => mc_command::args::full_id(v),
                        _ => "minecraft:kebab".into(),
                    };
                    let (hung, pos) = crate::decor::painting::hang(ctx, &name, cell, facing);
                    return Some((Kind::Hung(hung), Some(pos)));
                }
                "leash_knot" => Decor::LeashKnot,
                _ => Decor::Frame(Frame {
                    glow: n == "glow_item_frame",
                    item: stack(ctx, nbt.get("Item")),
                    rotation: int(nbt, "ItemRotation").map_or(0, |r| (r & 7) as u8),
                    invisible: flag(nbt, "Invisible"),
                    fixed: flag(nbt, "Fixed"),
                }),
            };
            let pos = if n == "leash_knot" {
                [
                    f64::from(cell[0]) + 0.5,
                    f64::from(cell[1]) + 0.5,
                    f64::from(cell[2]) + 0.5,
                ]
            } else {
                crate::decor::hung_pos(cell, facing)
            };
            Some((
                Kind::Hung(Hung {
                    decor,
                    facing,
                    yaw: 0.0,
                    anchor,
                }),
                Some(pos),
            ))
        }
        _ => {
            let craft = if let Some(w) = n.strip_suffix("_chest_boat") {
                Craft::Boat {
                    wood: w.into(),
                    chest: true,
                }
            } else if let Some(w) = n.strip_suffix("_boat") {
                Craft::Boat {
                    wood: w.into(),
                    chest: false,
                }
            } else if n == "bamboo_raft" {
                Craft::Boat {
                    wood: "bamboo".into(),
                    chest: false,
                }
            } else if n == "bamboo_chest_raft" {
                Craft::Boat {
                    wood: "bamboo".into(),
                    chest: true,
                }
            } else {
                Craft::Minecart(match n {
                    "minecart" => Cart::Rideable,
                    "chest_minecart" => Cart::Chest,
                    "furnace_minecart" => Cart::Furnace,
                    "hopper_minecart" => Cart::Hopper,
                    "tnt_minecart" => Cart::Tnt,
                    "command_block_minecart" => Cart::Command,
                    _ => return None,
                })
            };
            let mut v = Vehicle::new(craft, yaw);
            if let Some(Tag::String(c)) = nbt.get("Command") {
                v.command.command = c.clone();
            }
            if let Some(Tag::Byte(b)) = nbt.get("TrackOutput") {
                v.command.track_output = *b != 0;
            }
            Some((Kind::Vehicle(v), None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_protocol::Position;

    fn game_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3")
    }

    /// A cart as mineshaft generation writes it.
    fn generated_cart(at: [f64; 3]) -> Compound {
        let mut nbt = Compound::new();
        nbt.insert("id", Tag::String("minecraft:chest_minecart".into()));
        nbt.insert(
            "Pos",
            Tag::List(at.iter().map(|v| Tag::Double(*v)).collect()),
        );
        nbt.insert(
            "LootTable",
            Tag::String("minecraft:chests/abandoned_mineshaft".into()),
        );
        nbt.insert("LootTableSeed", Tag::Long(42));
        nbt
    }

    #[test]
    fn a_generated_chest_minecart_rolls_its_loot_when_opened_or_broken() {
        let Ok(data) = mc_data::McData::load(&game_dir()) else {
            eprintln!("skipped: no game data");
            return;
        };
        let ctx = ServerContext::new(crate::Config::new(game_dir()), data).unwrap();
        let kind = crate::containers::entity_kind("minecraft:chest_minecart").unwrap();

        let opened = from_saved(&ctx, &generated_cart([0.5, -59.5, 0.5])).unwrap();
        assert!(matches!(
            &opened.kind,
            Kind::Vehicle(v) if v.craft == Craft::Minecart(Cart::Chest)
        ));
        assert_eq!(opened.pos, [0.5, -59.5, 0.5]);
        let key = crate::containers::entity_key(opened.id);
        let first = crate::containers::open(&ctx, 1, key, kind);
        assert!(first.iter().any(Option::is_some), "the loot is rolled");
        crate::containers::close(&ctx, 1, key);
        assert_eq!(
            crate::containers::open(&ctx, 2, key, kind),
            first,
            "rolled once"
        );

        let broken = from_saved(&ctx, &generated_cart([4.5, -59.5, 0.5])).unwrap();
        let (drops, viewers) = crate::containers::entity_gone(&ctx, broken.id);
        assert!(!drops.is_empty(), "breaking it drops the loot");
        assert!(viewers.is_empty());
    }

    #[test]
    fn mineshafts_leave_chest_minecarts_on_rails_and_no_chests() {
        let Ok(data) = mc_data::McData::load(&game_dir()) else {
            eprintln!("skipped: no game data");
            return;
        };
        let folder = std::env::temp_dir().join(format!("mc-carts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let mut config = crate::Config::new(game_dir());
        config.world = Some(folder.clone());
        config.seed = Some(1);
        let ctx = ServerContext::new(config, data).unwrap();
        let blocks = &ctx.data.blocks;
        let pipeline = ctx.world.pipeline().expect("a generating world");
        pipeline.prepare();
        let chest = blocks.default_state("minecraft:chest").unwrap();
        let mut found = None;
        let mut starts = pipeline.starts_within(0, 0, 40, Some("minecraft:mineshaft"));
        starts.truncate(6);
        'search: for (_, _, centre) in starts {
            for cx in (centre.x >> 4) - 3..=(centre.x >> 4) + 3 {
                for cz in (centre.z >> 4) - 3..=(centre.z >> 4) + 3 {
                    let at = Position::new(cx * 16, 0, cz * 16);
                    let _ = ctx.world.block(at, blocks);
                    for nbt in ctx.world.take_generated_entities() {
                        if nbt.get("id") == Some(&Tag::String("minecraft:chest_minecart".into())) {
                            found = Some(nbt);
                            break 'search;
                        }
                    }
                }
            }
        }
        let cart = found.expect("a mineshaft near the origin has a chest minecart");
        assert_eq!(
            cart.get("LootTable"),
            Some(&Tag::String("minecraft:chests/abandoned_mineshaft".into()))
        );
        assert!(matches!(cart.get("LootTableSeed"), Some(Tag::Long(_))));
        let e = from_saved(&ctx, &cart).unwrap();
        let cell = e.pos.map(|v| v.floor() as i32);
        let rail = ctx
            .world
            .block(Position::new(cell[0], cell[1], cell[2]), blocks)
            .unwrap();
        assert_eq!(blocks.block_name(rail), Some("minecraft:rail"), "on a rail");
        let (cx, cz) = (cell[0] >> 4, cell[2] >> 4);
        for x in cx * 16..cx * 16 + 16 {
            for z in cz * 16..cz * 16 + 16 {
                for y in cell[1] - 3..=cell[1] + 3 {
                    assert_ne!(
                        ctx.world.block(Position::new(x, y, z), blocks),
                        Some(chest),
                        "no chest blocks in a mineshaft chunk"
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(folder);
    }
}
