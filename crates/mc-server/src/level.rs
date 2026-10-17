//! The world as block logic (`mc_tick`) sees it, collecting what changed so
//! it can be sent to players.

use std::collections::HashMap;

use mc_protocol::Position;
use mc_tick::{BlockPos, Level};

use crate::context::ServerContext;

pub fn to_pos(p: Position) -> BlockPos {
    BlockPos::new(p.x, p.y, p.z)
}

pub fn to_position(p: BlockPos) -> Position {
    Position::new(p.x, p.y, p.z)
}

pub struct WorldLevel<'a> {
    ctx: &'a ServerContext,
    /// Changed blocks in the order they first changed, with their last state.
    changes: Vec<(Position, u32)>,
    index: HashMap<BlockPos, usize>,
}

impl<'a> WorldLevel<'a> {
    pub fn new(ctx: &'a ServerContext) -> Self {
        Self {
            ctx,
            changes: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn logic(&self) -> mc_tick::Ctx<'a> {
        mc_tick::Ctx {
            shapes: &self.ctx.shapes,
            facts: &self.ctx.rules,
        }
    }

    /// Sets a block with all updates (as a player or `/setblock` does).
    pub fn set_block(&mut self, p: Position, state: u32) -> bool {
        let logic = self.logic();
        mc_tick::set_block(self, logic, to_pos(p), state, mc_tick::Flags::ALL)
    }

    /// What changed, each block once with its final state.
    pub fn into_changes(self) -> Vec<(Position, u32)> {
        self.changes
    }
}

impl Level for WorldLevel<'_> {
    fn block(&self, p: BlockPos) -> u32 {
        self.ctx
            .world
            .block(to_position(p), &self.ctx.data.blocks)
            .unwrap_or(self.ctx.rules.air)
    }

    fn set(&mut self, p: BlockPos, state: u32) -> bool {
        let ctx = self.ctx;
        if ctx
            .world
            .set_block(to_position(p), state, &ctx.data.blocks, &ctx.rules)
            .is_none()
        {
            return false;
        }
        match self.index.get(&p) {
            Some(&i) => self.changes[i].1 = state,
            None => {
                self.index.insert(p, self.changes.len());
                self.changes.push((to_position(p), state));
            }
        }
        true
    }

    fn game_time(&self) -> i64 {
        self.ctx.time.lock().expect("time lock").age
    }

    fn schedule_fluid_tick(&mut self, p: BlockPos, fluid: u32, delay: i64) {
        let at = self.game_time() + delay;
        let mut ticks = self.ctx.ticks.lock().expect("ticks lock");
        ticks.fluids.schedule(p, fluid, at, 0);
    }

    fn schedule_block_tick(&mut self, p: BlockPos, block: u32, delay: i64, priority: i32) {
        let at = self.game_time() + delay;
        let mut ticks = self.ctx.ticks.lock().expect("ticks lock");
        ticks.blocks.schedule(p, block, at, priority);
    }
}
