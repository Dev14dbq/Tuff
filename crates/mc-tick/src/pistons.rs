//! Pistons and sticky pistons (minecraft.wiki: "Piston", "Slime Block",
//! "Honey Block"), checked on the parity bench.
//!
//! A piston is powered from any side but its front, or through the block
//! above it (quasi-connectivity). A change of power queues a block event;
//! block events run after the tick's block and fluid ticks. Extending moves
//! the blocks in front (at most 12; slime and honey blocks drag their
//! neighbours along, but not each other), breaks the ones that break when
//! pushed, and fails at an immovable one. Moved blocks and the head become
//! moving pistons for two ticks (clients are not sent those: they play the
//! block event themselves), then turn into what they carry. A sticky piston
//! pulls the block in front of its head back when it retracts, unless that
//! block only goes one way. How each state reacts to being pushed is
//! observed from vanilla (`observe push`).

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::redstone::power_from;
use crate::updates::{Ctx, Flags, set_block};

/// Most blocks one piston moves.
const PUSH_LIMIT: usize = 12;

/// A block being moved by a piston (vanilla's moving piston block entity).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moving {
    /// What it becomes when it stops.
    pub state: u32,
    pub facing: Dir,
    pub extending: bool,
    /// The piston's own head (or base, when retracting).
    pub source: bool,
    /// 0 to 1, half a block per tick.
    pub progress: f32,
}

/// How a block reacts to a piston.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    Normal,
    /// Pushed, never pulled (glazed terracotta).
    PushOnly,
    Destroy,
    Block,
}

/// Block event actions.
pub const EXTEND: u8 = 0;
pub const RETRACT: u8 = 1;
/// Retract without pulling (the pulse was too short).
pub const DROP: u8 = 2;

fn is_piston(ctx: Ctx, state: u32) -> bool {
    matches!(
        ctx.shapes.block_name(state),
        Some("minecraft:piston" | "minecraft:sticky_piston")
    )
}

fn is_sticky_piston(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.block_name(state) == Some("minecraft:sticky_piston")
}

fn facing(ctx: Ctx, state: u32) -> Option<Dir> {
    ctx.shapes
        .property(state, "facing")
        .and_then(Dir::from_name)
}

fn extended(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.property(state, "extended") == Some("true")
}

/// Power reaching the piston at `p`: from every side but its front, and
/// from around the block above it.
pub fn powered(level: &dyn Level, ctx: Ctx, p: BlockPos, front: Dir) -> bool {
    if Dir::ALL
        .into_iter()
        .any(|d| d != front && power_from(level, ctx, p, d, true) > 0)
    {
        return true;
    }
    let above = p.offset(Dir::Up);
    Dir::ALL
        .into_iter()
        .any(|d| d != Dir::Down && power_from(level, ctx, above, d, true) > 0)
}

fn reaction(level: &dyn Level, ctx: Ctx, p: BlockPos, dir: Dir) -> Push {
    let state = level.block(p);
    if !level.in_world(p) || !level.in_world(p.offset(dir)) {
        return Push::Block;
    }
    if is_piston(ctx, state) {
        return if extended(ctx, state) {
            Push::Block
        } else {
            Push::Normal
        };
    }
    ctx.shapes.push_reaction(state)
}

fn is_sticky(ctx: Ctx, state: u32) -> bool {
    matches!(
        ctx.shapes.block_name(state),
        Some("minecraft:slime_block" | "minecraft:honey_block")
    )
}

/// Slime and honey stick to anything but each other.
fn sticks(ctx: Ctx, a: u32, b: u32) -> bool {
    let (na, nb) = (ctx.shapes.block_name(a), ctx.shapes.block_name(b));
    !(matches!(
        (na, nb),
        (Some("minecraft:honey_block"), Some("minecraft:slime_block"))
            | (Some("minecraft:slime_block"), Some("minecraft:honey_block"))
    )) && (is_sticky(ctx, a) || is_sticky(ctx, b))
}

/// The blocks one piston move takes along.
struct Structure {
    piston: BlockPos,
    dir: Dir,
    push: Vec<BlockPos>,
    destroy: Vec<BlockPos>,
}

impl Structure {
    /// What a piston at `piston` facing `facing` moves (`extending`, or a
    /// sticky one pulling).
    fn resolve(
        level: &dyn Level,
        ctx: Ctx,
        piston: BlockPos,
        facing: Dir,
        extending: bool,
    ) -> Option<Structure> {
        let dir = if extending { facing } else { facing.opposite() };
        let start = if extending {
            piston.offset(facing)
        } else {
            piston.offset(facing).offset(facing)
        };
        let mut s = Structure {
            piston,
            dir,
            push: Vec::new(),
            destroy: Vec::new(),
        };
        let state = level.block(start);
        if ctx.shapes.is_air(state) {
            return Some(s);
        }
        match reaction(level, ctx, start, dir) {
            Push::Destroy if extending => {
                s.destroy.push(start);
                return Some(s);
            }
            Push::Normal => {}
            Push::PushOnly if extending => {}
            _ => return None,
        }
        if !s.line(level, ctx, start, dir) {
            return None;
        }
        let mut i = 0;
        while i < s.push.len() {
            let p = s.push[i];
            if is_sticky(ctx, level.block(p)) && !s.branches(level, ctx, p) {
                return None;
            }
            i += 1;
        }
        Some(s)
    }

    /// Adds the line of blocks from `origin` on in the move direction, with
    /// what sticks to it from behind. `false` if it cannot move.
    fn line(&mut self, level: &dyn Level, ctx: Ctx, origin: BlockPos, dir: Dir) -> bool {
        let state = level.block(origin);
        if ctx.shapes.is_air(state)
            || origin == self.piston
            || self.push.contains(&origin)
            || reaction(level, ctx, origin, dir) == Push::Destroy
        {
            return true;
        }
        if reaction(level, ctx, origin, dir) == Push::Block {
            return false;
        }
        // Blocks stuck behind it come along first.
        let mut behind = vec![origin];
        let mut cur = origin;
        let mut cur_state = state;
        while is_sticky(ctx, cur_state) {
            let back = cur.offset(dir.opposite());
            let back_state = level.block(back);
            if ctx.shapes.is_air(back_state)
                || !sticks(ctx, cur_state, back_state)
                || back == self.piston
                || !matches!(reaction(level, ctx, back, dir), Push::Normal)
            {
                break;
            }
            behind.push(back);
            if behind.len() + self.push.len() > PUSH_LIMIT {
                return false;
            }
            cur = back;
            cur_state = back_state;
        }
        if behind.len() + self.push.len() > PUSH_LIMIT {
            return false;
        }
        for p in behind.into_iter().rev() {
            self.push.push(p);
        }
        // Then everything in front of it.
        let mut next = origin.offset(dir);
        loop {
            let s = level.block(next);
            if ctx.shapes.is_air(s) {
                return true;
            }
            if next == self.piston {
                return false;
            }
            if self.push.contains(&next) {
                return true;
            }
            match reaction(level, ctx, next, dir) {
                Push::Block => return false,
                Push::Destroy => {
                    self.destroy.push(next);
                    return true;
                }
                _ => {}
            }
            if self.push.len() >= PUSH_LIMIT {
                return false;
            }
            self.push.push(next);
            next = next.offset(dir);
        }
    }

    /// Blocks beside a slime or honey block at `p` come too.
    fn branches(&mut self, level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
        let state = level.block(p);
        for d in Dir::ALL {
            if d.axis() == self.dir.axis() {
                continue;
            }
            let n = p.offset(d);
            let ns = level.block(n);
            if sticks(ctx, state, ns)
                && !ctx.shapes.is_air(ns)
                && matches!(reaction(level, ctx, n, self.dir), Push::Normal)
                && !self.line(level, ctx, n, self.dir)
            {
                return false;
            }
        }
        true
    }
}

/// A neighbour of the piston at `p` changed (or it was placed): queue a
/// move if its power no longer matches.
pub fn check(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if !is_piston(ctx, state) {
        return;
    }
    let Some(front) = facing(ctx, state) else {
        return;
    };
    let should = powered(level, ctx, p, front);
    let kind = ctx.shapes.block_index(state);
    if should && !extended(ctx, state) {
        if Structure::resolve(level, ctx, p, front, true).is_some() {
            level.block_event(p, kind, EXTEND, front.index() as u8);
        }
    } else if !should && extended(ctx, state) {
        // A head still on its way out: the block in front is dropped.
        let ahead = p.offset(front).offset(front);
        let early = level
            .moving(ahead)
            .is_some_and(|m| m.extending && m.facing == front && m.progress < 0.5);
        let action = if early { DROP } else { RETRACT };
        level.block_event(p, kind, action, front.index() as u8);
    }
}

/// Runs a queued block event of the piston at `p`. Returns whether it did
/// something (then clients are told, to play it).
pub fn block_event(level: &mut dyn Level, ctx: Ctx, p: BlockPos, action: u8, param: u8) -> bool {
    let state = level.block(p);
    if !is_piston(ctx, state) {
        return false;
    }
    let Some(front) = Dir::ALL.get(usize::from(param)).copied() else {
        return false;
    };
    let should = powered(level, ctx, p, front);
    if should && (action == RETRACT || action == DROP) {
        // Powered again before it could retract: stays out.
        let out = ctx.shapes.with(state, "extended", "true");
        set_block(level, ctx, p, out, Flags::SHAPES_ONLY);
        return false;
    }
    if !should && action == EXTEND {
        return false;
    }
    if action == EXTEND {
        if !move_blocks(level, ctx, p, front, true) {
            return false;
        }
        let out = ctx.shapes.with(state, "extended", "true");
        set_block(level, ctx, p, out, Flags::ALL);
        return true;
    }
    // Retracting: a head still moving out stops where it is.
    let head_pos = p.offset(front);
    if let Some(m) = level.moving(head_pos)
        && m.extending
    {
        finish(level, ctx, head_pos);
    }
    let base = ctx.shapes.with(state, "extended", "false");
    let moving_piston = moving_piston_state(ctx, front, is_sticky_piston(ctx, state));
    level.set_hidden(p, moving_piston);
    level.add_moving(
        p,
        Moving {
            state: base,
            facing: front,
            extending: false,
            source: true,
            progress: 0.0,
        },
    );
    crate::redstone::update_neighbors_at(level, ctx, p);
    if is_sticky_piston(ctx, state) {
        let ahead = head_pos.offset(front);
        let ahead_state = level.block(ahead);
        let mut pulled = false;
        if let Some(m) = level.moving(ahead) {
            // Spitting: a block still being pushed out is left there.
            if m.facing == front && m.extending {
                finish(level, ctx, ahead);
                pulled = true;
            }
        }
        if !pulled {
            if action == RETRACT
                && !ctx.shapes.is_air(ahead_state)
                && reaction(level, ctx, ahead, front.opposite()) == Push::Normal
            {
                move_blocks(level, ctx, p, front, false);
            } else {
                set_block(level, ctx, head_pos, ctx.shapes.air(), Flags::ALL);
            }
        }
    } else {
        set_block(level, ctx, head_pos, ctx.shapes.air(), Flags::ALL);
    }
    true
}

fn moving_piston_state(ctx: Ctx, facing: Dir, sticky: bool) -> u32 {
    let base = ctx
        .shapes
        .default_state("minecraft:moving_piston")
        .unwrap_or(ctx.shapes.air());
    let s = ctx.shapes.with(base, "facing", facing.name());
    ctx.shapes
        .with(s, "type", if sticky { "sticky" } else { "normal" })
}

/// Moves the structure in front of the piston at `p` (pushing or pulling).
fn move_blocks(level: &mut dyn Level, ctx: Ctx, p: BlockPos, facing: Dir, extending: bool) -> bool {
    let head_pos = p.offset(facing);
    // Pulling: the head goes first (clients see it go with the event).
    if !extending && ctx.shapes.block_name(level.block(head_pos)) == Some("minecraft:piston_head") {
        level.set_hidden(head_pos, ctx.shapes.air());
    }
    let Some(s) = Structure::resolve(level, ctx, p, facing, extending) else {
        return false;
    };
    let dir = s.dir;
    let states: Vec<u32> = s.push.iter().map(|&q| level.block(q)).collect();
    // What breaks, breaks (and drops).
    for &q in s.destroy.iter().rev() {
        let old = level.block(q);
        level.destroyed(q, old);
        set_block(level, ctx, q, ctx.shapes.air(), Flags::SHAPES_ONLY);
    }
    let sticky = is_sticky_piston(ctx, level.block(p));
    let mut vacated: Vec<BlockPos> = s.push.clone();
    // Each block becomes a moving piston one step on.
    for (i, &q) in s.push.iter().enumerate().rev() {
        let dest = q.offset(dir);
        vacated.retain(|&v| v != dest);
        level.set_hidden(dest, moving_piston_state(ctx, facing, sticky));
        level.add_moving(
            dest,
            Moving {
                state: states[i],
                facing,
                extending,
                source: false,
                progress: 0.0,
            },
        );
    }
    if extending {
        let head = ctx
            .shapes
            .default_state("minecraft:piston_head")
            .unwrap_or(ctx.shapes.air());
        let head = ctx.shapes.with(head, "facing", facing.name());
        let head = ctx
            .shapes
            .with(head, "type", if sticky { "sticky" } else { "normal" });
        vacated.retain(|&v| v != head_pos);
        level.set_hidden(head_pos, moving_piston_state(ctx, facing, sticky));
        level.add_moving(
            head_pos,
            Moving {
                state: head,
                facing,
                extending: true,
                source: true,
                progress: 0.0,
            },
        );
    }
    for &v in &vacated {
        set_block(level, ctx, v, ctx.shapes.air(), Flags::SHAPES_ONLY);
    }
    for &q in s.destroy.iter().chain(&s.push) {
        crate::redstone::update_neighbors_at(level, ctx, q);
    }
    for &q in &s.push {
        crate::redstone::update_neighbors_at(level, ctx, q.offset(dir));
    }
    if extending {
        crate::redstone::update_neighbors_at(level, ctx, head_pos);
    }
    true
}

/// Turns the moving piston at `p` into what it carries.
fn finish(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let Some(m) = level.remove_moving(p) else {
        return;
    };
    if ctx.shapes.block_name(level.block(p)) != Some("minecraft:moving_piston") {
        return;
    }
    let mut state = crate::updates::placement_state(level, ctx, p, m.state);
    if ctx.shapes.is_air(state) {
        state = m.state;
    }
    // A retracting head comes to nothing.
    if m.source && !m.extending && !is_piston(ctx, m.state) {
        state = ctx.shapes.air();
    }
    set_block(level, ctx, p, state, Flags::ALL);
    crate::redstone::update_neighbors_at(level, ctx, p);
}

/// One tick of every moving block: half a block on; stopped ones become
/// what they carry.
pub fn tick(level: &mut dyn Level, ctx: Ctx) {
    for p in level.moving_positions() {
        let Some(m) = level.moving(p) else { continue };
        if m.progress >= 1.0 {
            finish(level, ctx, p);
        } else {
            let progress = (m.progress + 0.5).min(1.0);
            level.add_moving(p, Moving { progress, ..m });
        }
    }
}
