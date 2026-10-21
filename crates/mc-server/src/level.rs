//! The world as block logic (`mc_tick`) sees it, collecting what changed so
//! it can be sent to players.

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
    /// Game time, read once: it does not move while block logic runs.
    time: i64,
    /// Changed blocks in the order they first changed, with their last state.
    changes: Vec<(Position, u32)>,
    index: mc_tick::fast::FastMap<BlockPos, usize>,
    /// Entities created by block logic, for the tick to add.
    pub spawned: Vec<mc_entity::Entity>,
    /// Chunks whose light changed.
    lit: Vec<(i32, i32)>,
    /// Changed blocks whose light is not fixed yet: light is worked out for
    /// all of them at once when the level is done (as vanilla's light
    /// engine also runs after the changes, not during them).
    unlit: Vec<(Position, u32)>,
    /// While entities move: their boxes as they are so far this tick.
    pub entity_boxes: Option<Vec<mc_entity::Aabb>>,
    /// Minecarts and where they are so far this tick (detector rails look).
    pub minecarts: Vec<(i32, mc_entity::Aabb)>,
}

impl<'a> WorldLevel<'a> {
    pub fn new(ctx: &'a ServerContext) -> Self {
        Self {
            ctx,
            time: ctx.time.lock().expect("time lock").age,
            changes: Vec::new(),
            index: Default::default(),
            spawned: Vec::new(),
            lit: Vec::new(),
            unlit: Vec::new(),
            entity_boxes: None,
            minecarts: Vec::new(),
        }
    }

    pub fn context(&self) -> &'a ServerContext {
        self.ctx
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

    /// Fixes the light around every block changed so far.
    pub fn light(&mut self) {
        if self.unlit.is_empty() {
            return;
        }
        let mut unlit = std::mem::take(&mut self.unlit);
        // Each block once, with the state it had first: the light stored is
        // still the light of that state.
        unlit.sort_by_key(|(p, _)| (p.x, p.z, p.y));
        unlit.dedup_by_key(|(p, _)| *p);
        for chunk in self.ctx.world.relight_many(&unlit) {
            if !self.lit.contains(&chunk) {
                self.lit.push(chunk);
            }
        }
    }

    fn send_light(&mut self) {
        self.light();
        if !self.lit.is_empty() {
            let lit = std::mem::take(&mut self.lit);
            let _ = self
                .ctx
                .events
                .send(crate::context::Event::LightChanged(lit));
        }
    }

    /// Entities block logic created that nobody took with `finish`: they
    /// join the world now.
    fn release_spawned(&mut self) {
        if self.spawned.is_empty() {
            return;
        }
        let mut entities = self.ctx.entities.lock().expect("entities lock");
        for entity in self.spawned.drain(..) {
            let _ = self
                .ctx
                .events
                .send(crate::context::Event::EntitySpawned(entity.clone()));
            entities.push(entity);
        }
    }

    /// What changed, each block once with its final state.
    pub fn into_changes(mut self) -> Vec<(Position, u32)> {
        self.send_light();
        std::mem::take(&mut self.changes)
    }

    /// What changed and the entities block logic created.
    pub fn finish(mut self) -> (Vec<(Position, u32)>, Vec<mc_entity::Entity>) {
        self.send_light();
        (
            std::mem::take(&mut self.changes),
            std::mem::take(&mut self.spawned),
        )
    }
}

impl Drop for WorldLevel<'_> {
    fn drop(&mut self) {
        self.send_light();
        self.release_spawned();
    }
}

impl Level for WorldLevel<'_> {
    fn allows_nether_portal(&self) -> bool {
        self.ctx.dim < 2
    }

    fn fast_lava(&self) -> bool {
        self.ctx.dim == 1
    }

    fn block(&self, p: BlockPos) -> u32 {
        self.ctx
            .world
            .block(to_position(p), &self.ctx.data.blocks)
            .unwrap_or(self.ctx.rules.air)
    }

    fn set(&mut self, p: BlockPos, state: u32) -> bool {
        let ctx = self.ctx;
        let Some(old) = ctx
            .world
            .set_block(to_position(p), state, &ctx.data.blocks, &ctx.rules)
        else {
            return false;
        };
        if ctx.data.blocks.block_name(state) == Some("minecraft:hopper") {
            crate::hoppers::register(ctx, to_position(p));
        }
        if ctx.data.blocks.block_name(state) == Some("minecraft:beacon") {
            crate::beacons::placed(ctx, to_position(p));
        }
        if !ctx.world.same_light(old, state) {
            self.unlit.push((to_position(p), old));
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
        self.time
    }

    fn schedule_fluid_tick(&mut self, p: BlockPos, fluid: u32, delay: i64) {
        let at = self.game_time() + delay;
        let mut ticks = self.ctx.ticks.lock().expect("ticks lock");
        ticks.fluids.schedule(p, fluid, at, 0);
    }

    fn in_world(&self, p: BlockPos) -> bool {
        let world = &self.ctx.world;
        p.y >= world.min_y && p.y < world.min_y + world.height as i32
    }

    fn set_hidden(&mut self, p: BlockPos, state: u32) -> bool {
        let ctx = self.ctx;
        let Some(old) = ctx
            .world
            .set_block(to_position(p), state, &ctx.data.blocks, &ctx.rules)
        else {
            return false;
        };
        if !ctx.world.same_light(old, state) {
            self.unlit.push((to_position(p), old));
        }
        // Already changed this tick: clients get it as it ends up.
        if let Some(&i) = self.index.get(&p) {
            self.changes[i].1 = state;
        }
        true
    }

    fn block_event(&mut self, p: BlockPos, block: u32, action: u8, param: u8) {
        let mut events = self.ctx.block_events.lock().expect("block events lock");
        if !events.contains(&(p, block, action, param)) {
            events.push((p, block, action, param));
        }
    }

    fn add_moving(&mut self, p: BlockPos, moving: mc_tick::pistons::Moving) {
        let mut all = self.ctx.moving.lock().expect("moving lock");
        match all.iter_mut().find(|(q, _)| *q == p) {
            Some(entry) => entry.1 = moving,
            None => all.push((p, moving)),
        }
    }

    fn moving(&self, p: BlockPos) -> Option<mc_tick::pistons::Moving> {
        let all = self.ctx.moving.lock().expect("moving lock");
        all.iter().find(|(q, _)| *q == p).map(|(_, m)| *m)
    }

    fn remove_moving(&mut self, p: BlockPos) -> Option<mc_tick::pistons::Moving> {
        let mut all = self.ctx.moving.lock().expect("moving lock");
        let i = all.iter().position(|(q, _)| *q == p)?;
        Some(all.remove(i).1)
    }

    fn moving_positions(&self) -> Vec<BlockPos> {
        let all = self.ctx.moving.lock().expect("moving lock");
        all.iter().map(|(p, _)| *p).collect()
    }

    fn brightness(&self, p: BlockPos) -> u8 {
        self.ctx.world.brightness(to_position(p))
    }

    fn block_light(&self, p: BlockPos) -> u8 {
        self.ctx.world.block_light(to_position(p))
    }

    fn light_block(&self, state: u32) -> u8 {
        self.ctx.world.light_block(state)
    }

    fn raining_at(&self, p: BlockPos) -> bool {
        self.ctx.weather.lock().expect("weather lock").raining
            && self.ctx.world.sky_light(to_position(p)) == 15
    }

    fn fire_spreads_at(&self, p: BlockPos) -> bool {
        let radius = self
            .ctx
            .game_rules
            .lock()
            .expect("game rules lock")
            .int("minecraft:fire_spread_radius_around_player");
        if radius < 0 {
            return true;
        }
        let r = f64::from(radius);
        self.ctx
            .players
            .lock()
            .expect("players lock")
            .values()
            .filter(|p| p.dimension == self.ctx.world.dimension)
            .filter_map(|player| player.pose)
            .any(|pose| {
                let (dx, dy, dz) = (
                    pose.x - f64::from(p.x) - 0.5,
                    pose.y - f64::from(p.y) - 0.5,
                    pose.z - f64::from(p.z) - 0.5,
                );
                dx * dx + dy * dy + dz * dz <= r * r
            })
    }

    fn destroyed(&mut self, p: BlockPos, state: u32) {
        let ctx = self.ctx;
        let blocks = &ctx.data.blocks;
        let at = to_position(p);
        if let Some(name) = blocks.block_name(state) {
            let props = |k: &str| blocks.property(state, k).map(str::to_owned);
            for drop in ctx.loot.drops(name, &props) {
                crate::tick::drop_item(ctx, at, drop);
            }
        }
        let _ = ctx.events.send(crate::context::Event::BlockBroken {
            by: 0,
            position: at,
            state,
        });
    }

    fn container_signal(&self, p: BlockPos) -> Option<u8> {
        crate::containers::signal(self.ctx, to_position(p))
    }

    fn comparator_output(&self, p: BlockPos) -> u8 {
        let be = self
            .ctx
            .world
            .block_entity(to_position(p), &self.ctx.data.blocks);
        match be.and_then(|be| be.data.get("OutputSignal").cloned()) {
            Some(mc_nbt::Tag::Int(v)) => v.clamp(0, 15) as u8,
            _ => 0,
        }
    }

    fn set_comparator_output(&mut self, p: BlockPos, signal: u8) {
        let blocks = &self.ctx.data.blocks;
        let at = to_position(p);
        let mut be = self
            .ctx
            .world
            .block_entity(at, blocks)
            .unwrap_or(mc_world::BlockEntity {
                x: p.x,
                y: p.y,
                z: p.z,
                id: "minecraft:comparator".into(),
                data: mc_nbt::Compound::new(),
            });
        be.data
            .insert("OutputSignal", mc_nbt::Tag::Int(i32::from(signal)));
        self.ctx.world.set_block_entity(be, blocks);
    }

    fn minecarts_at(&self, p: BlockPos) -> u32 {
        let (lo, hi) = (
            [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
            [
                f64::from(p.x) + 1.0,
                f64::from(p.y) + 1.0,
                f64::from(p.z) + 1.0,
            ],
        );
        self.minecarts
            .iter()
            .filter(|(_, b)| (0..3).all(|i| b.min[i] < hi[i] && b.max[i] > lo[i]))
            .count() as u32
    }

    fn entities_in(&self, min: [f64; 3], max: [f64; 3]) -> (u32, u32) {
        let touches = |lo: [f64; 3], hi: [f64; 3]| (0..3).all(|i| lo[i] < max[i] && hi[i] > min[i]);
        let mut living = 0;
        for player in self.ctx.players.lock().expect("players lock").values() {
            let Some(pose) = player.pose else { continue };
            if player.dimension != self.ctx.world.dimension {
                continue;
            }
            if player.game_mode == 3 {
                continue;
            }
            if touches(
                [pose.x - 0.3, pose.y, pose.z - 0.3],
                [pose.x + 0.3, pose.y + 1.8, pose.z + 0.3],
            ) {
                living += 1;
            }
        }
        let mut all = living;
        match &self.entity_boxes {
            Some(boxes) => all += boxes.iter().filter(|b| touches(b.min, b.max)).count() as u32,
            None => {
                if let Ok(entities) = self.ctx.entities.try_lock() {
                    all += entities
                        .iter()
                        .filter(|e| {
                            let b = e.aabb();
                            touches(b.min, b.max)
                        })
                        .count() as u32;
                }
            }
        }
        (all, living)
    }

    fn grow_tree(&mut self, p: BlockPos, state: u32) -> bool {
        crate::growth::grow_tree(self, p, state)
    }

    fn prime_tnt(&mut self, p: BlockPos) -> bool {
        if !self.ctx.game_rule_bool("minecraft:tnt_explodes") {
            return false;
        }
        let entity =
            crate::explosion::primed(self.ctx, to_position(p), crate::explosion::TNT_FUSE, None);
        crate::explosion::primed_sound(self.ctx, &entity);
        self.spawned.push(entity);
        true
    }

    fn torch_burnout(&mut self, p: BlockPos, add: bool) -> bool {
        let mut all = self.ctx.torch_toggles.lock().expect("torch lock");
        let list = all.entry((p.x, p.y, p.z)).or_default();
        list.retain(|&t| t >= self.time - 60);
        if add {
            list.push(self.time);
        }
        let burnt = list.len() >= 8;
        if list.is_empty() {
            all.remove(&(p.x, p.y, p.z));
        }
        burnt
    }

    fn torch_fizz(&mut self, _p: BlockPos) {}

    fn ultrawarm(&self) -> bool {
        self.ctx.world.dimension.contains("nether")
    }

    fn sponge_dried(&mut self, p: BlockPos) {
        let at = [
            f64::from(p.x) + 0.5,
            f64::from(p.y) + 0.5,
            f64::from(p.z) + 0.5,
        ];
        crate::fx::sound(
            self.ctx,
            "block.fire.extinguish",
            crate::fx::category::BLOCKS,
            at,
            1.0,
            0.8,
        );
        // World event 2009: water evaporating (large smoke).
        crate::fx::level_event(self.ctx, 2009, at, 0);
    }

    fn container_transfer(
        &mut self,
        from: BlockPos,
        from_side: mc_tick::Dir,
        to: BlockPos,
        to_side: mc_tick::Dir,
    ) -> bool {
        crate::hoppers::transfer(
            self.ctx,
            to_position(from),
            from_side,
            to_position(to),
            to_side,
        )
    }

    fn hopper_cooldown(&self, p: BlockPos) -> i32 {
        crate::hoppers::cooldown(self.ctx, to_position(p))
    }

    fn set_hopper_cooldown(&mut self, p: BlockPos, ticks: i32) {
        crate::hoppers::set_cooldown(self.ctx, to_position(p), ticks);
    }

    fn hopper_pickup(&mut self, p: BlockPos) -> bool {
        crate::hoppers::pickup(self.ctx, to_position(p))
    }

    fn dispense(&mut self, p: BlockPos, facing: mc_tick::Dir, dropper: bool) {
        crate::hoppers::dispense(self, to_position(p), facing, dropper);
    }

    fn spawn_falling_block(&mut self, p: BlockPos, state: u32) {
        let entity = mc_entity::Entity {
            id: self.ctx.next_entity_id(),
            uuid: crate::util::random_uuid(),
            kind: mc_entity::Kind::FallingBlock { state },
            pos: [f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5],
            vel: [0.0; 3],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        };
        self.spawned.push(entity);
    }

    fn is_block_tick_scheduled(&self, p: BlockPos, block: u32) -> bool {
        self.ctx
            .ticks
            .lock()
            .expect("ticks lock")
            .blocks
            .is_scheduled(p, block)
    }

    fn schedule_block_tick(&mut self, p: BlockPos, block: u32, delay: i64, priority: i32) {
        let at = self.game_time() + delay;
        let mut ticks = self.ctx.ticks.lock().expect("ticks lock");
        ticks.blocks.schedule(p, block, at, priority);
    }
}
