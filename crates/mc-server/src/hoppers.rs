//! Hoppers, hopper minecarts, droppers and dispensers on the server: the
//! rules are in `mc_tick::hopper` and `mc_tick::block_entity::transfer`;
//! this keeps the list of hoppers that tick and gives the rules the world
//! ([`Host`]). The dispenser behaviours that change the block in front
//! (buckets, bone meal, TNT, fire, shears, glass bottles) are here too, the
//! rest in `dispense`.

use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::Dir;
use mc_tick::block_entity::transfer::{self, Dispensing};

use mc_entity::extra::{Cart, Craft};

use crate::block_entities::Host;
use crate::context::ServerContext;
use crate::level::{WorldLevel, to_pos};
use crate::tick::lock;

pub(crate) use mc_tick::block_entity::transfer::{Behavior, done};

/// The hoppers that tick, in the order they take their turns.
pub type Active = mc_tick::hopper::Hoppers;

/// Moves one item from `from` into `to`. See `Level::container_transfer`.
#[cfg(test)]
pub fn transfer(
    ctx: &ServerContext,
    from: Position,
    from_side: Dir,
    to: Position,
    to_side: Dir,
) -> bool {
    transfer::transfer(
        &mut Host::new(ctx),
        &ctx.data.blocks,
        to_pos(from),
        from_side,
        to_pos(to),
        to_side,
    )
}

/// A hopper's push or pull (`Level::container_transfer`): composters,
/// jukeboxes and containers.
pub fn container_transfer(
    level: &mut WorldLevel,
    from: Position,
    from_side: Dir,
    to: Position,
    to_side: Dir,
) -> bool {
    let ctx = level.context();
    let logic = level.logic();
    transfer::container_transfer(
        level,
        logic,
        &mut Host::new(ctx),
        &ctx.data.blocks,
        to_pos(from),
        from_side,
        to_pos(to),
        to_side,
    )
}

pub fn cooldown(ctx: &ServerContext, p: Position) -> i32 {
    transfer::cooldown(&Host::new(ctx), to_pos(p))
}

pub fn set_cooldown(ctx: &ServerContext, p: Position, ticks: i32) {
    transfer::set_cooldown(&mut Host::new(ctx), to_pos(p), ticks);
}

/// The hopper takes one item (a whole stack's worth, one at a time) from an
/// item entity in the space above it.
pub fn pickup(ctx: &ServerContext, p: Position) -> bool {
    transfer::pickup(&mut Host::new(ctx), to_pos(p))
}

/// A dropper or dispenser fires (`transfer::dispense`); armour goes on a
/// player in front first, then the behaviours here and in `dispense`.
pub fn dispense(level: &mut WorldLevel, p: Position, facing: Dir, dropper: bool) {
    let ctx = level.context();
    let mut behaviors = Behaviors { level };
    transfer::dispense(
        &mut Host::new(ctx),
        &mut behaviors,
        to_pos(p),
        facing,
        dropper,
    );
}

/// The dispenser behaviours of the server's item registry.
struct Behaviors<'l, 'a> {
    level: &'l mut WorldLevel<'a>,
}

impl Dispensing for Behaviors<'_, '_> {
    fn behavior(
        &mut self,
        name: &str,
        stack: &ItemStack,
        p: mc_tick::BlockPos,
        facing: Dir,
    ) -> Behavior {
        let ctx = self.level.context();
        let p = crate::level::to_position(p);
        let (dx, dy, dz) = facing.offset();
        let front = Position::new(p.x + dx, p.y + dy, p.z + dz);
        if dress_player(ctx, front, stack) {
            // Armour goes on a player in front first.
            done(None)
        } else {
            behavior(ctx, self.level, name, stack, p, facing)
        }
    }

    fn sound(&self, name: &str) -> Option<i32> {
        crate::dispense::sound_of(name)
    }
}

/// A hopper exists at `p`: it gets ticked.
pub fn register(ctx: &ServerContext, p: Position) {
    lock(&ctx.hoppers).insert(to_pos(p));
}

/// One game tick of every hopper minecart that is on (see
/// `transfer::tick_cart`).
pub fn tick_carts(ctx: &ServerContext) {
    let carts: Vec<(i32, [f64; 3])> = lock(&ctx.entities)
        .iter()
        .filter(|e| {
            !e.removed
                && matches!(&e.kind, mc_entity::Kind::Vehicle(v)
                    if v.enabled && v.craft == Craft::Minecart(Cart::Hopper))
        })
        .map(|e| (e.id, e.pos))
        .collect();
    let Some(kind) = crate::containers::entity_kind("minecraft:hopper_minecart") else {
        return;
    };
    let mut host = Host::new(ctx);
    for (id, pos) in carts {
        transfer::tick_cart(&mut host, Host::entity_store(id, kind), pos);
    }
}

/// One game tick of every known hopper.
pub fn tick(ctx: &ServerContext, level: &mut WorldLevel) {
    let logic = level.logic();
    let all: Vec<_> = lock(&ctx.hoppers).iter().copied().collect();
    for at in all {
        let p = crate::level::to_position(at);
        // Hoppers in unloaded chunks wait; loading the chunk wakes them again.
        // Outside the block-ticking square they wait without being forgotten.
        if !ctx.world.is_loaded(p) {
            lock(&ctx.hoppers).remove(at);
            continue;
        }
        if !crate::tickets::blocks_run(ctx, at.x >> 4, at.z >> 4) {
            continue;
        }
        if !mc_tick::hopper::tick(level, logic, at) {
            lock(&ctx.hoppers).remove(at);
        }
    }
}

fn centre_of(p: Position) -> [f64; 3] {
    [f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5]
}

/// The behaviour of a dispenser for `name`, acting on the block `front`.
fn behavior(
    ctx: &ServerContext,
    level: &mut crate::level::WorldLevel,
    name: &str,
    stack: &ItemStack,
    p: Position,
    facing: Dir,
) -> Behavior {
    let (dx, dy, dz) = facing.offset();
    let front = Position::new(p.x + dx, p.y + dy, p.z + dz);
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(front, blocks).unwrap_or(0);
    let block = blocks.block_name(state).unwrap_or("");
    let item = |n: &str| ctx.items.id(n);
    match name {
        "minecraft:water_bucket"
        | "minecraft:lava_bucket"
        | "minecraft:cod_bucket"
        | "minecraft:salmon_bucket"
        | "minecraft:pufferfish_bucket"
        | "minecraft:tropical_fish_bucket"
        | "minecraft:axolotl_bucket"
        | "minecraft:tadpole_bucket" => {
            let fluid = if name == "minecraft:lava_bucket" {
                "minecraft:lava"
            } else {
                "minecraft:water"
            };
            let free = ctx.shapes.is_air(state) || ctx.rules.is_replaceable(state);
            let logs = blocks.property(state, "waterlogged") == Some("false");
            // In the Nether the water is gone at once, including into a
            // waterloggable block; the bucket is empty.
            if fluid == "minecraft:water" && mc_tick::Level::ultrawarm(level) && (free || logs) {
                if free && !ctx.shapes.is_air(state) {
                    level.set_block(front, ctx.shapes.air());
                }
                mc_tick::Level::sponge_dried(level, crate::level::to_pos(front));
                return done(item("minecraft:bucket"));
            }
            let placed = if fluid == "minecraft:water"
                && blocks.property(state, "waterlogged") == Some("false")
            {
                level.set_block(front, crate::campfires::waterlogged(ctx, front, state))
            } else if ctx.shapes.is_air(state) || ctx.rules.is_replaceable(state) {
                let source = ctx.shapes.default_state(fluid).unwrap_or(0);
                let same = block == fluid && blocks.property(state, "level") == Some("0");
                !same && level.set_block(front, source)
            } else {
                false
            };
            if placed {
                done(item("minecraft:bucket"))
            } else {
                Behavior::Default
            }
        }
        "minecraft:bucket" => {
            let (new, full) = if block == "minecraft:powder_snow" {
                (ctx.shapes.air(), "minecraft:powder_snow_bucket")
            } else if blocks.property(state, "waterlogged") == Some("true") {
                (
                    ctx.shapes.with(state, "waterlogged", "false"),
                    "minecraft:water_bucket",
                )
            } else if blocks.property(state, "level") != Some("0") {
                return Behavior::Default;
            } else {
                match block {
                    "minecraft:water" => (ctx.shapes.air(), "minecraft:water_bucket"),
                    "minecraft:lava" => (ctx.shapes.air(), "minecraft:lava_bucket"),
                    _ => return Behavior::Default,
                }
            };
            match item(full) {
                Some(full) if level.set_block(front, new) => done(Some(full)),
                _ => Behavior::Default,
            }
        }
        "minecraft:bone_meal" => {
            if crate::growth::bone_meal(level, front)
                || crate::growth::bone_meal_water(level, front, None)
            {
                done(None)
            } else {
                Behavior::Failed
            }
        }
        "minecraft:tnt" => {
            // It hops up, a little to one side, as lit TNT does.
            let mut tnt = crate::explosion::primed(ctx, front, crate::explosion::TNT_FUSE, None);
            tnt.pos = centre_of(front);
            let _ = ctx
                .events
                .send(crate::context::Event::EntitySpawned(tnt.clone()));
            lock(&ctx.entities).push(tnt);
            done(None)
        }
        "minecraft:flint_and_steel" => {
            let lit = block.ends_with("campfire") || block.ends_with("candle");
            let ok = if block == "minecraft:tnt" {
                crate::explosion::ignite(ctx, level, front, None)
            } else if lit
                && blocks.property(state, "lit") == Some("false")
                && blocks.property(state, "waterlogged") != Some("true")
            {
                level.set_block(front, ctx.shapes.with(state, "lit", "true"))
            } else if let Some(fire) =
                mc_tick::fire::can_light(&*level, level.logic(), crate::level::to_pos(front))
            {
                level.set_block(front, fire)
            } else {
                false
            };
            if !ok {
                Behavior::Failed
            } else {
                Behavior::Done {
                    leftover: None,
                    wear: 1,
                }
            }
        }
        "minecraft:shears" => {
            let honey = matches!(block, "minecraft:beehive" | "minecraft:bee_nest")
                && blocks.property(state, "honey_level") == Some("5");
            if honey {
                level.set_block(front, ctx.shapes.with(state, "honey_level", "0"));
                if let Some(comb) = item("minecraft:honeycomb") {
                    crate::tick::spawn_item_at(ctx, centre_of(front), comb, 3);
                }
                Behavior::Done {
                    leftover: None,
                    wear: 1,
                }
            } else if crate::decor::leash::snip_at(ctx, front) {
                Behavior::Done {
                    leftover: None,
                    wear: 1,
                }
            } else {
                // Else the sheep, mooshrooms and leads in front.
                crate::dispense::behavior(level, name, stack, p, facing)
                    .unwrap_or(Behavior::Default)
            }
        }
        "minecraft:glass_bottle" => {
            let honey = matches!(block, "minecraft:beehive" | "minecraft:bee_nest")
                && blocks.property(state, "honey_level") == Some("5");
            if honey {
                level.set_block(front, ctx.shapes.with(state, "honey_level", "0"));
                return done(item("minecraft:honey_bottle"));
            }
            // A water source (or a waterlogged block).
            let water = (block == "minecraft:water"
                && blocks.property(state, "level") == Some("0"))
                || blocks.property(state, "waterlogged") == Some("true");
            match item("minecraft:potion") {
                Some(potion) if water => {
                    let mut contents = mc_nbt::Compound::new();
                    contents.insert("potion", "minecraft:water");
                    let mut bottle = ItemStack::new(potion, 1);
                    bottle
                        .components
                        .set("potion_contents", Tag::Compound(contents));
                    Behavior::Done {
                        leftover: Some(bottle),
                        wear: 0,
                    }
                }
                _ => Behavior::Default,
            }
        }
        _ => crate::dispense::behavior(level, name, stack, p, facing).unwrap_or(Behavior::Default),
    }
}

/// The inventory slot an item is worn in, if it is armour.
fn armor_slot(name: &str) -> Option<usize> {
    let n = name.trim_start_matches("minecraft:");
    if n.ends_with("_helmet")
        || n.ends_with("_head")
        || n.ends_with("_skull")
        || n == "carved_pumpkin"
    {
        Some(5)
    } else if n.ends_with("_chestplate") || n == "elytra" {
        Some(6)
    } else if n.ends_with("_leggings") {
        Some(7)
    } else if n.ends_with("_boots") {
        Some(8)
    } else {
        None
    }
}

/// Armour dispensed at a player standing in front: it is put on if that
/// slot is free.
fn dress_player(ctx: &ServerContext, front: Position, stack: &ItemStack) -> bool {
    let Some(name) = ctx.items.name(stack.item) else {
        return false;
    };
    let Some(slot) = armor_slot(name) else {
        return false;
    };
    let (fx, fy, fz) = (f64::from(front.x), f64::from(front.y), f64::from(front.z));
    let candidates: Vec<u128> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter(|p| p.game_mode != 3)
        .filter_map(|p| p.pose.map(|pose| (p.profile.uuid, pose)))
        .filter(|(_, pose)| {
            pose.x + 0.3 > fx
                && pose.x - 0.3 < fx + 1.0
                && pose.z + 0.3 > fz
                && pose.z - 0.3 < fz + 1.0
                && pose.y + 1.8 > fy
                && pose.y < fy + 1.0
        })
        .map(|(uuid, _)| uuid)
        .collect();
    let mut inventories = lock(&ctx.inventories);
    for uuid in candidates {
        let Some(hands) = inventories.get_mut(&uuid) else {
            continue;
        };
        if hands.slots[slot].is_some() {
            continue;
        }
        hands.slots[slot] = Some(stack.with_count(1));
        let _ = ctx.events.send(crate::context::Event::InventorySlot {
            uuid,
            slot: slot as i16,
            item: hands.slots[slot].clone(),
        });
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers;

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    /// A dispenser at the origin holding `item`, facing east.
    fn loaded(ctx: &ServerContext, item: &str, count: i32) -> Position {
        let p = Position::new(0, -60, 0);
        let state = ctx
            .data
            .blocks
            .default_state("minecraft:dispenser")
            .unwrap();
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

    fn fire(ctx: &ServerContext, p: Position) {
        let mut level = WorldLevel::new(ctx);
        dispense(&mut level, p, Dir::East, false);
    }

    fn slot0(ctx: &ServerContext, p: Position) -> Option<ItemStack> {
        let kind = containers::kind_of("minecraft:dispenser").unwrap();
        containers::slots(ctx, p, kind)[0].clone()
    }

    #[test]
    fn bucket_pours_and_scoops() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:water_bucket", 1);
        fire(&ctx, p);
        let front = Position::new(1, -60, 0);
        let name = |ctx: &ServerContext| {
            ctx.world
                .block(front, &ctx.data.blocks)
                .and_then(|s| ctx.data.blocks.block_name(s).map(str::to_owned))
        };
        assert_eq!(name(&ctx).as_deref(), Some("minecraft:water"));
        assert_eq!(
            slot0(&ctx, p).map(|s| s.item),
            ctx.items.id("minecraft:bucket")
        );
        // The empty bucket takes the water back.
        fire(&ctx, p);
        assert_eq!(name(&ctx).as_deref(), Some("minecraft:air"));
        assert_eq!(
            slot0(&ctx, p).map(|s| s.item),
            ctx.items.id("minecraft:water_bucket")
        );
    }

    #[test]
    fn a_dispenser_cannot_waterlog_a_block_in_the_nether() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let nether = ctx.dimension(1).expect("the nether");
        let p = Position::new(0, 70, 0);
        let front = Position::new(1, 70, 0);
        let _ = nether.world.block(p, &nether.data.blocks);
        let slab = nether
            .data
            .blocks
            .default_state("minecraft:oak_slab")
            .unwrap();
        assert!(WorldLevel::new(&nether).set_block(front, slab));
        let disp = nether
            .data
            .blocks
            .state_id("minecraft:dispenser", &[("facing", "east")])
            .unwrap();
        assert!(WorldLevel::new(&nether).set_block(p, disp));
        let item = nether.items.id("minecraft:water_bucket").unwrap();
        containers::put(
            &nether,
            p,
            containers::kind_of("minecraft:dispenser").unwrap(),
            &[(0, Some(ItemStack::new(item, 1)))],
        );
        fire(&nether, p);
        let state = nether.world.block(front, &nether.data.blocks).unwrap();
        assert_eq!(
            nether.data.blocks.block_name(state),
            Some("minecraft:oak_slab")
        );
        assert_eq!(
            nether.data.blocks.property(state, "waterlogged"),
            Some("false")
        );
        assert_eq!(
            slot0(&nether, p).map(|s| s.item),
            nether.items.id("minecraft:bucket")
        );
    }

    #[test]
    fn tnt_is_primed_not_dropped() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:tnt", 2);
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.count), Some(1));
        let entities = lock(&ctx.entities);
        assert!(matches!(
            entities[..],
            [mc_entity::Entity {
                kind: mc_entity::Kind::Tnt { .. },
                ..
            }]
        ));
    }

    #[test]
    fn bone_meal_needs_a_plant() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:bone_meal", 3);
        // Nothing to grow: the meal stays and nothing drops.
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.count), Some(3));
        assert!(lock(&ctx.entities).is_empty());
        let front = Position::new(1, -60, 0);
        let wheat = ctx.data.blocks.default_state("minecraft:wheat").unwrap();
        WorldLevel::new(&ctx).set_block(front, wheat);
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.count), Some(2));
        let age = ctx
            .world
            .block(front, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.property(s, "age").map(str::to_owned));
        assert!(age.is_some_and(|a| a != "0"));
    }

    #[test]
    fn flint_and_steel_wears() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:flint_and_steel", 1);
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.damage), Some(1));
        assert!(lock(&ctx.entities).is_empty());
    }

    #[test]
    fn plain_items_drop() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:stick", 2);
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.count), Some(1));
        assert_eq!(lock(&ctx.entities).len(), 1);
    }

    fn cart(ctx: &ServerContext, cart: Cart, pos: [f64; 3]) -> i32 {
        let id = ctx.next_entity_id();
        lock(&ctx.entities).push(mc_entity::Entity {
            id,
            uuid: 1,
            kind: mc_entity::Kind::Vehicle(mc_entity::extra::Vehicle::new(
                Craft::Minecart(cart),
                0.0,
            )),
            pos,
            vel: [0.0; 3],
            on_ground: true,
            age: 0,
            removed: false,
            tags: Vec::new(),
        });
        id
    }

    #[test]
    fn hoppers_and_minecarts_pass_items() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let hopper = Position::new(0, -60, 0);
        WorldLevel::new(&ctx).set_block(hopper, blocks.default_state("minecraft:hopper").unwrap());
        WorldLevel::new(&ctx).set_block(
            Position::new(0, -59, 0),
            blocks.default_state("minecraft:rail").unwrap(),
        );
        // A chest minecart on the rail over the hopper is emptied into it.
        let chest = cart(&ctx, Cart::Chest, [0.5, -59.0, 0.5]);
        let chest_kind = containers::entity_kind("minecraft:chest_minecart").unwrap();
        let stick = ctx.items.id("minecraft:stick").unwrap();
        containers::put(
            &ctx,
            containers::entity_key(chest),
            chest_kind,
            &[(3, Some(ItemStack::new(stick, 2)))],
        );
        assert!(transfer(
            &ctx,
            Position::new(0, -59, 0),
            Dir::Down,
            hopper,
            Dir::Up
        ));
        let hopper_kind = containers::kind_of("minecraft:hopper").unwrap();
        assert_eq!(
            containers::slots(&ctx, hopper, hopper_kind)[0]
                .as_ref()
                .map(|s| s.count),
            Some(1)
        );
        assert_eq!(
            containers::slots(&ctx, containers::entity_key(chest), chest_kind)[3]
                .as_ref()
                .map(|s| s.count),
            Some(1)
        );
        // A hopper minecart under a chest takes from it.
        let box_at = Position::new(5, -59, 0);
        WorldLevel::new(&ctx).set_block(box_at, blocks.default_state("minecraft:chest").unwrap());
        let chest_block = containers::kind_of("minecraft:chest").unwrap();
        containers::put(
            &ctx,
            box_at,
            chest_block,
            &[(0, Some(ItemStack::new(stick, 1)))],
        );
        let hc = cart(&ctx, Cart::Hopper, [5.5, -60.0, 0.5]);
        tick_carts(&ctx);
        let hc_kind = containers::entity_kind("minecraft:hopper_minecart").unwrap();
        assert_eq!(
            containers::slots(&ctx, containers::entity_key(hc), hc_kind)[0]
                .as_ref()
                .map(|s| s.item),
            Some(stick)
        );
        assert!(containers::slots(&ctx, box_at, chest_block)[0].is_none());
    }

    /// Hoppers A → B → a chest, ticked in `order` (0 is A, 1 is B) each
    /// game tick; the tick the chest gets its first item.
    fn chain(ctx: &ServerContext, x: i32, order: [usize; 2]) -> Option<usize> {
        let blocks = &ctx.data.blocks;
        let east = ctx.shapes.with(
            blocks.default_state("minecraft:hopper").unwrap(),
            "facing",
            "east",
        );
        let (a, b, c) = (
            Position::new(x, -60, 0),
            Position::new(x + 1, -60, 0),
            Position::new(x + 2, -60, 0),
        );
        WorldLevel::new(ctx).set_block(a, east);
        WorldLevel::new(ctx).set_block(b, east);
        WorldLevel::new(ctx).set_block(c, blocks.default_state("minecraft:chest").unwrap());
        let stick = ctx.items.id("minecraft:stick").unwrap();
        let hopper = containers::kind_of("minecraft:hopper").unwrap();
        containers::put(ctx, a, hopper, &[(0, Some(ItemStack::new(stick, 2)))]);
        let chest = containers::kind_of("minecraft:chest").unwrap();
        for tick in 0..20 {
            let mut level = WorldLevel::new(ctx);
            let logic = level.logic();
            for &i in &order {
                mc_tick::hopper::tick(&mut level, logic, crate::level::to_pos([a, b][i]));
            }
            if containers::slots(ctx, c, chest)[0].is_some() {
                return Some(tick);
            }
        }
        None
    }

    #[test]
    fn a_hopper_handed_its_first_item_waits_seven_turns() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        // Whether B had its turn before A filled it or not, it passes the
        // item on 7 ticks later.
        assert_eq!(chain(&ctx, 10, [0, 1]), Some(7));
        assert_eq!(chain(&ctx, 20, [1, 0]), Some(7));
    }

    #[test]
    fn hoppers_in_a_chunk_read_from_disk_tick() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        if mc_data::McData::load(&dir).is_err() {
            eprintln!("skipped: no game data");
            return;
        }
        let folder = std::env::temp_dir().join(format!("mc-hoppers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let open = || {
            let mut config = crate::Config::new(dir.clone());
            config.world = Some(folder.clone());
            config.seed = Some(12345);
            ServerContext::new(config, mc_data::McData::load(&dir).unwrap()).unwrap()
        };
        let (hopper, chest) = (Position::new(40, 200, 40), Position::new(40, 199, 40));
        let stick = {
            let ctx = open();
            let blocks = &ctx.data.blocks;
            // Locked by nothing, facing down into a chest.
            WorldLevel::new(&ctx)
                .set_block(chest, blocks.default_state("minecraft:chest").unwrap());
            WorldLevel::new(&ctx)
                .set_block(hopper, blocks.default_state("minecraft:hopper").unwrap());
            let stick = ctx.items.id("minecraft:stick").unwrap();
            containers::put(
                &ctx,
                hopper,
                containers::kind_of("minecraft:hopper").unwrap(),
                &[(0, Some(ItemStack::new(stick, 3)))],
            );
            lock(&ctx.hoppers).remove(to_pos(hopper));
            crate::save(&ctx);
            stick
        };
        let ctx = open();
        assert!(lock(&ctx.hoppers).iter().next().is_none());
        // Reading the chunk wakes the hopper: it pushes on the next tick.
        let _ = ctx.world.block(hopper, &ctx.data.blocks);
        crate::tick::run_scheduled_ticks(&ctx);
        let chest_kind = containers::kind_of("minecraft:chest").unwrap();
        assert_eq!(
            containers::slots(&ctx, chest, chest_kind)[0]
                .as_ref()
                .map(|s| (s.item, s.count)),
            Some((stick, 1))
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// A world of hopper setups for [`shortcuts_change_nothing`]: chains
    /// into and out of chests, a full hopper under a chest, an empty one,
    /// a locked one, composters fed and emptied, a furnace, a brewing
    /// stand, a jukebox, minecarts both ways and items thrown in. Returns
    /// the containers to compare.
    fn setups(ctx: &ServerContext) -> Vec<(Position, containers::Kind)> {
        let blocks = &ctx.data.blocks;
        let state = |name: &str, props: &[(&str, &str)]| blocks.state_id(name, props).unwrap();
        let place = |p: Position, s: u32| assert!(WorldLevel::new(ctx).set_block(p, s));
        let item = |n: &str| ctx.items.id(n).unwrap();
        let kind = |n: &str| containers::kind_of(n).unwrap();
        let hopper = |facing: &str| state("minecraft:hopper", &[("facing", facing)]);
        let y = -60;
        let watch = std::cell::RefCell::new(Vec::new());
        let fill = |p: Position, block: &str, slots: &[(usize, &str, i32)]| {
            let changes: Vec<_> = slots
                .iter()
                .map(|&(i, n, c)| (i, Some(ItemStack::new(item(n), c))))
                .collect();
            containers::put(ctx, p, kind(block), &changes);
            watch.borrow_mut().push((p, kind(block)));
        };
        // A chest emptied down a chain of three hoppers into a chest; the
        // middle one starts full of something else.
        place(Position::new(0, y + 1, 0), state("minecraft:chest", &[]));
        place(Position::new(0, y, 0), hopper("east"));
        place(Position::new(1, y, 0), hopper("east"));
        place(Position::new(2, y, 0), hopper("east"));
        place(Position::new(3, y, 0), state("minecraft:chest", &[]));
        fill(
            Position::new(0, y + 1, 0),
            "minecraft:chest",
            &[(0, "minecraft:stick", 20), (5, "minecraft:cobblestone", 3)],
        );
        fill(Position::new(0, y, 0), "minecraft:hopper", &[]);
        fill(
            Position::new(1, y, 0),
            "minecraft:hopper",
            &[
                (0, "minecraft:dirt", 64),
                (1, "minecraft:dirt", 64),
                (2, "minecraft:dirt", 64),
                (3, "minecraft:dirt", 64),
                (4, "minecraft:dirt", 63),
            ],
        );
        fill(Position::new(2, y, 0), "minecraft:hopper", &[]);
        fill(
            Position::new(3, y, 0),
            "minecraft:chest",
            &[(0, "minecraft:stick", 63)],
        );
        // A full hopper under a chest: nothing comes down.
        place(Position::new(0, y + 1, 4), state("minecraft:chest", &[]));
        place(Position::new(0, y, 4), hopper("down"));
        let full: Vec<_> = (0..5).map(|i| (i, "minecraft:cobblestone", 64)).collect();
        fill(Position::new(0, y, 4), "minecraft:hopper", &full);
        fill(
            Position::new(0, y + 1, 4),
            "minecraft:chest",
            &[(0, "minecraft:stone", 5)],
        );
        // A locked hopper over a chest.
        place(
            Position::new(4, y, 4),
            state(
                "minecraft:hopper",
                &[("facing", "down"), ("enabled", "false")],
            ),
        );
        place(Position::new(4, y - 1, 4), state("minecraft:chest", &[]));
        fill(
            Position::new(4, y, 4),
            "minecraft:hopper",
            &[(2, "minecraft:stick", 4)],
        );
        fill(Position::new(4, y - 1, 4), "minecraft:chest", &[]);
        // Pumpkin pie into a composter (always a layer), and bone meal out
        // of a full one.
        place(Position::new(8, y + 1, 0), hopper("down"));
        place(Position::new(8, y, 0), state("minecraft:composter", &[]));
        fill(
            Position::new(8, y + 1, 0),
            "minecraft:hopper",
            &[(0, "minecraft:pumpkin_pie", 9)],
        );
        place(
            Position::new(8, y + 1, 4),
            state("minecraft:composter", &[("level", "8")]),
        );
        place(Position::new(8, y, 4), hopper("east"));
        fill(Position::new(8, y, 4), "minecraft:hopper", &[]);
        // Into a furnace's top and side, and a brewing stand.
        place(Position::new(12, y + 1, 0), hopper("down"));
        place(Position::new(12, y, 0), state("minecraft:furnace", &[]));
        place(Position::new(11, y, 0), hopper("east"));
        fill(
            Position::new(12, y + 1, 0),
            "minecraft:hopper",
            &[(0, "minecraft:raw_iron", 3)],
        );
        fill(
            Position::new(11, y, 0),
            "minecraft:hopper",
            &[(0, "minecraft:coal", 2), (1, "minecraft:stick", 2)],
        );
        fill(Position::new(12, y, 0), "minecraft:furnace", &[]);
        place(Position::new(12, y + 1, 4), hopper("down"));
        place(
            Position::new(12, y, 4),
            state("minecraft:brewing_stand", &[]),
        );
        fill(
            Position::new(12, y + 1, 4),
            "minecraft:hopper",
            &[(0, "minecraft:nether_wart", 2), (1, "minecraft:stick", 1)],
        );
        fill(Position::new(12, y, 4), "minecraft:brewing_stand", &[]);
        // A disc into a jukebox.
        place(Position::new(16, y + 1, 0), hopper("down"));
        place(Position::new(16, y, 0), state("minecraft:jukebox", &[]));
        fill(
            Position::new(16, y + 1, 0),
            "minecraft:hopper",
            &[(0, "minecraft:music_disc_cat", 1)],
        );
        // A chest minecart over a hopper; a hopper minecart under a chest.
        place(Position::new(20, y, 0), hopper("east"));
        place(Position::new(21, y, 0), state("minecraft:chest", &[]));
        place(Position::new(20, y + 1, 0), state("minecraft:rail", &[]));
        fill(Position::new(20, y, 0), "minecraft:hopper", &[]);
        fill(Position::new(21, y, 0), "minecraft:chest", &[]);
        let chest = cart(ctx, Cart::Chest, [20.5, f64::from(y + 1), 0.5]);
        let chest_kind = containers::entity_kind("minecraft:chest_minecart").unwrap();
        containers::put(
            ctx,
            containers::entity_key(chest),
            chest_kind,
            &[(3, Some(ItemStack::new(item("minecraft:stick"), 4)))],
        );
        watch
            .borrow_mut()
            .push((containers::entity_key(chest), chest_kind));
        place(Position::new(24, y + 1, 0), state("minecraft:chest", &[]));
        fill(
            Position::new(24, y + 1, 0),
            "minecraft:chest",
            &[(7, "minecraft:apple", 3)],
        );
        let hc = cart(ctx, Cart::Hopper, [24.5, f64::from(y), 0.5]);
        watch.borrow_mut().push((
            containers::entity_key(hc),
            containers::entity_kind("minecraft:hopper_minecart").unwrap(),
        ));
        // Empty hoppers with items thrown in over them, one over a chest.
        place(Position::new(28, y, 0), hopper("down"));
        place(Position::new(28, y - 1, 0), state("minecraft:chest", &[]));
        place(Position::new(28, y, 4), hopper("east"));
        fill(Position::new(28, y, 0), "minecraft:hopper", &[]);
        fill(Position::new(28, y - 1, 0), "minecraft:chest", &[]);
        fill(Position::new(28, y, 4), "minecraft:hopper", &[]);
        crate::tick::spawn_item_at(
            ctx,
            [28.5, f64::from(y) + 1.0, 0.5],
            item("minecraft:stick"),
            3,
        );
        crate::tick::spawn_item_at(
            ctx,
            [28.5, f64::from(y) + 1.5, 4.5],
            item("minecraft:egg"),
            20,
        );
        watch.into_inner()
    }

    /// One game tick of every hopper and hopper minecart, with the
    /// shortcuts (containers kept decoded, the entity index, vanilla's
    /// empty and full checks) or without; what then is where.
    fn step(
        ctx: &ServerContext,
        shortcuts: bool,
        watch: &[(Position, containers::Kind)],
        tick: usize,
    ) -> String {
        if tick == 6 {
            // Something thrown in on the way.
            let egg = ctx.items.id("minecraft:egg").unwrap();
            crate::tick::spawn_item_at(ctx, [28.5, -59.0, 0.5], egg, 2);
        }
        if shortcuts {
            containers::keep(ctx);
        }
        let mut level = WorldLevel::new(ctx);
        let logic = level.logic();
        let all: Vec<_> = lock(&ctx.hoppers).iter().copied().collect();
        for at in all {
            mc_tick::hopper::tick_with(&mut level, logic, at, shortcuts);
        }
        drop(level);
        tick_carts(ctx);
        if shortcuts {
            containers::release(ctx);
        }
        let mut seen = String::new();
        for &(p, kind) in watch {
            let slots = containers::slots(ctx, p, kind);
            let stored = (containers::entity_of(p).is_none()).then(|| {
                ctx.world
                    .block_entity(p, &ctx.data.blocks)
                    .map(|be| be.data)
            });
            seen += &format!("{p:?} {slots:?} {stored:?} {}\n", cooldown(ctx, p));
        }
        for x in [8, 12, 16] {
            for z in [0, 4] {
                for dy in [-1, 0, 1] {
                    let p = Position::new(x, -60 + dy, z);
                    seen += &format!("{:?}\n", ctx.world.block(p, &ctx.data.blocks));
                }
            }
        }
        for e in lock(&ctx.entities).iter() {
            seen += &format!(
                "{} {:?} {}\n",
                e.id,
                e.kind.as_stack().map(|s| s.count),
                e.removed
            );
        }
        seen
    }

    #[test]
    fn shortcuts_change_nothing() {
        let (Some(fast), Some(plain)) = (context(), context()) else {
            eprintln!("skipped: no game data");
            return;
        };
        let watch = setups(&fast);
        assert_eq!(setups(&plain).len(), watch.len());
        let mut moved = 0;
        let mut last = String::new();
        for tick in 0..60 {
            let a = step(&fast, true, &watch, tick);
            let b = step(&plain, false, &watch, tick);
            assert_eq!(a, b, "tick {tick}");
            moved += usize::from(a != last);
            last = a;
        }
        // The setups really do something over the run.
        assert!(moved > 10, "only {moved} ticks changed anything");
    }
}
