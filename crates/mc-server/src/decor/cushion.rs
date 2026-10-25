//! Cushions (26.3, minecraft.wiki "Cushion"): an entity lying in a block
//! cell, on top of what is in it when that is lower than a block (a slab,
//! snow) or on the block below. A player sits on one by using it. Any hit
//! breaks it and it drops itself; it breaks too when what holds it goes,
//! which is looked at every 100 ticks.

use mc_entity::extra::{Decor, Hung};
use mc_entity::{Entity, Kind};
use mc_protocol::Position;
use mc_protocol::components::DYES;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::hits::{self, Tick};
use crate::interact::{Actor, Outcome};
use crate::tick::lock;
use crate::vehicles::Act;

/// The color index of a `*_cushion` item, if it is one.
pub fn color_of_item(name: &str) -> Option<u8> {
    let color = name
        .trim_start_matches("minecraft:")
        .strip_suffix("_cushion")?;
    DYES.iter().position(|d| *d == color).map(|i| i as u8)
}

pub fn color_of(name: &str) -> u8 {
    DYES.iter().position(|d| *d == name).unwrap_or(0) as u8
}

pub fn color_name(color: u8) -> &'static str {
    DYES.get(usize::from(color)).copied().unwrap_or("white")
}

pub fn item_of(color: u8) -> String {
    format!("minecraft:{}_cushion", color_name(color))
}

/// The height a cushion in `cell` lies at: on what is in the cell when that
/// is lower than a block, else on the block below. `None` where it cannot
/// lie: in a full block or a liquid, or over nothing.
pub fn rest(ctx: &ServerContext, cell: [i32; 3]) -> Option<f64> {
    let state = hits::block_at(ctx, cell)?;
    let blocks = &ctx.data.blocks;
    if matches!(
        blocks.block_name(state),
        Some("minecraft:water" | "minecraft:lava")
    ) || blocks.property(state, "waterlogged") == Some("true")
    {
        return None;
    }
    let y = f64::from(cell[1]);
    if ctx.rules.blocks_motion(state) {
        // A full cube has no boxes in the table.
        let boxes = crate::collision::table(ctx).get(state as usize)?.as_ref()?;
        let top = boxes.iter().map(|b| b[4]).fold(0.0, f64::max);
        return (top < 1.0).then_some(y + top);
    }
    hits::stops(ctx, [cell[0], cell[1] - 1, cell[2]]).then_some(y)
}

/// A cushion lying in `cell`.
pub fn new(ctx: &ServerContext, cell: [i32; 3], y: f64, color: u8) -> Entity {
    super::entity(
        ctx,
        [f64::from(cell[0]) + 0.5, y, f64::from(cell[2]) + 0.5],
        Hung {
            decor: Decor::Cushion {
                color,
                sitter: None,
            },
            facing: 0,
            yaw: 0.0,
            anchor: cell,
        },
    )
}

fn taken(ctx: &ServerContext, cell: [i32; 3]) -> bool {
    lock(&ctx.entities).iter().any(|e| {
        !e.removed
            && matches!(&e.kind, Kind::Hung(h)
                if h.anchor == cell && matches!(h.decor, Decor::Cushion { .. }))
    })
}

/// A cushion item used on a block: it goes into the block clicked when
/// its top is clicked and it is lower than a block, else next to the face.
pub fn place(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    color: u8,
    clicked: Position,
    face: usize,
) -> bool {
    let inside = [clicked.x, clicked.y, clicked.z];
    let next = crate::blocks::offset(clicked, face);
    let next = [next.x, next.y, next.z];
    let cell = if face == 1
        && ctx
            .rules
            .blocks_motion(hits::block_at(ctx, inside).unwrap_or(0))
    {
        rest(ctx, inside).map_or(next, |_| inside)
    } else {
        next
    };
    let Some(y) = rest(ctx, cell) else {
        return false;
    };
    if taken(ctx, cell) {
        return false;
    }
    let e = new(ctx, cell, y, color);
    crate::vibrations::emit(
        ctx,
        "entity_place",
        e.pos,
        crate::vibrations::Source::player(ctx, actor.uuid),
        None,
    );
    super::finish(ctx, outcome, actor, hand, e, "entity.cushion.place")
}

fn passengers(ctx: &ServerContext, vehicle: i32, sitter: Option<i32>) {
    fx::emit(
        ctx,
        fx::Fx::Passengers(mc_protocol::packets::play::SetPassengers {
            vehicle,
            passengers: sitter.into_iter().collect(),
        }),
    );
}

/// A player uses a cushion: they sit on it if nobody does.
pub fn sit(ctx: &ServerContext, actor: Actor, e: &mut Entity) -> bool {
    let id = e.id;
    let pos = e.pos;
    let Kind::Hung(Hung {
        decor: Decor::Cushion { sitter, .. },
        ..
    }) = &mut e.kind
    else {
        return false;
    };
    if sitter.is_some() || actor.sneaking || actor.game_mode == 3 {
        return false;
    }
    *sitter = Some(actor.entity_id);
    passengers(ctx, id, *sitter);
    crate::criteria::started_riding(actor.uuid);
    fx::sound(
        ctx,
        "entity.cushion.sit",
        fx::category::BLOCKS,
        pos,
        1.0,
        1.0,
    );
    crate::vibrations::emit(
        ctx,
        "entity_mount",
        pos,
        crate::vibrations::Source::player(ctx, actor.uuid),
        None,
    );
    true
}

/// Whoever sits on it gets up.
pub fn get_up(ctx: &ServerContext, e: &mut Entity) {
    let id = e.id;
    let pos = e.pos;
    if let Kind::Hung(Hung {
        decor: Decor::Cushion { sitter, .. },
        ..
    }) = &mut e.kind
        && sitter.take().is_some()
    {
        passengers(ctx, id, None);
        fx::sound(
            ctx,
            "entity.cushion.get_up",
            fx::category::BLOCKS,
            pos,
            1.0,
            1.0,
        );
    }
}

/// A sitter's own word: sneaking gets them up, onto the cushion's top.
/// Whether they sat on a cushion.
pub fn act(ctx: &ServerContext, actor: &Actor, act: &Act) -> bool {
    let mut entities = lock(&ctx.entities);
    let Some(e) = entities.iter_mut().find(|e| {
        !e.removed
            && matches!(&e.kind, Kind::Hung(Hung { decor: Decor::Cushion { sitter, .. }, .. })
                if *sitter == Some(actor.entity_id))
    }) else {
        return false;
    };
    if matches!(act, Act::Dismount) {
        get_up(ctx, e);
        let pose = crate::context::Pose {
            x: e.pos[0],
            y: e.pos[1],
            z: e.pos[2],
            on_ground: true,
            ..actor.pose
        };
        drop(entities);
        let _ = ctx.events.send(Event::Teleport {
            uuid: actor.uuid,
            pose,
        });
    }
    true
}

/// Every second, a sitter who left is gone from it; every 100 ticks, a
/// cushion with nothing to lie on breaks.
pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Hung(h) = &e.kind else {
        return;
    };
    let Decor::Cushion { sitter, .. } = h.decor else {
        return;
    };
    let anchor = h.anchor;
    if e.age.is_multiple_of(20)
        && let Some(s) = sitter
        && !lock(&t.ctx.players).values().any(|p| p.entity_id == s)
    {
        get_up(t.ctx, e);
    }
    if e.age.is_multiple_of(100) && rest(t.ctx, anchor).is_none_or(|y| (y - e.pos[1]).abs() > 1e-3)
    {
        let mut fresh = Vec::new();
        super::break_hung(t.ctx, e, &mut fresh, true);
        t.out.spawn.extend(fresh);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::tests::flat;

    fn actor(mode: i32) -> Actor {
        Actor {
            uuid: 9,
            entity_id: 4,
            game_mode: mode,
            pose: crate::context::Pose {
                x: 0.0,
                y: -60.0,
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
            sneaking: false,
        }
    }

    fn set(ctx: &ServerContext, p: [i32; 3], state: u32) {
        ctx.world.set_block(
            Position::new(p[0], p[1], p[2]),
            state,
            &ctx.data.blocks,
            &ctx.rules,
        );
    }

    fn put(ctx: &ServerContext, clicked: [i32; 3]) -> bool {
        let mut outcome = Outcome::default();
        place(
            ctx,
            &mut outcome,
            actor(1),
            0,
            14,
            Position::new(clicked[0], clicked[1], clicked[2]),
            1,
        )
    }

    fn cushion_at(ctx: &ServerContext, cell: [i32; 3]) -> Option<Entity> {
        lock(&ctx.entities)
            .iter()
            .find(|e| {
                !e.removed
                    && matches!(&e.kind, Kind::Hung(h)
                        if h.anchor == cell && matches!(h.decor, Decor::Cushion { .. }))
            })
            .cloned()
    }

    #[test]
    fn a_cushion_lies_on_the_ground_or_in_a_slab_but_not_in_water() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        assert!(put(&ctx, [0, -61, 0]));
        let e = cushion_at(&ctx, [0, -60, 0]).expect("a cushion on the grass");
        assert_eq!(e.pos, [0.5, -60.0, 0.5]);
        assert!(matches!(&e.kind, Kind::Hung(h)
            if matches!(h.decor, Decor::Cushion { color: 14, sitter: None })));
        assert!(!put(&ctx, [0, -61, 0]), "two in one cell");
        // In a bottom slab, on its top half.
        let slab = ctx
            .data
            .blocks
            .default_state("minecraft:stone_slab")
            .unwrap();
        set(&ctx, [3, -60, 0], slab);
        assert!(put(&ctx, [3, -60, 0]));
        assert_eq!(cushion_at(&ctx, [3, -60, 0]).unwrap().pos[1], -59.5);
        // Not on water.
        let water = ctx.data.blocks.default_state("minecraft:water").unwrap();
        set(&ctx, [6, -60, 0], water);
        assert!(!put(&ctx, [6, -61, 0]));
    }

    #[test]
    fn a_player_sits_on_it_and_gets_up() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        assert!(put(&ctx, [0, -61, 0]));
        {
            let mut all = lock(&ctx.entities);
            let e = all.iter_mut().find(|e| !e.removed).unwrap();
            assert!(sit(&ctx, actor(0), e));
            assert!(!sit(&ctx, actor(0), e), "sat twice");
            assert!(matches!(&e.kind, Kind::Hung(h)
                if matches!(h.decor, Decor::Cushion { sitter: Some(4), .. })));
        }
        assert!(act(&ctx, &actor(0), &Act::Dismount));
        let e = cushion_at(&ctx, [0, -60, 0]).unwrap();
        assert!(matches!(&e.kind, Kind::Hung(h)
            if matches!(h.decor, Decor::Cushion { sitter: None, .. })));
        assert!(!act(&ctx, &actor(0), &Act::Dismount), "still sitting");
    }

    #[test]
    fn with_nothing_under_it_it_breaks_and_drops_itself() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        assert!(put(&ctx, [10, -61, 0]));
        let mut e = cushion_at(&ctx, [10, -60, 0]).unwrap();
        lock(&ctx.entities).clear();
        set(&ctx, [10, -61, 0], 0);
        let mut out = hits::Out::default();
        for _ in 0..100 {
            let mut t = Tick {
                ctx: &ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            super::super::tick(&mut t, &mut e);
        }
        assert!(e.removed);
        let red = ctx.items.id("minecraft:red_cushion").unwrap();
        assert!(
            out.spawn
                .iter()
                .any(|d| matches!(d.kind, Kind::Item { item, .. } if item == red))
        );
    }
}
