//! `/summon` for the entities of `mc_entity::extra`: their kinds from an
//! entity type name and the NBT given.

use mc_entity::Kind;
use mc_entity::extra::{
    Arrow, Cart, Craft, Decor, Firework, Frame, Hung, Pickup, Projectile, Shot, Stand, Vehicle,
};
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

/// The item of an `Item` compound: id and damage.
fn stack(ctx: &ServerContext, tag: Option<&Tag>) -> Option<(i32, i32)> {
    let Some(Tag::Compound(c)) = tag else {
        return None;
    };
    let Some(Tag::String(id)) = c.get("id") else {
        return None;
    };
    let item = ctx.items.id(&mc_command::args::full_id(id))?;
    Some((
        item,
        mc_protocol::packets::play::ItemStack::from_nbt_with(c, item).damage,
    ))
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
        "splash_potion" => shot(potion(false)?),
        "lingering_potion" => shot(potion(true)?),
        "firework_rocket" => shot(Shot::Firework(Firework {
            life: int(nbt, "LifeTime").unwrap_or(30),
            boosts: None,
            stars: Vec::new(),
            shot_at_angle: false,
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
                    decor: Decor::Stand(s),
                    facing: 0,
                    yaw,
                    anchor: at.map(|v| v.floor() as i32),
                }),
                None,
            ))
        }
        "item_frame" | "glow_item_frame" | "painting" | "leash_knot" => {
            let facing = int(nbt, "Facing").map_or(3, |f| f.clamp(0, 5) as u8);
            let cell = at.map(|v| v.floor() as i32);
            let (dx, dy, dz) = crate::blocks::FACES[usize::from(facing)];
            let anchor = [cell[0] - dx, cell[1] - dy, cell[2] - dz];
            let decor = match n {
                "painting" => Decor::Painting {
                    variant: match nbt.get("variant") {
                        Some(Tag::String(v)) => mc_command::args::full_id(v),
                        _ => "minecraft:kebab".into(),
                    },
                    size: (1, 1),
                },
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
