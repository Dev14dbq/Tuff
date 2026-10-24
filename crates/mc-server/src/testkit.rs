//! What tests of players with gear share: a flat world, a player in it,
//! enchanted stacks.

use mc_protocol::packets::play::ItemStack;

use crate::context::{Pose, ServerContext};
use crate::tick::lock;

/// A superflat server context (surface at y = -60), `None` without the
/// game data.
pub fn flat() -> Option<std::sync::Arc<ServerContext>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let data = mc_data::McData::load(&dir).ok()?;
    ServerContext::new(crate::Config::new(dir), data).ok()
}

/// Puts a player (`uuid`, entity id `entity_id`) at `pos` in game mode
/// `mode`, with the chunks around loaded.
pub fn join(ctx: &ServerContext, uuid: u128, entity_id: i32, pos: [f64; 3], mode: i32) {
    for cx in -2..=2 {
        for cz in -2..=2 {
            let _ = ctx.world.block(
                mc_protocol::Position::new(cx * 16 + pos[0] as i32, 0, cz * 16 + pos[2] as i32),
                &ctx.data.blocks,
            );
        }
    }
    lock(&ctx.players).insert(
        uuid,
        crate::context::OnlinePlayer {
            profile: mc_protocol::packets::GameProfile {
                uuid,
                name: format!("P{entity_id}"),
                properties: Vec::new(),
            },
            game_mode: mode,
            entity_id,
            skin_parts: 0,
            pose: Some(Pose {
                x: pos[0],
                y: pos[1],
                z: pos[2],
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            }),
            dimension: ctx.world.dimension.clone(),
            main_hand: None,
            flying: false,
            address: None,
            extra: Default::default(),
        },
    );
    lock(&ctx.vitals).insert(uuid, crate::vitals::Vitals::default());
    lock(&ctx.inventories).insert(uuid, crate::interact::Hands::default());
}

/// Moves a player.
pub fn place(ctx: &ServerContext, uuid: u128, pos: [f64; 3], on_ground: bool) {
    if let Some(p) = lock(&ctx.players).get_mut(&uuid)
        && let Some(pose) = &mut p.pose
    {
        (pose.x, pose.y, pose.z, pose.on_ground) = (pos[0], pos[1], pos[2], on_ground);
    }
}

/// One of an item, enchanted.
pub fn stack(ctx: &ServerContext, item: &str, enchants: &[(&str, i32)]) -> ItemStack {
    let id = ctx.items.id(&format!("minecraft:{item}")).expect("item");
    let mut s = ItemStack::new(id, 1);
    let list: Vec<(String, i32)> = enchants
        .iter()
        .map(|(n, l)| (format!("minecraft:{n}"), *l))
        .collect();
    crate::enchants::set_levels(&mut s, "minecraft:enchantments", &list);
    s
}

/// Puts a stack in an inventory slot (5..8 armor, 36.. hotbar, 45 off hand).
pub fn wear(ctx: &ServerContext, uuid: u128, slot: usize, stack: ItemStack) {
    if let Some(h) = lock(&ctx.inventories).get_mut(&uuid) {
        h.slots[slot] = Some(stack);
    }
}

/// The block name at a position.
pub fn block(ctx: &ServerContext, p: [i32; 3]) -> String {
    ctx.world
        .block(
            mc_protocol::Position::new(p[0], p[1], p[2]),
            &ctx.data.blocks,
        )
        .and_then(|s| ctx.data.blocks.block_name(s))
        .unwrap_or("")
        .to_owned()
}

/// Sets a block with all updates.
pub fn set(ctx: &ServerContext, p: [i32; 3], name: &str) {
    let state = ctx.data.blocks.default_state(name).expect("block");
    let mut level = crate::level::WorldLevel::new(ctx);
    level.set_block(mc_protocol::Position::new(p[0], p[1], p[2]), state);
    let _ = level.into_changes();
}
