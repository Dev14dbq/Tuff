//! The world as block entities (`mc_tick::block_entity`) see it, on top of
//! the server context: containers, item entities, recipes, jukebox songs,
//! and changes sent to players at once.

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::BlockPos;
use mc_tick::block_entity::jukebox::{Jukeboxes, Song};
use mc_tick::block_entity::transfer::{Containers, Holder, ItemEntities, Store};
use mc_tick::block_entity::{Blocks, Cooking, Effects, Items, Recipes, Slots, brewing, furnace};

use crate::containers::{self, Kind};
use crate::context::{Event, ServerContext};
use crate::level::{WorldLevel, to_pos, to_position};
use crate::tick::lock;

/// The host block entities run on.
#[derive(Clone, Copy)]
pub struct Host<'a> {
    pub ctx: &'a ServerContext,
}

impl<'a> Host<'a> {
    pub fn new(ctx: &'a ServerContext) -> Self {
        Self { ctx }
    }

    /// A container block of this kind at `p`.
    pub fn store(p: Position, kind: Kind, name: &str) -> Store<Kind> {
        Store {
            at: to_pos(p),
            kind,
            holder: holder(name),
        }
    }

    /// An entity holding items as `kind`.
    pub fn entity_store(id: i32, kind: Kind) -> Store<Kind> {
        Store {
            at: to_pos(containers::entity_key(id)),
            kind,
            holder: Holder::Entity,
        }
    }
}

fn holder(name: &str) -> Holder {
    match name {
        "" => Holder::Entity,
        "minecraft:hopper" => Holder::Hopper,
        "minecraft:ender_chest" => Holder::EnderChest,
        "minecraft:crafter" => Holder::Crafter,
        "minecraft:brewing_stand" => Holder::BrewingStand,
        "minecraft:furnace" | "minecraft:blast_furnace" | "minecraft:smoker" => Holder::Furnace,
        _ => Holder::Other,
    }
}

/// The container block at `p` and its name.
fn kind_at(ctx: &ServerContext, p: Position) -> Option<(Kind, &str)> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    let name = blocks.block_name(state)?;
    if name == "minecraft:decorated_pot" {
        return Some((crate::pots::KIND, name));
    }
    let kind = containers::kind_of(name)?;
    Some((kind, name))
}

impl Items for Host<'_> {
    fn max_stack(&self, item: i32) -> i32 {
        self.ctx.items.max_stack(item)
    }

    fn max_damage(&self, item: i32) -> i32 {
        self.ctx.items.max_damage(item)
    }

    fn name(&self, item: i32) -> Option<&str> {
        self.ctx.items.name(item)
    }

    fn id(&self, name: &str) -> Option<i32> {
        self.ctx.items.id(name)
    }

    fn read_items(&self, data: &Compound, size: usize) -> Slots {
        containers::read_items(self.ctx, data, size)
    }

    fn write_items(&self, slots: &[Option<ItemStack>]) -> Tag {
        containers::write_items(self.ctx, slots)
    }
}

impl Blocks for Host<'_> {
    fn block(&self, p: BlockPos) -> Option<u32> {
        self.ctx.world.block(to_position(p), &self.ctx.data.blocks)
    }

    fn block_entity(&self, p: BlockPos) -> Option<Compound> {
        self.ctx
            .world
            .block_entity(to_position(p), &self.ctx.data.blocks)
            .map(|be| be.data)
    }

    fn edit_block_entity(
        &mut self,
        p: BlockPos,
        create: Option<&str>,
        edit: &mut dyn FnMut(&mut Compound),
    ) {
        let blocks = &self.ctx.data.blocks;
        let at = to_position(p);
        let mut be = match (self.ctx.world.block_entity(at, blocks), create) {
            (Some(be), _) => be,
            (None, Some(id)) => mc_world::BlockEntity {
                x: p.x,
                y: p.y,
                z: p.z,
                id: id.to_owned(),
                data: Compound::new(),
            },
            (None, None) => return,
        };
        edit(&mut be.data);
        self.ctx.world.set_block_entity(be, blocks);
    }
}

impl Containers for Host<'_> {
    type Kind = Kind;

    fn store_at(&mut self, p: BlockPos) -> Option<Store<Kind>> {
        let ctx = self.ctx;
        if let Some((kind, name)) = kind_at(ctx, to_position(p)) {
            return Some(Self::store(to_position(p), kind, name));
        }
        let (lo, hi) = (
            [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
            [
                f64::from(p.x) + 1.0,
                f64::from(p.y) + 1.0,
                f64::from(p.z) + 1.0,
            ],
        );
        let found: Vec<(i32, Kind)> = lock(&ctx.entities)
            .iter()
            .filter(|e| !e.removed)
            .filter_map(|e| {
                let mc_entity::Kind::Vehicle(v) = &e.kind else {
                    return None;
                };
                let kind = containers::entity_kind(&crate::vehicles::item_of(&v.craft))?;
                let b = e.aabb();
                (0..3)
                    .all(|i| b.min[i] < hi[i] && b.max[i] > lo[i])
                    .then_some((e.id, kind))
            })
            .collect();
        // One of them, at random.
        let pick = found.get((crate::util::random_u64() % found.len().max(1) as u64) as usize)?;
        Some(Self::entity_store(pick.0, pick.1))
    }

    fn block_store(&self, p: BlockPos) -> Option<Store<Kind>> {
        let (kind, name) = kind_at(self.ctx, to_position(p))?;
        Some(Self::store(to_position(p), kind, name))
    }

    fn slots(&self, store: &Store<Kind>) -> Slots {
        containers::slots(self.ctx, to_position(store.at), store.kind)
    }

    fn put(&mut self, store: &Store<Kind>, changes: &[(usize, Option<ItemStack>)]) {
        containers::put(self.ctx, to_position(store.at), store.kind, changes);
    }
}

impl ItemEntities for Host<'_> {
    fn items_in(&self, min: [f64; 3], max: [f64; 3]) -> Vec<(i32, ItemStack)> {
        lock(&self.ctx.entities)
            .iter()
            .filter(|e| !e.removed && !(0..3).any(|i| e.pos[i] < min[i] || e.pos[i] >= max[i]))
            .filter_map(|e| Some((e.id, e.kind.as_stack()?)))
            .collect()
    }

    fn take_items(&mut self, id: i32, taken: i32) {
        let mut entities = lock(&self.ctx.entities);
        let Some(e) = entities.iter_mut().find(|e| e.id == id) else {
            return;
        };
        let mc_entity::Kind::Item {
            count,
            pickup_delay,
            ..
        } = &mut e.kind
        else {
            return;
        };
        if taken >= *count {
            e.removed = true;
        } else {
            *count -= taken;
            *pickup_delay = 0;
        }
    }
}

impl Effects for Host<'_> {
    fn level_event(&mut self, p: BlockPos, event: i32, data: i32) {
        let at = [f64::from(p.x), f64::from(p.y), f64::from(p.z)];
        crate::fx::level_event(self.ctx, event, at, data);
    }

    fn particles(&mut self, name: &str, at: [f64; 3], spread: f32, speed: f32, count: i32) {
        crate::fx::particles(self.ctx, name, at, spread, speed, count);
    }

    fn spawn_stack(&mut self, at: [f64; 3], stack: &ItemStack) {
        crate::tick::spawn_stack_at(self.ctx, at, stack);
    }

    fn set_block(&mut self, p: BlockPos, state: u32) {
        let mut level = WorldLevel::new(self.ctx);
        level.set_block(to_position(p), state);
        let changes = level.into_changes();
        let _ = self
            .ctx
            .events
            .send(Event::BlocksChanged { by: 0, changes });
    }

    fn update_neighbors(&mut self, at: &[BlockPos]) {
        let mut level = WorldLevel::new(self.ctx);
        let logic = level.logic();
        for &p in at {
            mc_tick::redstone::update_neighbors_at(&mut level, logic, p);
        }
        let changes = level.into_changes();
        if !changes.is_empty() {
            let _ = self
                .ctx
                .events
                .send(Event::BlocksChanged { by: 0, changes });
        }
    }

    fn window_values(&mut self, p: BlockPos, values: &[i16]) {
        let _ = self.ctx.events.send(Event::FurnaceProgress {
            position: to_position(p),
            values: values.to_vec(),
        });
    }

    fn show_block_entity(&mut self, p: BlockPos, id: &str, data: &Compound) {
        let data = crate::world::client_data(id, data);
        if let (Some(kind), Some(data)) = (self.ctx.world.block_entity_kind(id), data) {
            let _ = self.ctx.events.send(Event::BlockEntity {
                position: to_position(p),
                kind,
                data,
            });
        }
    }

    fn crafter_crafted(&mut self, centre: [f64; 3], radius: f64, recipe: &str, used: &[ItemStack]) {
        for uuid in crate::criteria::players_near(self.ctx, centre, radius) {
            crate::criteria::crafted(
                self.ctx,
                uuid,
                "minecraft:crafter_recipe_crafted",
                recipe,
                used,
            );
        }
    }

    fn random(&mut self) -> u64 {
        crate::util::random_u64()
    }
}

impl Recipes for Host<'_> {
    fn cooking(&self, kind: &str, item: i32) -> Option<Cooking> {
        self.ctx
            .recipes
            .get()
            .cooking
            .iter()
            .find(|r| r.kind == kind && r.input.contains(&item))
            .map(|r| Cooking {
                name: r.name.clone(),
                result: r.result.clone(),
                time: r.time,
            })
    }

    fn craft(&self, grid: &[Option<ItemStack>]) -> Option<(ItemStack, String)> {
        self.ctx
            .recipes
            .get()
            .craft_named(grid, 3)
            .map(|(made, recipe)| (made, recipe.to_owned()))
    }

    fn fuels(&self) -> &furnace::Fuels {
        &self.ctx.fuels
    }

    fn mixes(&self) -> &brewing::Mixes {
        &self.ctx.brewing
    }

    fn brewing_fuel(&self, component: &Compound) -> (i32, f32) {
        let json = mc_command::text::to_json(&Tag::Compound(component.clone()));
        crate::brewing::fuel_values(&self.ctx.data, &json)
    }
}

impl Jukeboxes for Host<'_> {
    fn song(&self, disc: &str) -> Option<Song> {
        crate::jukeboxes::song_of(self.ctx, disc)
    }

    fn playing(&self, p: BlockPos) -> Option<(i64, i64)> {
        lock(&self.ctx.jukeboxes).get(&(p.x, p.y, p.z)).copied()
    }

    fn set_playing(&mut self, p: BlockPos, song: Option<(i64, i64)>) {
        let mut playing = lock(&self.ctx.jukeboxes);
        match song {
            Some(song) => {
                playing.insert((p.x, p.y, p.z), song);
            }
            None => {
                playing.remove(&(p.x, p.y, p.z));
            }
        }
    }
}
