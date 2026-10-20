//! `/place feature|template`: worldgen features and structure templates
//! put into the live world, with vanilla's messages.

use std::collections::HashMap;
use std::sync::OnceLock;

use mc_command::args::{Arg, full_id};
use mc_features::structure::template::{Mirror, Rotation, Settings};
use mc_features::{Features, Heightmap, Heights, P, Region};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::text::translate;

use super::run::err;
use super::{Result, Run};
use crate::context::ServerContext;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["place", "feature", "feature", ..] => feature(r),
        ["place", "template", "template", rest @ ..] => template(r, rest),
        ["place", "structure", "structure", ..] => structure(r),
        ["place", "jigsaw", ..] => jigsaw(r),
        _ => return None,
    })
}

/// Features loaded once for worlds without a generator of ours (the flat
/// world); generated worlds use their pipeline's.
static LOADED: OnceLock<Option<Features>> = OnceLock::new();

fn features(ctx: &ServerContext) -> Option<&Features> {
    if let Some(p) = ctx.world.pipeline() {
        return Some(&p.features);
    }
    LOADED
        .get_or_init(|| {
            let observed = |file: &str| {
                std::fs::read_to_string(ctx.config.data_dir.join("observed").join(file))
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|v| serde_json::from_value::<Vec<i32>>(v["states"].clone()).ok())
            };
            let shapes = mc_tick::Shapes::new(
                &ctx.data,
                observed("fence_attach.json"),
                observed("conductor.json"),
            )
            .with_instruments(observed("instrument.json"))
            .with_support(observed("support.json"))
            .with_push(observed("push.json"));
            let biomes: HashMap<String, u32> = (0..)
                .map_while(|i| ctx.world.biome_name(i).map(|n| (n.to_owned(), i)))
                .collect();
            let heights = Heights {
                min_y: ctx.world.min_y,
                height: ctx.world.height as i32,
                sea_level: 63,
            };
            crate::generation::features_with(
                &ctx.data,
                shapes,
                &biomes,
                heights,
                mc_worldgen::generator::Dimension::Overworld,
            )
            .map_err(|e| tracing::warn!("no features for /place: {e}"))
            .ok()
        })
        .as_ref()
}

/// The live world as features see it; changes are kept until `finish`.
struct Live<'a> {
    ctx: &'a ServerContext,
    features: &'a Features,
    changes: HashMap<(i32, i32, i32), u32>,
    order: Vec<(i32, i32, i32)>,
    entities: Vec<(P, Compound)>,
}

impl Live<'_> {
    fn pos(p: P) -> Position {
        Position::new(p.x, p.y, p.z)
    }

    /// Puts the changes into the world (with updates and packets).
    fn finish(self, r: &mut Run) {
        let changes: Vec<(Position, u32)> = self
            .order
            .iter()
            .map(|&(x, y, z)| (Position::new(x, y, z), self.changes[&(x, y, z)]))
            .collect();
        super::world::apply(r.ctx, changes, false, r.out);
        for (p, nbt) in self.entities {
            let pos = Self::pos(p);
            if let Some(state) = r.ctx.world.block(pos, &r.ctx.data.blocks) {
                super::world::put_block_data(r.ctx, pos, state, nbt);
            }
        }
    }
}

impl Region for Live<'_> {
    fn block(&self, p: P) -> u32 {
        if let Some(s) = self.changes.get(&(p.x, p.y, p.z)) {
            return *s;
        }
        self.ctx
            .world
            .block(Self::pos(p), &self.ctx.data.blocks)
            .unwrap_or(self.features.facts.air)
    }

    fn set(&mut self, p: P, state: u32) -> bool {
        let w = &self.ctx.world;
        if p.y < w.min_y || p.y >= w.min_y + w.height as i32 {
            return false;
        }
        if self.changes.insert((p.x, p.y, p.z), state).is_none() {
            self.order.push((p.x, p.y, p.z));
        }
        true
    }

    fn height(&self, map: Heightmap, x: i32, z: i32) -> i32 {
        let w = &self.ctx.world;
        let facts = &self.features.facts;
        let mut y = w.min_y + w.height as i32 - 1;
        while y >= w.min_y {
            if map.counts(facts, self.block(P::new(x, y, z))) {
                return y + 1;
            }
            y -= 1;
        }
        w.min_y
    }

    fn biome(&self, p: P) -> u32 {
        self.ctx
            .world
            .biome(Self::pos(p), &self.ctx.data.blocks)
            .unwrap_or(0)
    }

    fn heights(&self) -> Heights {
        Heights {
            min_y: self.ctx.world.min_y,
            height: self.ctx.world.height as i32,
            sea_level: 63,
        }
    }

    fn set_block_entity(&mut self, p: P, nbt: Compound) {
        self.entities.push((p, nbt));
    }

    fn schedule_tick(&mut self, _p: P) {}

    fn seed(&self) -> i64 {
        self.ctx.config.seed.unwrap_or(0)
    }
}

impl mc_features::structure::Ground for Live<'_> {
    fn height(&mut self, x: i32, z: i32, ocean_floor: bool) -> i32 {
        let map = if ocean_floor {
            Heightmap::OceanFloorWg
        } else {
            Heightmap::WorldSurfaceWg
        };
        Region::height(self, map, x, z)
    }

    fn biome(&mut self, qx: i32, qy: i32, qz: i32) -> u32 {
        Region::biome(self, P::new(qx << 2, qy << 2, qz << 2))
    }
}

/// The block position argument `name`, if its chunk is loaded already
/// (vanilla's check: without loading it).
pub(super) fn loaded_pos(r: &Run, name: &str) -> Result<Position> {
    let p = r
        .block_pos(name)
        .ok_or_else(|| err("argument.pos.unloaded", Vec::new()))?;
    if !super::world::in_world(r.ctx, p) {
        return Err(err("argument.pos.outofworld", Vec::new()));
    }
    if !r.ctx.world.is_loaded(p) {
        return Err(err("argument.pos.unloaded", Vec::new()));
    }
    Ok(p)
}

/// Block changes in order, and block entity data to apply after them.
pub(crate) type Placed = (Vec<(Position, u32)>, Vec<(Position, Compound)>);

/// Places the configured feature `id` at `origin` as `/place feature`
/// would, with `cleared` made air first. Returns the changes in order and
/// the block entity data to apply, or `None` if it did not place.
pub(crate) fn place_configured(
    ctx: &ServerContext,
    id: &str,
    origin: Position,
    cleared: &[Position],
) -> Option<Placed> {
    let features = features(ctx)?;
    let f = features.configured(id)?.clone();
    let mut live = Live {
        ctx,
        features,
        changes: HashMap::new(),
        order: Vec::new(),
        entities: Vec::new(),
    };
    for p in cleared {
        live.set(P::new(p.x, p.y, p.z), features.facts.air);
    }
    let cx = mc_features::feature::Ctx {
        facts: &features.facts,
        features,
    };
    let mut rand = mc_features::rand::WorldgenRandom::legacy(random_seed());
    if !f.place(
        &cx,
        &mut live,
        &mut rand,
        P::new(origin.x, origin.y, origin.z),
    ) {
        return None;
    }
    let changes = live
        .order
        .iter()
        .map(|&(x, y, z)| (Position::new(x, y, z), live.changes[&(x, y, z)]))
        .collect();
    let entities = live
        .entities
        .into_iter()
        .map(|(p, nbt)| (Live::pos(p), nbt))
        .collect();
    Some((changes, entities))
}

fn random_seed() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64)
}

fn feature(r: &mut Run) -> Result<i32> {
    let id = match r.arg("feature") {
        Some(Arg::Id(s) | Arg::Str(s) | Arg::Raw(s)) => full_id(s),
        _ => String::new(),
    };
    let p = if r.arg("pos").is_some() {
        loaded_pos(r, "pos")?
    } else {
        let [x, y, z] = r.src.pos;
        Position::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    };
    let Some(features) = features(r.ctx) else {
        return Err(err("commands.place.feature.failed", Vec::new()));
    };
    let Some(f) = features.configured(&id).cloned() else {
        return Err(err(
            "argument.resource_or_id.no_such_element",
            vec![Tag::String(id), Tag::from("minecraft:worldgen/feature")],
        ));
    };
    let mut live = Live {
        ctx: r.ctx,
        features,
        changes: HashMap::new(),
        order: Vec::new(),
        entities: Vec::new(),
    };
    let cx = mc_features::feature::Ctx {
        facts: &features.facts,
        features,
    };
    let mut rand = mc_features::rand::WorldgenRandom::legacy(random_seed());
    if !f.place(&cx, &mut live, &mut rand, P::new(p.x, p.y, p.z)) {
        return Err(err("commands.place.feature.failed", Vec::new()));
    }
    live.finish(r);
    r.success(
        translate(
            "commands.place.feature.success",
            vec![Tag::String(id), Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)],
        ),
        true,
    );
    Ok(1)
}

fn template(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let id = r
        .string("template")
        .map(|s| full_id(&s))
        .unwrap_or_default();
    let p = if rest.is_empty() {
        let [x, y, z] = r.src.pos;
        Position::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    } else {
        loaded_pos(r, "pos")?
    };
    let features = features(r.ctx);
    let Some(t) = features.and_then(|f| f.structures.template(&id)) else {
        return Err(err(
            "commands.place.template.invalid",
            vec![Tag::String(id)],
        ));
    };
    let features = features.expect("templates come from features");
    let rotation = match r.string("rotation").as_deref() {
        Some("clockwise_90") => Rotation(1),
        Some("180") => Rotation(2),
        Some("counterclockwise_90") => Rotation(3),
        _ => Rotation(0),
    };
    let mirror = match r.string("mirror").as_deref() {
        Some("left_right") => Mirror::LeftRight,
        Some("front_back") => Mirror::FrontBack,
        _ => Mirror::None,
    };
    let integrity = r.float("integrity").unwrap_or(1.0);
    let seed = r.int("seed").map_or(0, i64::from);
    let mut settings = Settings {
        rotation,
        mirror,
        ..Settings::default()
    };
    if integrity < 1.0 {
        settings.processors.push(std::sync::Arc::new(
            mc_features::structure::template::Processor::BlockRot {
                integrity,
                rottable: None,
            },
        ));
        settings.seed = Some(seed);
    }
    let mut live = Live {
        ctx: r.ctx,
        features,
        changes: HashMap::new(),
        order: Vec::new(),
        entities: Vec::new(),
    };
    let mut rand = mc_features::rand::WorldgenRandom::legacy(seed);
    let at = P::new(p.x, p.y, p.z);
    // Vanilla reports success even when integrity left nothing to place.
    t.place(&features.facts, &mut live, &mut rand, at, at, &settings);
    live.finish(r);
    r.success(
        translate(
            "commands.place.template.success",
            vec![Tag::String(id), Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)],
        ),
        true,
    );
    Ok(1)
}

fn structure(r: &mut Run) -> Result<i32> {
    let id = r
        .string("structure")
        .map(|s| full_id(&s))
        .unwrap_or_default();
    let p = if r.arg("pos").is_some() {
        loaded_pos(r, "pos")?
    } else {
        let [x, y, z] = r.src.pos;
        Position::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    };
    let features = features(r.ctx);
    let Some(features) = features.filter(|f| f.structures.has(&id)) else {
        return Err(err(
            "commands.place.structure.invalid",
            vec![Tag::String(id)],
        ));
    };
    let mut live = Live {
        ctx: r.ctx,
        features,
        changes: HashMap::new(),
        order: Vec::new(),
        entities: Vec::new(),
    };
    let seed = r.ctx.config.seed.unwrap_or(0);
    let heights = Region::heights(&live);
    let Some(start) =
        features
            .structures
            .generate_anywhere(&id, seed, p.x >> 4, p.z >> 4, heights, &mut live)
    else {
        return Err(err("commands.place.structure.failed", Vec::new()));
    };
    let b = start.bbox;
    for cz in (b.min.z >> 4)..=(b.max.z >> 4) {
        for cx in (b.min.x >> 4)..=(b.max.x >> 4) {
            let mut rand = mc_features::rand::WorldgenRandom::legacy(0);
            rand.set_large_feature_seed(seed, cx, cz);
            features
                .structures
                .place(features, &mut live, &mut rand, &start, cx, cz);
        }
    }
    live.finish(r);
    r.success(
        translate(
            "commands.place.structure.success",
            vec![Tag::String(id), Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)],
        ),
        true,
    );
    Ok(1)
}

fn jigsaw(r: &mut Run) -> Result<i32> {
    let pool = r.string("pool").map(|s| full_id(&s)).unwrap_or_default();
    let target = r.string("target").map(|s| full_id(&s)).unwrap_or_default();
    let depth = r.int("max_depth").unwrap_or(1);
    let p = if r.arg("position").is_some() {
        loaded_pos(r, "position")?
    } else {
        let [x, y, z] = r.src.pos;
        Position::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    };
    let features = features(r.ctx);
    let Some(features) = features.filter(|f| f.structures.has_pool(&pool)) else {
        return Err(err(
            "commands.place.jigsaw.invalid",
            vec![Tag::String(pool)],
        ));
    };
    let mut live = Live {
        ctx: r.ctx,
        features,
        changes: HashMap::new(),
        order: Vec::new(),
        entities: Vec::new(),
    };
    let heights = Region::heights(&live);
    let seed = crate::util::random_u64() as i64;
    let Ok(start) = features.structures.generate_jigsaw(
        &pool,
        &target,
        depth,
        mc_features::P::new(p.x, p.y, p.z),
        seed,
        heights,
        &mut live,
    ) else {
        return Err(err("commands.place.jigsaw.failed", Vec::new()));
    };
    let b = start.bbox;
    let world_seed = r.ctx.config.seed.unwrap_or(0);
    for cz in (b.min.z >> 4)..=(b.max.z >> 4) {
        for cx in (b.min.x >> 4)..=(b.max.x >> 4) {
            let mut rand = mc_features::rand::WorldgenRandom::legacy(0);
            rand.set_large_feature_seed(world_seed, cx, cz);
            features
                .structures
                .place(features, &mut live, &mut rand, &start, cx, cz);
        }
    }
    live.finish(r);
    r.success(
        translate(
            "commands.place.jigsaw.success",
            vec![Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)],
        ),
        true,
    );
    Ok(1)
}
