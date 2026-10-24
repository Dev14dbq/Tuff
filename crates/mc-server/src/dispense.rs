//! What dispensers do with items that act on entities or put entities into
//! the world (minecraft.wiki, "Dispenser"): projectiles fly out, boats,
//! minecarts, armor stands and mobs from spawn eggs appear in front,
//! shulker boxes are placed, armour and heads are put on whoever stands in
//! front, saddles and horse armour on animals, shears cut wool and leads,
//! and a few items change the block in front (powder snow, glowstone,
//! honeycomb, water bottles). The block-only ones (buckets, bone meal, TNT,
//! fire, glass bottles) are in `hoppers::behavior`.

use mc_entity::extra::{Craft, Decor, Firework, Pickup, Shot};
use mc_entity::mob::{Gear, slot};
use mc_entity::{Entity, Kind, Species};
use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::Dir;

use crate::context::ServerContext;
use crate::fx;
use crate::hits;
use crate::hoppers::{Behavior, done};
use crate::level::WorldLevel;
use crate::projectiles;
use crate::tick::lock;

/// Where a dispenser lets things out: 0.7 from its centre toward its face.
pub fn mouth(p: Position, facing: Dir) -> [f64; 3] {
    let (dx, dy, dz) = facing.offset();
    [
        f64::from(p.x) + 0.5 + 0.7 * f64::from(dx),
        f64::from(p.y) + 0.5 + 0.7 * f64::from(dy),
        f64::from(p.z) + 0.5 + 0.7 * f64::from(dz),
    ]
}

fn step(facing: Dir) -> [f64; 3] {
    let (dx, dy, dz) = facing.offset();
    [f64::from(dx), f64::from(dy), f64::from(dz)]
}

/// The level event a dispenser plays for an item it used: a launch for
/// projectiles, the firework and blaze sounds, nothing for spawn eggs, a
/// click for the rest.
pub fn sound_of(name: &str) -> Option<i32> {
    let n = name.trim_start_matches("minecraft:");
    Some(match n {
        "arrow" | "spectral_arrow" | "tipped_arrow" | "snowball" | "egg" | "splash_potion"
        | "lingering_potion" => 1002,
        "firework_rocket" => 1004,
        "fire_charge" => 1018,
        _ if n.ends_with("_spawn_egg") => return None,
        _ => 1000,
    })
}

/// The stack's components as saved, for the readers that take them so.
fn comps(stack: &ItemStack) -> Option<Tag> {
    crate::held::comps_of(stack)
}

/// The behaviour for `name`, or `None` to fall back on the others.
pub fn behavior(
    level: &mut WorldLevel,
    name: &str,
    stack: &ItemStack,
    p: Position,
    facing: Dir,
) -> Option<Behavior> {
    let ctx = level.context();
    let (dx, dy, dz) = facing.offset();
    let front = Position::new(p.x + dx, p.y + dy, p.z + dz);
    let n = name.trim_start_matches("minecraft:");
    // Projectiles: up a little from straight out, spread by 6 (potions go
    // farther and straighter).
    let shot = match n {
        "arrow" | "spectral_arrow" | "tipped_arrow" => {
            let mut a = mc_entity::extra::Arrow::new(stack.item);
            a.spectral = n == "spectral_arrow";
            if n == "tipped_arrow" {
                a.effects = projectiles::potions::contents(comps(stack).as_ref());
            }
            a.pickup = Pickup::Yes;
            Some((Shot::Arrow(a), 1.1, 6.0))
        }
        "snowball" => Some((Shot::Snowball, 1.1, 6.0)),
        "egg" => Some((Shot::Egg, 1.1, 6.0)),
        "splash_potion" | "lingering_potion" => {
            let effects = projectiles::potions::contents(comps(stack).as_ref());
            Some((
                Shot::Potion {
                    item: stack.item,
                    lingering: n == "lingering_potion",
                    color: projectiles::potions::color(&effects),
                    effects,
                },
                1.1 * 1.25,
                6.0 * 0.5,
            ))
        }
        _ => None,
    };
    if let Some((shot, speed, spread)) = shot {
        let d = step(facing);
        projectiles::shoot(
            ctx,
            shot,
            None,
            mouth(p, facing),
            [d[0], d[1] + 0.1, d[2]],
            speed,
            spread,
        );
        return Some(done(None));
    }
    match n {
        "fire_charge" => {
            // A blaze's fireball, straight out with a little spread.
            let d = step(facing);
            let dir = d.map(|c| c + hits::gauss() * 0.05);
            projectiles::shoot(
                ctx,
                Shot::Fireball { small: true },
                None,
                mouth(p, facing),
                dir,
                1.0,
                0.0,
            );
            Some(done(None))
        }
        "firework_rocket" => {
            let (flight, stars) = projectiles::firework::read(stack);
            let life = 10 * (flight + 1) + (hits::rnd() * 5.0) as i32 + (hits::rnd() * 6.0) as i32;
            let shot = Shot::Firework(Firework {
                life,
                boosts: None,
                stars: vec![Vec::new(); stars],
                shot_at_angle: true,
                stack: Some(stack.with_count(1)),
            });
            let d = step(facing);
            let at = [
                f64::from(p.x) + 0.5 + d[0] * 0.5,
                f64::from(p.y) + 0.5 + d[1] * 0.5,
                f64::from(p.z) + 0.5 + d[2] * 0.5,
            ];
            projectiles::shoot(ctx, shot, None, at, d, 0.5, 1.0);
            Some(done(None))
        }
        _ if n.ends_with("_boat") || n.ends_with("_raft") => Some(boat(ctx, name, front, facing)),
        _ if n.ends_with("minecart") => Some(minecart(ctx, name, front, facing)),
        _ if n.ends_with("_spawn_egg") => {
            let species = crate::mobs::eggs::species_of(n)?;
            let mut e = crate::mobs::new_mob(
                ctx,
                species,
                [
                    f64::from(front.x) + 0.5,
                    f64::from(front.y),
                    f64::from(front.z) + 0.5,
                ],
            );
            if let Kind::Mob(m) = &mut e.kind {
                m.yaw = fx::hung_yaw(facing as u8);
                m.head_yaw = m.yaw;
            }
            level.spawn(e);
            Some(done(None))
        }
        "armor_stand" => {
            let e = crate::decor::entity(
                ctx,
                [
                    f64::from(front.x) + 0.5,
                    f64::from(front.y),
                    f64::from(front.z) + 0.5,
                ],
                mc_entity::extra::Hung {
                    decor: Decor::Stand(Default::default()),
                    facing: 0,
                    yaw: fx::hung_yaw(facing as u8),
                    anchor: [front.x, front.y, front.z],
                },
            );
            fx::sound(
                ctx,
                "entity.armor_stand.place",
                fx::category::BLOCKS,
                e.pos,
                1.0,
                1.0,
            );
            crate::decor::spawn_all(ctx, vec![e]);
            Some(done(None))
        }
        _ if n.ends_with("shulker_box") => Some(shulker_box(level, name, stack, front, facing)),
        "saddle" => Some(on_mobs(ctx, front, |ctx, e| {
            crate::mobs::ride::saddle(ctx, e, stack.item)
        })),
        _ if n.ends_with("_horse_armor") => Some(on_mobs(ctx, front, |ctx, e| {
            horse_armor(ctx, e, stack.item)
        })),
        "shears" => shears(ctx, front),
        "glowstone" => anchor(level, front),
        "honeycomb" => wax(level, front),
        "potion" if is_water_bottle(stack) => mud(level, front),
        "powder_snow_bucket" => {
            let state = ctx.world.block(front, &ctx.data.blocks).unwrap_or(0);
            let snow = ctx.shapes.default_state("minecraft:powder_snow")?;
            if (ctx.shapes.is_air(state) || ctx.rules.is_replaceable(state))
                && level.set_block(front, snow)
            {
                fx::sound(
                    ctx,
                    "item.bucket.empty_powder_snow",
                    fx::category::BLOCKS,
                    centre(front),
                    1.0,
                    1.0,
                );
                Some(done(ctx.items.id("minecraft:bucket")))
            } else {
                Some(Behavior::Default)
            }
        }
        _ => equip(ctx, name, stack, front),
    }
}

fn centre(p: Position) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ]
}

fn is_water_bottle(stack: &ItemStack) -> bool {
    match stack.components.get("minecraft:potion_contents") {
        Some(Tag::Compound(c)) => {
            matches!(c.get("potion"), Some(Tag::String(p)) if p == "minecraft:water")
        }
        Some(Tag::String(p)) => p == "minecraft:water",
        _ => false,
    }
}

/// A boat is put on the water in front, or in the air over water there;
/// else it drops.
fn boat(ctx: &ServerContext, name: &str, front: Position, facing: Dir) -> Behavior {
    let Some(craft) = crate::vehicles::craft_of(name) else {
        return Behavior::Default;
    };
    let water = |p: Position| {
        hits::in_water(
            ctx,
            [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ],
        )
    };
    let below = Position::new(front.x, front.y - 1, front.z);
    let y = if water(front) {
        f64::from(front.y) + 1.0
    } else if !hits::stops(ctx, [front.x, front.y, front.z]) && water(below) {
        f64::from(front.y)
    } else {
        return Behavior::Default;
    };
    let (dx, _, dz) = facing.offset();
    let pos = [
        f64::from(front.x) + 0.5 - f64::from(dx) * 0.375,
        y,
        f64::from(front.z) + 0.5 - f64::from(dz) * 0.375,
    ];
    let e = crate::vehicles::new(ctx, craft, pos, fx::hung_yaw(facing as u8));
    crate::decor::spawn_all(ctx, vec![e]);
    done(None)
}

/// A minecart is put on the rail in front, or on one just below it; else
/// it drops.
fn minecart(ctx: &ServerContext, name: &str, front: Position, facing: Dir) -> Behavior {
    let Some(craft @ Craft::Minecart(_)) = crate::vehicles::craft_of(name) else {
        return Behavior::Default;
    };
    let rail_at = |p: Position| {
        ctx.world
            .block(p, &ctx.data.blocks)
            .and_then(|s| Some((ctx.data.blocks.property(s, "shape")?.to_owned(), s)))
            .filter(|(_, s)| {
                ctx.data
                    .blocks
                    .block_name(*s)
                    .is_some_and(|n| n.ends_with("rail"))
            })
    };
    let below = Position::new(front.x, front.y - 1, front.z);
    let (cell, shape) = match rail_at(front) {
        Some((shape, _)) => (front, shape),
        None if !hits::stops(ctx, [front.x, front.y, front.z]) => match rail_at(below) {
            Some((shape, _)) => (below, shape),
            None => return Behavior::Default,
        },
        None => return Behavior::Default,
    };
    let rise = if shape.starts_with("ascending") {
        0.5
    } else {
        0.0
    };
    let pos = [
        f64::from(cell.x) + 0.5,
        f64::from(cell.y) + 0.0625 + rise,
        f64::from(cell.z) + 0.5,
    ];
    let e = crate::vehicles::new(ctx, craft, pos, fx::hung_yaw(facing as u8));
    crate::decor::spawn_all(ctx, vec![e]);
    done(None)
}

/// A shulker box is placed in front, facing out (up when there is
/// something under that space), with what it holds.
fn shulker_box(
    level: &mut WorldLevel,
    name: &str,
    stack: &ItemStack,
    front: Position,
    facing: Dir,
) -> Behavior {
    let ctx = level.context();
    let blocks = &ctx.data.blocks;
    let here = ctx.world.block(front, blocks).unwrap_or(0);
    if !(ctx.shapes.is_air(here) || ctx.rules.is_replaceable(here)) {
        return Behavior::Failed;
    }
    let below = ctx
        .world
        .block(Position::new(front.x, front.y - 1, front.z), blocks)
        .unwrap_or(0);
    let face = if ctx.shapes.is_air(below) {
        facing.name()
    } else {
        "up"
    };
    let Some(state) = blocks
        .default_state(name)
        .map(|s| ctx.shapes.with(s, "facing", face))
    else {
        return Behavior::Failed;
    };
    if !level.set_block(front, state) {
        return Behavior::Failed;
    }
    let mut be = ctx
        .world
        .block_entity(front, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: front.x,
            y: front.y,
            z: front.z,
            id: "minecraft:shulker_box".into(),
            data: mc_nbt::Compound::new(),
        });
    // The item's `container` list becomes the block's `Items`.
    if let Some(Tag::List(list)) = stack.components.get("minecraft:container") {
        let items = list
            .iter()
            .filter_map(|t| {
                let Tag::Compound(c) = t else { return None };
                let Some(Tag::Int(slot)) = c.get("slot") else {
                    return None;
                };
                let Some(Tag::Compound(item)) = c.get("item") else {
                    return None;
                };
                let mut out = item.clone();
                out.insert("Slot", Tag::Byte(*slot as i8));
                Some(Tag::Compound(out))
            })
            .collect();
        be.data.insert("Items", Tag::List(items));
    }
    if let Some(name) = stack.components.get("minecraft:custom_name") {
        be.data.insert("CustomName", name.clone());
    }
    ctx.world.set_block_entity(be, blocks);
    done(None)
}

/// Mobs whose boxes touch the block `front`, by entity id.
fn mobs_in(ctx: &ServerContext, front: Position) -> Vec<i32> {
    let (lo, hi) = (
        [f64::from(front.x), f64::from(front.y), f64::from(front.z)],
        [
            f64::from(front.x) + 1.0,
            f64::from(front.y) + 1.0,
            f64::from(front.z) + 1.0,
        ],
    );
    lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.dying.is_none()))
        .filter(|e| {
            let b = e.aabb();
            (0..3).all(|i| b.min[i] < hi[i] && b.max[i] > lo[i])
        })
        .map(|e| e.id)
        .collect()
}

/// Tries `act` on the mobs in front, one by one, until it works.
fn on_mobs(
    ctx: &ServerContext,
    front: Position,
    mut act: impl FnMut(&ServerContext, &mut Entity) -> bool,
) -> Behavior {
    for id in mobs_in(ctx, front) {
        let Some(mut e) = lock(&ctx.entities).iter().find(|e| e.id == id).cloned() else {
            continue;
        };
        let worked = act(ctx, &mut e);
        if worked {
            if let Some(slot) = lock(&ctx.entities).iter_mut().find(|x| x.id == id) {
                *slot = e;
            }
            return done(None);
        }
    }
    Behavior::Failed
}

fn horse_armor(ctx: &ServerContext, e: &mut Entity, item: i32) -> bool {
    let id = e.id;
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if m.species != Species::Horse || !m.is_tame() || m.gear[slot::BODY].is_some() {
        return false;
    }
    m.gear[slot::BODY] = Some(Gear {
        item,
        count: 1,
        damage: 0,
    });
    m.persistent = true;
    fx::sound(
        ctx,
        "entity.horse.armor",
        fx::category::NEUTRAL,
        pos,
        0.5,
        1.0,
    );
    send_gear(ctx, id, m);
    true
}

fn send_gear(ctx: &ServerContext, id: i32, m: &mc_entity::mob::Mob) {
    fx::emit(
        ctx,
        fx::Fx::Equip(mc_protocol::packets::play::SetEquipment {
            entity_id: id,
            slots: crate::mobs::meta::worn(m),
        }),
    );
}

/// Shears: a full beehive is handled before; here a sheep or mooshroom in
/// front is shorn, else a leashed mob is let go (the lead drops).
fn shears(ctx: &ServerContext, front: Position) -> Option<Behavior> {
    if let Behavior::Done { .. } = on_mobs(ctx, front, crate::mobs::interact::shear) {
        return Some(Behavior::Done {
            leftover: None,
            wear: 1,
        });
    }
    for id in mobs_in(ctx, front) {
        if crate::decor::leash::detach(ctx, id) {
            let pos = lock(&ctx.entities)
                .iter()
                .find(|e| e.id == id)
                .map(|e| [e.pos[0], e.pos[1] + 0.5, e.pos[2]]);
            if let (Some(pos), Some(lead)) = (pos, ctx.items.id("minecraft:lead")) {
                crate::tick::spawn_item_at(ctx, pos, lead, 1);
            }
            fx::sound(
                ctx,
                "item.shears.snip",
                fx::category::BLOCKS,
                centre(front),
                1.0,
                1.0,
            );
            return Some(Behavior::Done {
                leftover: None,
                wear: 1,
            });
        }
    }
    Some(Behavior::Failed)
}

/// Glowstone charges a respawn anchor that is not full.
fn anchor(level: &mut WorldLevel, front: Position) -> Option<Behavior> {
    let ctx = level.context();
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(front, blocks)?;
    if blocks.block_name(state) != Some("minecraft:respawn_anchor") {
        return Some(Behavior::Default);
    }
    let charges: i32 = blocks.property(state, "charges")?.parse().ok()?;
    if charges >= 4 {
        return Some(Behavior::Failed);
    }
    let new = ctx
        .shapes
        .with(state, "charges", &(charges + 1).to_string());
    level.set_block(front, new);
    fx::sound(
        ctx,
        "block.respawn_anchor.charge",
        fx::category::BLOCKS,
        centre(front),
        1.0,
        1.0,
    );
    Some(done(None))
}

/// Honeycomb waxes the copper block in front.
fn wax(level: &mut WorldLevel, front: Position) -> Option<Behavior> {
    let ctx = level.context();
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(front, blocks)?;
    let n = blocks.block_name(state)?.trim_start_matches("minecraft:");
    let copper = n.contains("copper") || n.ends_with("lightning_rod");
    if !copper || n.starts_with("waxed_") {
        return Some(Behavior::Default);
    }
    let waxed = if n == "copper_block" {
        "minecraft:waxed_copper_block".to_owned()
    } else {
        format!("minecraft:waxed_{n}")
    };
    let Some(new) = same_properties(ctx, state, &waxed) else {
        return Some(Behavior::Default);
    };
    level.set_block(front, new);
    fx::level_event(ctx, 3003, centre(front), 0);
    Some(done(None))
}

/// The state of block `name` with the properties `state` has that it has.
fn same_properties(ctx: &ServerContext, state: u32, name: &str) -> Option<u32> {
    let blocks = &ctx.data.blocks;
    let info = blocks.get(name)?;
    let mut new = blocks.default_state(name)?;
    for (key, _) in &info.default_properties {
        if let Some(v) = blocks.property(state, key) {
            new = ctx.shapes.with(new, key, v);
        }
    }
    Some(new)
}

/// A water bottle turns dirt, coarse dirt and rooted dirt into mud.
fn mud(level: &mut WorldLevel, front: Position) -> Option<Behavior> {
    let ctx = level.context();
    let state = ctx.world.block(front, &ctx.data.blocks)?;
    let dirt = matches!(
        ctx.data.blocks.block_name(state),
        Some("minecraft:dirt" | "minecraft:coarse_dirt" | "minecraft:rooted_dirt")
    );
    if !dirt {
        return Some(Behavior::Default);
    }
    level.set_block(front, ctx.shapes.default_state("minecraft:mud")?);
    fx::sound(
        ctx,
        "entity.generic.splash",
        fx::category::BLOCKS,
        centre(front),
        1.0,
        1.0,
    );
    Some(done(ctx.items.id("minecraft:glass_bottle")))
}

/// The equipment slot armour, elytra, heads and carved pumpkins go in.
fn armor_slot(name: &str) -> Option<usize> {
    let n = name.trim_start_matches("minecraft:");
    Some(
        if n.ends_with("_helmet")
            || n.ends_with("_head")
            || n.ends_with("_skull")
            || n == "carved_pumpkin"
        {
            slot::HEAD
        } else if n.ends_with("_chestplate") || n == "elytra" {
            slot::CHEST
        } else if n.ends_with("_leggings") {
            slot::LEGS
        } else if n.ends_with("_boots") {
            slot::FEET
        } else {
            return None;
        },
    )
}

/// Armour (and heads, carved pumpkins, elytra) put on a mob that can pick
/// items up, or an armor stand, in front, in an empty slot. (Players in
/// front are dressed by `hoppers::dress_player` first.)
fn equip(ctx: &ServerContext, name: &str, stack: &ItemStack, front: Position) -> Option<Behavior> {
    let gear_slot = armor_slot(name)?;
    let worked = on_mobs(ctx, front, |ctx, e| {
        let id = e.id;
        let Kind::Mob(m) = &mut e.kind else {
            return false;
        };
        if !m.can_pick_up || m.gear[gear_slot].is_some() {
            return false;
        }
        m.gear[gear_slot] = Some(Gear {
            item: stack.item,
            count: 1,
            damage: stack.damage,
        });
        // Put on by a dispenser: it always drops, and the mob stays.
        m.drop_chance[gear_slot] = 2.0;
        m.persistent = true;
        send_gear(ctx, id, m);
        true
    });
    if matches!(worked, Behavior::Done { .. }) {
        return Some(worked);
    }
    // Armor stands: the slot numbers are the same.
    let (lo, hi) = (
        [f64::from(front.x), f64::from(front.y), f64::from(front.z)],
        [
            f64::from(front.x) + 1.0,
            f64::from(front.y) + 1.0,
            f64::from(front.z) + 1.0,
        ],
    );
    let mut entities = lock(&ctx.entities);
    for e in entities.iter_mut() {
        let b = e.aabb();
        if e.removed || !(0..3).all(|i| b.min[i] < hi[i] && b.max[i] > lo[i]) {
            continue;
        }
        let id = e.id;
        let Kind::Hung(h) = &mut e.kind else { continue };
        let Decor::Stand(s) = &mut h.decor else {
            continue;
        };
        if s.marker || s.gear[gear_slot].is_some() {
            continue;
        }
        s.gear[gear_slot] = Some(stack.with_count(1));
        fx::emit(
            ctx,
            fx::Fx::Equip(mc_protocol::packets::play::SetEquipment {
                entity_id: id,
                slots: vec![(gear_slot as u8, Some(stack.with_count(1)))],
            }),
        );
        return Some(done(None));
    }
    // Heads and pumpkins with nobody to wear them do nothing.
    let n = name.trim_start_matches("minecraft:");
    if gear_slot == slot::HEAD && !n.ends_with("_helmet") {
        return Some(Behavior::Failed);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers;
    use crate::projectiles::tests::flat;

    /// A dispenser at `(x, -60, 0)` facing east holding `item`.
    fn loaded(ctx: &ServerContext, x: i32, item: &str, count: i32) -> Position {
        let p = Position::new(x, -60, 0);
        let state = ctx
            .data
            .blocks
            .default_state("minecraft:dispenser")
            .unwrap();
        let state = ctx.shapes.with(state, "facing", "east");
        WorldLevel::new(ctx).set_block(p, state);
        let item = ctx.items.id(item).unwrap();
        containers::put(
            ctx,
            p,
            containers::kind_of("minecraft:dispenser").unwrap(),
            &[(0, Some(ItemStack::new(item, count)))],
        );
        p
    }

    /// Fires it once; the entities the block logic made.
    fn fire(ctx: &ServerContext, p: Position) -> Vec<Entity> {
        let mut level = WorldLevel::new(ctx);
        crate::hoppers::dispense(&mut level, p, Dir::East, false);
        std::mem::take(&mut level.spawned)
    }

    fn left(ctx: &ServerContext, p: Position) -> i32 {
        let kind = containers::kind_of("minecraft:dispenser").unwrap();
        containers::slots(ctx, p, kind)[0]
            .as_ref()
            .map_or(0, |s| s.count)
    }

    fn set(ctx: &ServerContext, p: Position, name: &str) {
        let state = ctx.data.blocks.default_state(name).unwrap();
        WorldLevel::new(ctx).set_block(p, state);
    }

    fn block(ctx: &ServerContext, p: Position) -> String {
        ctx.world
            .block(p, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
            .unwrap_or("")
            .to_owned()
    }

    fn entities(ctx: &ServerContext) -> Vec<Entity> {
        lock(&ctx.entities).clone()
    }

    #[test]
    fn projectiles_fly_out_of_its_face() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        for (i, item) in [
            "minecraft:arrow",
            "minecraft:snowball",
            "minecraft:fire_charge",
        ]
        .into_iter()
        .enumerate()
        {
            lock(&ctx.entities).clear();
            let p = loaded(&ctx, 10 * i as i32, item, 2);
            fire(&ctx, p);
            assert_eq!(left(&ctx, p), 1, "{item}");
            let all = entities(&ctx);
            let [e] = &all[..] else {
                panic!("{item}: {all:?}")
            };
            let Kind::Projectile(pr) = &e.kind else {
                panic!("{item}: {e:?}")
            };
            assert!(
                e.vel[0] > 0.5 && e.vel[0].abs() > e.vel[2].abs() * 3.0,
                "{item}"
            );
            assert!((e.pos[0] - f64::from(p.x) - 1.2).abs() < 1e-9);
            if item == "minecraft:fire_charge" {
                assert_eq!(pr.shot, Shot::Fireball { small: true });
            }
        }
    }

    #[test]
    fn a_firework_flies_straight_out() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, 0, "minecraft:firework_rocket", 1);
        fire(&ctx, p);
        let all = entities(&ctx);
        let [e] = &all[..] else { panic!("{all:?}") };
        assert!(
            matches!(&e.kind, Kind::Projectile(pr) if matches!(&pr.shot, Shot::Firework(f) if f.shot_at_angle))
        );
        assert!(e.vel[0] > 0.4 && e.vel[1].abs() < 0.1);
    }

    #[test]
    fn minecarts_go_on_rails_and_boats_on_water() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // No rail: it drops as an item.
        let p = loaded(&ctx, 0, "minecraft:minecart", 2);
        fire(&ctx, p);
        assert!(matches!(
            entities(&ctx)[..],
            [Entity {
                kind: Kind::Item { .. },
                ..
            }]
        ));
        lock(&ctx.entities).clear();
        set(&ctx, Position::new(1, -60, 0), "minecraft:rail");
        fire(&ctx, p);
        let all = entities(&ctx);
        let [e] = &all[..] else { panic!("{all:?}") };
        assert!(
            matches!(&e.kind, Kind::Vehicle(v) if v.craft == Craft::Minecart(mc_entity::extra::Cart::Rideable))
        );
        assert!((e.pos[1] + 59.9375).abs() < 1e-9 && (e.pos[0] - 1.5).abs() < 1e-9);
        // A boat needs water.
        lock(&ctx.entities).clear();
        let p = loaded(&ctx, 20, "minecraft:oak_boat", 2);
        fire(&ctx, p);
        assert!(matches!(
            entities(&ctx)[..],
            [Entity {
                kind: Kind::Item { .. },
                ..
            }]
        ));
        lock(&ctx.entities).clear();
        set(&ctx, Position::new(21, -60, 0), "minecraft:water");
        fire(&ctx, p);
        assert!(matches!(
            entities(&ctx)[..],
            [Entity {
                kind: Kind::Vehicle(_),
                ..
            }]
        ));
    }

    #[test]
    fn spawn_eggs_armor_stands_and_shulker_boxes_are_put_in_front() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, 0, "minecraft:pig_spawn_egg", 1);
        let made = fire(&ctx, p);
        assert!(
            matches!(&made[..], [Entity { kind: Kind::Mob(m), pos, .. }] if m.species == Species::Pig && (pos[0] - 1.5).abs() < 1e-9 && (pos[1] + 60.0).abs() < 1e-9)
        );
        assert_eq!(left(&ctx, p), 0);
        let p = loaded(&ctx, 10, "minecraft:armor_stand", 1);
        fire(&ctx, p);
        assert!(matches!(
            entities(&ctx)[..],
            [Entity {
                kind: Kind::Hung(_),
                ..
            }]
        ));
        // A shulker box keeps what it holds.
        let p = loaded(&ctx, 20, "minecraft:red_shulker_box", 1);
        {
            let kind = containers::kind_of("minecraft:dispenser").unwrap();
            let mut slots = containers::slots(&ctx, p, kind);
            let stack = slots[0].as_mut().unwrap();
            let mut item = mc_nbt::Compound::new();
            item.insert("id", Tag::String("minecraft:stick".into()));
            item.insert("count", Tag::Int(5));
            let mut entry = mc_nbt::Compound::new();
            entry.insert("slot", Tag::Int(3));
            entry.insert("item", Tag::Compound(item));
            stack
                .components
                .set("minecraft:container", Tag::List(vec![Tag::Compound(entry)]));
            containers::put(&ctx, p, kind, &[(0, slots[0].clone())]);
        }
        fire(&ctx, p);
        let front = Position::new(21, -60, 0);
        assert_eq!(block(&ctx, front), "minecraft:red_shulker_box");
        let kind = containers::kind_of("minecraft:red_shulker_box").unwrap();
        let held = containers::slots(&ctx, front, kind)[3].clone();
        assert_eq!(
            held.map(|s| (s.item, s.count)),
            Some((ctx.items.id("minecraft:stick").unwrap(), 5))
        );
        // Something in the way: it fails and keeps the box.
        let p = loaded(&ctx, 30, "minecraft:shulker_box", 1);
        set(&ctx, Position::new(31, -60, 0), "minecraft:stone");
        fire(&ctx, p);
        assert_eq!(left(&ctx, p), 1);
    }

    #[test]
    fn armour_saddles_and_shears_work_on_who_is_in_front() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mob = |species: Species, x: i32| {
            let e = crate::mobs::new_mob(&ctx, species, [f64::from(x) + 0.5, -60.0, 0.5]);
            let id = e.id;
            lock(&ctx.entities).push(e);
            id
        };
        let with = |id: i32, f: &dyn Fn(&mc_entity::mob::Mob) -> bool| {
            lock(&ctx.entities)
                .iter()
                .any(|e| e.id == id && matches!(&e.kind, Kind::Mob(m) if f(m)))
        };
        // A saddle on a pig.
        let pig = mob(Species::Pig, 1);
        let p = loaded(&ctx, 0, "minecraft:saddle", 1);
        fire(&ctx, p);
        assert!(with(pig, &|m| m.saddled()));
        assert_eq!(left(&ctx, p), 0);
        // Shears on a sheep.
        let sheep = mob(Species::Sheep, 11);
        let p = loaded(&ctx, 10, "minecraft:shears", 1);
        fire(&ctx, p);
        assert!(with(sheep, &|m| m.sheared));
        let kind = containers::kind_of("minecraft:dispenser").unwrap();
        assert_eq!(
            containers::slots(&ctx, p, kind)[0]
                .as_ref()
                .map(|s| s.damage),
            Some(1)
        );
        // A helmet on a zombie that picks things up, not on one that doesn't.
        let zombie = mob(Species::Zombie, 21);
        let p = loaded(&ctx, 20, "minecraft:iron_helmet", 1);
        let picky = |on: bool| {
            for e in lock(&ctx.entities).iter_mut() {
                if e.id == zombie
                    && let Kind::Mob(m) = &mut e.kind
                {
                    m.can_pick_up = on;
                    m.gear[slot::HEAD] = None;
                }
            }
        };
        picky(false);
        fire(&ctx, p);
        assert!(with(zombie, &|m| m.gear[slot::HEAD].is_none()));
        picky(true);
        loaded(&ctx, 20, "minecraft:iron_helmet", 1);
        fire(&ctx, p);
        assert!(with(zombie, &|m| m.gear[slot::HEAD].is_some()));
        // Boots on an armor stand.
        lock(&ctx.entities).clear();
        let p = loaded(&ctx, 30, "minecraft:armor_stand", 1);
        fire(&ctx, p);
        loaded(&ctx, 30, "minecraft:iron_boots", 1);
        fire(&ctx, p);
        assert!(lock(&ctx.entities).iter().any(|e| matches!(&e.kind, Kind::Hung(h) if matches!(&h.decor, Decor::Stand(s) if s.gear[slot::FEET].is_some()))));
        assert_eq!(left(&ctx, p), 0);
        // A carved pumpkin with no one to wear it stays.
        let p = loaded(&ctx, 40, "minecraft:carved_pumpkin", 1);
        fire(&ctx, p);
        assert_eq!(left(&ctx, p), 1);
    }

    #[test]
    fn some_items_change_the_block_in_front() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, 0, "minecraft:honeycomb", 1);
        set(&ctx, Position::new(1, -60, 0), "minecraft:copper_block");
        fire(&ctx, p);
        assert_eq!(
            block(&ctx, Position::new(1, -60, 0)),
            "minecraft:waxed_copper_block"
        );
        let p = loaded(&ctx, 10, "minecraft:glowstone", 1);
        set(&ctx, Position::new(11, -60, 0), "minecraft:respawn_anchor");
        fire(&ctx, p);
        let s = ctx
            .world
            .block(Position::new(11, -60, 0), &ctx.data.blocks)
            .unwrap();
        assert_eq!(ctx.data.blocks.property(s, "charges"), Some("1"));
        let p = loaded(&ctx, 20, "minecraft:powder_snow_bucket", 1);
        fire(&ctx, p);
        assert_eq!(
            block(&ctx, Position::new(21, -60, 0)),
            "minecraft:powder_snow"
        );
        let kind = containers::kind_of("minecraft:dispenser").unwrap();
        let first = |ctx: &ServerContext| {
            containers::slots(ctx, p, kind)[0]
                .as_ref()
                .and_then(|s| ctx.items.name(s.item).map(str::to_owned))
        };
        assert_eq!(first(&ctx).as_deref(), Some("minecraft:bucket"));
        // And the bucket scoops it back up.
        fire(&ctx, p);
        assert_eq!(first(&ctx).as_deref(), Some("minecraft:powder_snow_bucket"));
        assert_eq!(block(&ctx, Position::new(21, -60, 0)), "minecraft:air");
        // A water bottle makes mud of dirt.
        let p = loaded(&ctx, 30, "minecraft:potion", 1);
        {
            let mut slots = containers::slots(&ctx, p, kind);
            let mut c = mc_nbt::Compound::new();
            c.insert("potion", Tag::String("minecraft:water".into()));
            slots[0]
                .as_mut()
                .unwrap()
                .components
                .set("minecraft:potion_contents", Tag::Compound(c));
            containers::put(&ctx, p, kind, &[(0, slots[0].clone())]);
        }
        set(&ctx, Position::new(31, -60, 0), "minecraft:dirt");
        fire(&ctx, p);
        assert_eq!(block(&ctx, Position::new(31, -60, 0)), "minecraft:mud");
    }

    #[test]
    fn tnt_hops_out() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, 0, "minecraft:tnt", 1);
        fire(&ctx, p);
        let all = entities(&ctx);
        let [e] = &all[..] else { panic!("{all:?}") };
        assert!(e.vel[1] > 0.1, "{:?}", e.vel);
    }
}
