//! Campfires and soul campfires on the server: the rules are in
//! `mc_tick::block_entity::campfire`; this keeps the campfires with food on
//! them ticking, drops the food when one breaks, and puts one out when water
//! is poured in.

use std::collections::HashSet;

use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::BlockPos;
use mc_tick::block_entity::campfire;

use crate::block_entities::Host;
use crate::context::ServerContext;
use crate::level::to_pos;
use crate::tick::lock;

pub use mc_tick::block_entity::campfire::{Food, is_campfire};

pub type Active = HashSet<(i32, i32, i32)>;

fn block_name(ctx: &ServerContext, p: Position) -> Option<&str> {
    ctx.world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
}

/// The food on the campfire at `p`.
pub fn food(ctx: &ServerContext, p: Position) -> Food {
    campfire::food(&Host::new(ctx), to_pos(p))
}

/// Puts one `stack` on the campfire at `p`; `false` when it is full or the
/// item does not cook.
pub fn place(ctx: &ServerContext, p: Position, stack: &ItemStack) -> bool {
    if !campfire::place(&mut Host::new(ctx), to_pos(p), stack) {
        return false;
    }
    wake(ctx, p);
    true
}

/// The campfire at `p` is gone: what was on it, to drop.
pub fn broken(ctx: &ServerContext, p: Position) -> Vec<ItemStack> {
    lock(&ctx.campfires).remove(&(p.x, p.y, p.z));
    food(ctx, p).items.into_iter().flatten().collect()
}

/// The campfire at `p` may have food to cook.
pub fn wake(ctx: &ServerContext, p: Position) {
    if block_name(ctx, p).is_some_and(is_campfire) {
        lock(&ctx.campfires).insert((p.x, p.y, p.z));
    }
}

/// One tick of every campfire with food on it.
pub fn tick(ctx: &ServerContext) {
    let active: Vec<(i32, i32, i32)> = lock(&ctx.campfires).iter().copied().collect();
    let mut host = Host::new(ctx);
    for key in active {
        if !crate::tickets::blocks_run(ctx, key.0 >> 4, key.2 >> 4) {
            continue;
        }
        let p = BlockPos::new(key.0, key.1, key.2);
        if !campfire::tick(&mut host, &ctx.data.blocks, p) {
            lock(&ctx.campfires).remove(&key);
        }
    }
}

/// `state` with water poured in: a lit campfire goes out with a hiss.
pub fn waterlogged(ctx: &ServerContext, p: Position, state: u32) -> u32 {
    let wet = ctx.shapes.with(state, "waterlogged", "true");
    let blocks = &ctx.data.blocks;
    if blocks.block_name(state).is_some_and(is_campfire)
        && blocks.property(state, "lit") == Some("true")
    {
        crate::fx::sound(
            ctx,
            "block.fire.extinguish",
            crate::fx::category::BLOCKS,
            crate::blockuse::centre(p),
            1.0,
            1.0,
        );
        return ctx.shapes.with(wet, "lit", "false");
    }
    wet
}

/// Damage a lit campfire at the feet does: 1, or 2 for a soul campfire.
pub fn damage(ctx: &ServerContext, state: u32) -> Option<f32> {
    campfire::damage(&ctx.data.blocks, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::WorldLevel;

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    #[test]
    fn cooks_four_and_cools_unlit() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(0, -60, 0);
        let blocks = &ctx.data.blocks;
        let lit = blocks.default_state("minecraft:campfire").unwrap();
        assert_eq!(blocks.property(lit, "lit"), Some("true"));
        WorldLevel::new(&ctx).set_block(p, lit);
        let beef = ItemStack::new(ctx.items.id("minecraft:beef").unwrap(), 5);
        let dirt = ItemStack::new(ctx.items.id("minecraft:dirt").unwrap(), 1);
        assert!(!place(&ctx, p, &dirt));
        for _ in 0..4 {
            assert!(place(&ctx, p, &beef));
        }
        assert!(!place(&ctx, p, &beef), "only four fit");
        for _ in 0..100 {
            tick(&ctx);
        }
        assert_eq!(food(&ctx, p).times, [100; 4]);
        // Put out: cooking goes back two a tick.
        let out = ctx.shapes.with(lit, "lit", "false");
        WorldLevel::new(&ctx).set_block(p, out);
        for _ in 0..10 {
            tick(&ctx);
        }
        assert_eq!(food(&ctx, p).times, [80; 4]);
        WorldLevel::new(&ctx).set_block(p, lit);
        wake(&ctx, p);
        for _ in 0..520 {
            tick(&ctx);
        }
        let f = food(&ctx, p);
        assert!(f.items.iter().all(Option::is_none), "{f:?}");
        let cooked = ctx.items.id("minecraft:cooked_beef").unwrap();
        let popped = lock(&ctx.entities)
            .iter()
            .filter(|e| matches!(&e.kind, mc_entity::Kind::Item { item, .. } if *item == cooked))
            .count();
        assert_eq!(popped, 4);
        assert!(lock(&ctx.campfires).is_empty());
        assert_eq!(damage(&ctx, lit), Some(1.0));
        let soul = blocks.default_state("minecraft:soul_campfire").unwrap();
        assert_eq!(damage(&ctx, soul), Some(2.0));
        assert_eq!(damage(&ctx, out), None);
    }
}
