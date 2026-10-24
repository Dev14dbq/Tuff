//! Respawning at a bed or a respawn anchor (minecraft.wiki, "Bed",
//! "Respawn Anchor"): a safe spot beside the block is searched for in the
//! game's order, never in the block itself. A bed or anchor that is gone,
//! blocked in or (for an anchor) out of charges sends the player to the
//! world spawn and is forgotten.

use mc_protocol::Position;

use crate::context::{Pose, ServerContext};
use crate::fx;
use crate::players::Respawn;

/// Cells (x, z) of a bed's surroundings in the order a player is put
/// there: the ten blocks around the two halves, starting beside the head on
/// the side the player was facing when they set the spawn and going round
/// the bed away from the head's side (clockwise for the right, otherwise
/// counter-clockwise).
///
/// `head` is the head half and `facing` the way from foot to head.
pub fn bed_ring(head: (i32, i32), facing: (i32, i32), right: bool) -> Vec<(i32, i32)> {
    let (fx, fz) = facing;
    // The player's right when lying with the head ahead: a clockwise turn.
    let (rx, rz) = if right { (-fz, fx) } else { (fz, -fx) };
    let foot = (head.0 - fx, head.1 - fz);
    let at = |base: (i32, i32), side: i32, along: i32| {
        (
            base.0 + rx * side + fx * along,
            base.1 + rz * side + fz * along,
        )
    };
    vec![
        at(head, 1, 0),
        at(foot, 1, 0),
        at(foot, 1, -1),
        at(foot, 0, -1),
        at(foot, -1, -1),
        at(foot, -1, 0),
        at(head, -1, 0),
        at(head, -1, 1),
        at(head, 0, 1),
        at(head, 1, 1),
    ]
}

/// The cells (dx, dy, dz) round a respawn anchor in the order they are
/// tried: its own level and the one below first, the one above last, the
/// four sides before the corners.
pub fn anchor_cells() -> Vec<(i32, i32, i32)> {
    const SIDES: [(i32, i32); 8] = [
        (0, -1),
        (-1, 0),
        (0, 1),
        (1, 0),
        (-1, -1),
        (1, -1),
        (-1, 1),
        (1, 1),
    ];
    [0, -1, 1]
        .iter()
        .flat_map(|&dy| SIDES.iter().map(move |&(dx, dz)| (dx, dy, dz)))
        .collect()
}

fn direction(name: &str) -> Option<(i32, i32)> {
    Some(match name {
        "north" => (0, -1),
        "south" => (0, 1),
        "west" => (-1, 0),
        "east" => (1, 0),
        _ => return None,
    })
}

/// The yaw that looks from `from` towards `to` (block centres, x and z).
fn yaw_towards(from: (f64, f64), to: (f64, f64)) -> f32 {
    (-(to.0 - from.0)).atan2(to.1 - from.1).to_degrees() as f32
}

/// Whether a player can stand with their feet in the cell `p`: room for
/// the body, something to stand on and nothing that hurts.
fn standable(ctx: &ServerContext, p: Position) -> bool {
    let blocks = &ctx.data.blocks;
    let state = |p: Position| ctx.world.block(p, blocks);
    let free = |p: Position| {
        state(p).is_some_and(|s| {
            !ctx.rules.blocks_motion(s)
                && !blocks.is_fluid(s)
                && !matches!(
                    blocks.block_name(s),
                    Some(
                        "minecraft:fire"
                            | "minecraft:soul_fire"
                            | "minecraft:cactus"
                            | "minecraft:sweet_berry_bush"
                            | "minecraft:wither_rose"
                            | "minecraft:campfire"
                            | "minecraft:soul_campfire"
                    )
                )
        })
    };
    let floor = Position::new(p.x, p.y - 1, p.z);
    free(p)
        && free(Position::new(p.x, p.y + 1, p.z))
        && state(floor).is_some_and(|s| {
            ctx.rules.blocks_motion(s) && blocks.block_name(s) != Some("minecraft:magma_block")
        })
}

/// Where a player is put for the bed at `at` (either half) when the spawn
/// was set facing `yaw`, `None` if the bed is gone or every place round it
/// is taken.
pub fn bed_spot(ctx: &ServerContext, at: Position, yaw: f32) -> Option<Pose> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(at, blocks)?;
    if !blocks.block_name(state)?.ends_with("_bed") {
        return None;
    }
    let facing = direction(blocks.property(state, "facing")?)?;
    let head = if blocks.property(state, "part") == Some("head") {
        (at.x, at.z)
    } else {
        (at.x + facing.0, at.z + facing.1)
    };
    // Which side of the bed the player was on: the one they faced.
    let look = (
        -f64::from(yaw).to_radians().sin(),
        f64::from(yaw).to_radians().cos(),
    );
    let right_of_head = (-facing.1, facing.0);
    let right = look.0 * f64::from(right_of_head.0) + look.1 * f64::from(right_of_head.1) >= 0.0;
    let ring = bed_ring(head, facing, right);
    // The bed itself is at `at.y`; places may be a little lower.
    let mut cells: Vec<Position> = Vec::new();
    for (x, z) in ring {
        for dy in 0..=2 {
            cells.push(Position::new(x, at.y - dy, z));
        }
    }
    // Above the head, then above the foot.
    cells.push(Position::new(head.0, at.y + 1, head.1));
    cells.push(Position::new(
        head.0 - facing.0,
        at.y + 1,
        head.1 - facing.1,
    ));
    // One column is tried from its top down before the next.
    let cell = cells.into_iter().find(|&p| standable(ctx, p))?;
    let look_at = (f64::from(head.0) + 0.5, f64::from(head.1) + 0.5);
    let me = (f64::from(cell.x) + 0.5, f64::from(cell.z) + 0.5);
    Some(Pose {
        x: me.0,
        y: f64::from(cell.y),
        z: me.1,
        yaw: yaw_towards(me, look_at),
        pitch: 0.0,
        on_ground: true,
    })
}

/// Where a player is put for the respawn anchor at `at`: `None` if it is
/// gone, out of charges or walled in.
pub fn anchor_spot(ctx: &ServerContext, at: Position) -> Option<Pose> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(at, blocks)?;
    if blocks.block_name(state)? != "minecraft:respawn_anchor"
        || blocks.property(state, "charges") == Some("0")
    {
        return None;
    }
    let cell = anchor_cells()
        .into_iter()
        .map(|(dx, dy, dz)| Position::new(at.x + dx, at.y + dy, at.z + dz))
        .find(|&p| standable(ctx, p))?;
    let me = (f64::from(cell.x) + 0.5, f64::from(cell.z) + 0.5);
    Some(Pose {
        x: me.0,
        y: f64::from(cell.y),
        z: me.1,
        yaw: yaw_towards(me, (f64::from(at.x) + 0.5, f64::from(at.z) + 0.5)),
        pitch: 0.0,
        on_ground: true,
    })
}

/// A respawn anchor loses a charge for the player put beside it.
fn deplete(there: &ServerContext, at: Position) {
    let blocks = &there.data.blocks;
    let Some(state) = there.world.block(at, blocks) else {
        return;
    };
    let Some(charges) = blocks
        .property(state, "charges")
        .and_then(|c| c.parse::<i32>().ok())
    else {
        return;
    };
    let mut level = crate::level::WorldLevel::new(there);
    level.set_block(
        at,
        there
            .shapes
            .with(state, "charges", &(charges - 1).max(0).to_string()),
    );
    let changes = level.into_changes();
    let _ = there
        .events
        .send(crate::context::Event::BlocksChanged { by: 0, changes });
    fx::sound(
        there,
        "block.respawn_anchor.deplete",
        fx::category::BLOCKS,
        [
            f64::from(at.x) + 0.5,
            f64::from(at.y) + 0.5,
            f64::from(at.z) + 0.5,
        ],
        1.0,
        1.0,
    );
}

/// Where a player with the (not forced) respawn point `r` comes back, and
/// in which dimension; `None` if the bed or anchor will not have them. An
/// anchor pays a charge for a respawn.
pub fn find(ctx: &ServerContext, r: &Respawn) -> Option<(String, Pose)> {
    let there = if r.dimension == ctx.world.dimension {
        None
    } else {
        Some(ctx.in_dimension(&r.dimension)?)
    };
    let there = there.as_deref().unwrap_or(ctx);
    let at = Position::new(r.pos.0, r.pos.1, r.pos.2);
    let blocks = &there.data.blocks;
    let state = there.world.block(at, blocks)?;
    let pose = if blocks.block_name(state)? == "minecraft:respawn_anchor" {
        let pose = anchor_spot(there, at)?;
        deplete(there, at);
        pose
    } else {
        bed_spot(there, at, r.yaw)?
    };
    Some((r.dimension.clone(), pose))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::flat;

    #[test]
    fn the_ring_round_a_bed_has_ten_cells_none_of_them_the_bed() {
        // Head at (0, 0), the bed lying north to south, head to the north.
        let ring = bed_ring((0, 0), (0, -1), true);
        assert_eq!(ring.len(), 10);
        assert_eq!(ring[0], (1, 0), "beside the head, on the right");
        assert_eq!(ring[1], (1, 1), "then down the side");
        assert!(!ring.contains(&(0, 0)) && !ring.contains(&(0, 1)));
        let mut sorted = ring.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 10);
        // The left starts on the other side and goes the other way round.
        let left = bed_ring((0, 0), (0, -1), false);
        assert_eq!(left[0], (-1, 0));
        assert_eq!(left[1], (-1, 1));
        assert_eq!(left[9], (-1, -1));
    }

    #[test]
    fn an_anchor_is_tried_on_its_own_level_first_and_by_sides_before_corners() {
        let cells = anchor_cells();
        assert_eq!(cells.len(), 24);
        assert_eq!(cells[0], (0, 0, -1));
        assert!(cells[..8].iter().all(|c| c.1 == 0));
        assert!(cells[8..16].iter().all(|c| c.1 == -1));
        assert!(cells[16..].iter().all(|c| c.1 == 1));
        assert_eq!(cells[3], (1, 0, 0));
        assert_eq!(cells[4], (-1, 0, -1));
    }

    fn put(ctx: &ServerContext, p: Position, name: &str, props: &[(&str, &str)]) {
        let state = ctx.data.blocks.state_id(name, props).unwrap();
        crate::level::WorldLevel::new(ctx).set_block(p, state);
    }

    fn bed(ctx: &ServerContext, head: Position) {
        // Facing north: the foot is south of the head.
        let props =
            |part: &'static str| [("facing", "north"), ("occupied", "false"), ("part", part)];
        put(ctx, head, "minecraft:red_bed", &props("head"));
        put(
            ctx,
            Position::new(head.x, head.y, head.z + 1),
            "minecraft:red_bed",
            &props("foot"),
        );
    }

    #[test]
    fn a_player_is_put_beside_the_bed_looking_at_its_head() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let head = Position::new(20, -60, 20);
        bed(&ctx, head);
        // Set facing east (yaw -90): the player's right of the bed, east.
        let pose = bed_spot(&ctx, head, -90.0).expect("a free spot");
        assert_eq!((pose.x, pose.y, pose.z), (21.5, -60.0, 20.5));
        // The head is to the west and a bit north... straight west.
        assert!((pose.yaw.abs() - 90.0).abs() < 1.0, "{}", pose.yaw);
        // Asked from the foot half, the same bed.
        let foot = Position::new(20, -60, 21);
        let from_foot = bed_spot(&ctx, foot, -90.0).expect("the same bed");
        assert_eq!((from_foot.x, from_foot.z), (pose.x, pose.z));
        // Facing the other way puts them on the other side.
        let other = bed_spot(&ctx, head, 90.0).expect("a free spot");
        assert_eq!((other.x, other.z), (19.5, 20.5));
    }

    #[test]
    fn a_blocked_side_sends_them_round_the_bed() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let head = Position::new(30, -60, 30);
        bed(&ctx, head);
        put(&ctx, Position::new(31, -60, 30), "minecraft:stone", &[]);
        // Wall the place off a level up too: no room to stand.
        let pose = bed_spot(&ctx, head, -90.0).expect("a free spot");
        assert_eq!((pose.x, pose.z), (31.5, 31.5), "down the same side");
        // Both sides and every other cell taken: above the head.
        for x in 28..=32 {
            for z in 28..=33 {
                let p = Position::new(x, -60, z);
                let bed_cell = x == 30 && (z == 30 || z == 31);
                if !bed_cell {
                    put(&ctx, p, "minecraft:stone", &[]);
                }
            }
        }
        let up = bed_spot(&ctx, head, -90.0).expect("on top of the bed");
        assert_eq!((up.x, up.y, up.z), (30.5, -59.0, 30.5));
        // And with the head's top roofed over, on top of the foot.
        put(&ctx, Position::new(30, -59, 30), "minecraft:stone", &[]);
        let up = bed_spot(&ctx, head, -90.0).expect("on top of the foot");
        assert_eq!((up.x, up.y, up.z), (30.5, -59.0, 31.5));
        put(&ctx, Position::new(30, -59, 31), "minecraft:stone", &[]);
        assert!(bed_spot(&ctx, head, -90.0).is_none());
    }

    #[test]
    fn no_bed_no_place() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        assert!(bed_spot(&ctx, Position::new(40, -60, 40), 0.0).is_none());
    }

    #[test]
    fn an_anchor_takes_a_charge_per_respawn_and_then_refuses() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(50, -60, 50);
        put(&ctx, at, "minecraft:respawn_anchor", &[("charges", "2")]);
        let r = Respawn {
            dimension: ctx.world.dimension.clone(),
            pos: (at.x, at.y, at.z),
            yaw: 0.0,
            pitch: 0.0,
            forced: false,
        };
        let charges = || {
            let s = ctx.world.block(at, &ctx.data.blocks).unwrap();
            ctx.data.blocks.property(s, "charges").unwrap().to_owned()
        };
        let (_, pose) = find(&ctx, &r).expect("charged");
        // Not in the anchor, standing on the ground beside it.
        assert_eq!(pose.y, -60.0);
        assert_ne!((pose.x, pose.z), (50.5, 50.5));
        assert_eq!(charges(), "1");
        assert!(find(&ctx, &r).is_some());
        assert_eq!(charges(), "0");
        assert!(find(&ctx, &r).is_none());
        assert_eq!(charges(), "0");
    }
}
