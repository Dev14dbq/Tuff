//! Where players may not touch blocks (minecraft.wiki, "Spawn protection",
//! "Adventure"; `spawn-protection` in `server.properties`):
//! - spawn protection: within the radius of the world spawn, square and
//!   counted in blocks sideways, only operators may break, place or use
//!   things. None where the world has no operators at all, none outside the
//!   spawn's dimension, none at radius 0;
//! - the world border: nothing outside it;
//! - adventure mode: a player breaks only the blocks the item in the main
//!   hand lists in its `can_break` component, and an item acts on a block
//!   only if its `can_place_on` lists that block. A list is a block
//!   predicate (`blocks`, `state`) or several, any of which may match.
//!   The `nbt` of a predicate is not looked at.

use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use serde_json::Value;

use crate::context::{Event, ServerContext};
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

const ADVENTURE: i32 = 2;

/// Whether the block at `pos` is inside the spawn protection of this
/// dimension for this player (an operator never is).
pub fn under_spawn_protection(ctx: &ServerContext, uuid: u128, pos: Position) -> bool {
    let radius = ctx.config.spawn_protection;
    let spawn = lock(&ctx.spawn).clone();
    if ctx.world.dimension != spawn.dimension || radius <= 0 {
        return false;
    }
    let name = lock(&ctx.players)
        .get(&uuid)
        .map(|p| p.profile.name.clone())
        .unwrap_or_default();
    {
        let lists = lock(&ctx.lists);
        if lists.ops.is_empty() || lists.op(uuid, &name).is_some() {
            return false;
        }
    }
    let (dx, dz) = (
        i64::from(pos.x - spawn.pos.0).abs(),
        i64::from(pos.z - spawn.pos.2).abs(),
    );
    dx.max(dz) <= i64::from(radius)
}

/// Whether the player may do things to the block at `pos`: not under spawn
/// protection and inside the world border.
pub fn may_interact(ctx: &ServerContext, uuid: u128, pos: Position) -> bool {
    if under_spawn_protection(ctx, uuid, pos) {
        return false;
    }
    lock(&ctx.border).contains_block(pos.x, pos.z)
}

/// Tells the player in the action bar that the block is protected.
pub fn protected_message(outcome: &mut Outcome, uuid: u128, pos: Position) {
    outcome.events.push(Event::Message {
        uuid,
        line: mc_protocol::text::translate_colored(
            "build.spawn_protection",
            vec![mc_protocol::text::plain(&format!(
                "{}, {}, {}",
                pos.x, pos.y, pos.z
            ))],
            "red",
        ),
        overlay: true,
    });
}

/// Whether a block predicate (or a list of them, or `{predicates: [...]}`
/// as once written) of an item's `can_break` or `can_place_on` matches the
/// block at `at`.
pub fn lists_block(ctx: &ServerContext, predicates: &Value, at: Position) -> bool {
    let list: Vec<&Value> = match predicates {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) if o.contains_key("predicates") => predicates["predicates"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        Value::Object(_) => vec![predicates],
        _ => Vec::new(),
    };
    list.into_iter()
        .any(|p| crate::criteria::block_is(ctx, p, at))
}

/// Whether the stack lists the block at `at` in its component
/// (`minecraft:can_break` or `minecraft:can_place_on`).
pub fn stack_lists(
    ctx: &ServerContext,
    stack: Option<&ItemStack>,
    component: &str,
    at: Position,
) -> bool {
    stack
        .and_then(|s| ctx.items.component(s, component))
        .is_some_and(|p| lists_block(ctx, &p, at))
}

/// An adventure player may break the block only with a main-hand item that
/// lists it in `can_break`; spectators may not at all. `true` means the
/// action is not allowed.
pub fn break_restricted(ctx: &ServerContext, actor: &Actor, at: Position) -> bool {
    match actor.game_mode {
        3 => true,
        ADVENTURE => {
            let held = crate::held::held(ctx, actor.uuid, 0);
            !stack_lists(
                ctx,
                held.as_ref().map(|h| &h.stack),
                "minecraft:can_break",
                at,
            )
        }
        _ => false,
    }
}

/// Whether an item the player holds may be used on the block at `at`: any
/// player but an adventure one; theirs only if it lists the block in
/// `can_place_on`.
pub fn may_use_on(
    ctx: &ServerContext,
    actor: &Actor,
    held: Option<&ItemStack>,
    at: Position,
) -> bool {
    actor.game_mode != ADVENTURE || stack_lists(ctx, held, "minecraft:can_place_on", at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interact::use_item_on;
    use crate::testkit::{block, flat, join, set, stack, wear};
    use mc_nbt::{Compound, Tag};
    use mc_protocol::packets::play::dig;

    fn world() -> std::sync::Arc<ServerContext> {
        flat().expect("game data")
    }

    fn spawn_xz(ctx: &ServerContext) -> (i32, i32) {
        let s = lock(&ctx.spawn).pos;
        (s.0, s.2)
    }

    fn op(ctx: &ServerContext, name: &str) {
        lock(&ctx.lists).ops.push(crate::lists::Op {
            uuid: None,
            name: name.into(),
            level: 4,
            bypasses_player_limit: false,
        });
    }

    fn actor(ctx: &ServerContext, uuid: u128) -> Actor {
        let p = lock(&ctx.players);
        let p = p.get(&uuid).unwrap();
        Actor {
            uuid,
            entity_id: p.entity_id,
            game_mode: p.game_mode,
            pose: p.pose.unwrap(),
            sneaking: false,
        }
    }

    fn listing(ctx: &ServerContext, item: &str, component: &str, block: &str) -> ItemStack {
        let mut s = stack(ctx, item, &[]);
        let mut c = Compound::new();
        c.insert("blocks", Tag::String(block.into()));
        s.components.set(component, Tag::Compound(c));
        s
    }

    #[test]
    fn spawn_protection_covers_a_square_around_the_spawn_for_non_operators() {
        let ctx = world();
        let (sx, sz) = spawn_xz(&ctx);
        let pos = [f64::from(sx) + 0.5, -60.0, f64::from(sz) + 0.5];
        join(&ctx, 1, 70, pos, 0);
        join(&ctx, 2, 71, pos, 0);
        let at = |dx: i32, dz: i32| Position::new(sx + dx, -60, sz + dz);
        // A world with no operators is not protected.
        assert!(!under_spawn_protection(&ctx, 1, at(0, 0)));
        op(&ctx, "P71");
        assert!(under_spawn_protection(&ctx, 1, at(0, 0)));
        assert!(under_spawn_protection(&ctx, 1, at(16, 16)));
        assert!(under_spawn_protection(&ctx, 1, at(-16, 3)));
        assert!(!under_spawn_protection(&ctx, 1, at(17, 0)));
        assert!(!under_spawn_protection(&ctx, 1, at(0, -17)));
        // Operators build where they like.
        assert!(!under_spawn_protection(&ctx, 2, at(0, 0)));
    }

    #[test]
    fn blocks_beyond_the_world_border_are_out_of_reach() {
        let ctx = world();
        join(&ctx, 1, 70, [0.5, -60.0, 0.5], 0);
        {
            let mut border = lock(&ctx.border);
            border.center_x = 0.0;
            border.center_z = 0.0;
            border.size = 10.0;
            border.lerp_target = 10.0;
        }
        assert!(may_interact(&ctx, 1, Position::new(-5, -60, 4)));
        assert!(!may_interact(&ctx, 1, Position::new(5, -60, 0)));
        assert!(!may_interact(&ctx, 1, Position::new(0, -60, -6)));
    }

    #[test]
    fn a_player_not_allowed_here_cannot_break_in_the_protected_square() {
        let ctx = world();
        let (sx, sz) = spawn_xz(&ctx);
        let pos = [f64::from(sx) + 0.5, -60.0, f64::from(sz) + 0.5];
        join(&ctx, 1, 70, pos, 1);
        join(&ctx, 2, 71, pos, 1);
        op(&ctx, "P71");
        let torch = [sx + 2, -60, sz];
        set(&ctx, torch, "minecraft:torch");
        let at = Position::new(torch[0], torch[1], torch[2]);
        let out = crate::interact::dig(&ctx, actor(&ctx, 1), dig::START, at, 1, 1);
        assert_eq!(block(&ctx, torch), "minecraft:torch");
        assert!(out.events.iter().any(|e| matches!(
            e,
            Event::Message {
                uuid: 1,
                overlay: true,
                ..
            }
        )));
        crate::interact::dig(&ctx, actor(&ctx, 2), dig::START, at, 1, 1);
        assert_eq!(block(&ctx, torch), "minecraft:air");
    }

    #[test]
    fn adventure_players_break_only_what_the_held_item_lists() {
        let ctx = world();
        join(&ctx, 3, 72, [0.5, -60.0, 0.5], 2);
        let torch = [2, -60, 0];
        let at = Position::new(2, -60, 0);
        set(&ctx, torch, "minecraft:torch");
        // Bare hands, or a list without the torch, break nothing.
        crate::interact::dig(&ctx, actor(&ctx, 3), dig::START, at, 1, 1);
        assert_eq!(block(&ctx, torch), "minecraft:torch");
        let pick = |block: &str| listing(&ctx, "stone_pickaxe", "can_break", block);
        wear(&ctx, 3, 36, pick("minecraft:stone"));
        crate::interact::dig(&ctx, actor(&ctx, 3), dig::START, at, 1, 2);
        assert_eq!(block(&ctx, torch), "minecraft:torch");
        wear(&ctx, 3, 36, pick("minecraft:torch"));
        crate::interact::dig(&ctx, actor(&ctx, 3), dig::START, at, 1, 3);
        assert_eq!(block(&ctx, torch), "minecraft:air");
    }

    #[test]
    fn adventure_items_act_only_on_the_blocks_they_list_to_place_on() {
        let ctx = world();
        join(&ctx, 3, 72, [0.5, -60.0, 0.5], 2);
        set(&ctx, [2, -60, 0], "minecraft:stone");
        let on = Position::new(2, -60, 0);
        let place = |item: ItemStack, seq: i32| {
            wear(&ctx, 3, 36, item);
            use_item_on(&ctx, actor(&ctx, 3), 0, on, 1, [0.5, 1.0, 0.5], seq);
        };
        place(stack(&ctx, "dirt", &[]), 1);
        assert_eq!(block(&ctx, [2, -59, 0]), "minecraft:air");
        place(
            listing(&ctx, "dirt", "can_place_on", "minecraft:oak_planks"),
            2,
        );
        assert_eq!(block(&ctx, [2, -59, 0]), "minecraft:air");
        place(listing(&ctx, "dirt", "can_place_on", "minecraft:stone"), 3);
        assert_eq!(block(&ctx, [2, -59, 0]), "minecraft:dirt");
    }

    #[test]
    fn using_an_item_in_the_protected_square_changes_nothing() {
        let ctx = world();
        let (sx, sz) = spawn_xz(&ctx);
        let pos = [f64::from(sx) + 0.5, -60.0, f64::from(sz) + 0.5];
        join(&ctx, 1, 70, pos, 0);
        join(&ctx, 2, 71, pos, 0);
        op(&ctx, "P71");
        let base = [sx + 2, -60, sz];
        set(&ctx, base, "minecraft:stone");
        let on = Position::new(base[0], base[1], base[2]);
        for (uuid, seq, placed) in [(1, 1, "minecraft:air"), (2, 2, "minecraft:dirt")] {
            wear(&ctx, uuid, 36, stack(&ctx, "dirt", &[]));
            let out = use_item_on(&ctx, actor(&ctx, uuid), 0, on, 1, [0.5, 1.0, 0.5], seq);
            assert_eq!(block(&ctx, [base[0], -59, base[2]]), placed);
            let told = out
                .events
                .iter()
                .any(|e| matches!(e, Event::Message { overlay: true, .. }));
            assert_eq!(told, uuid == 1);
            set(&ctx, [base[0], -59, base[2]], "minecraft:air");
        }
    }
}
