//! The dragon fight of the End (minecraft.wiki, "Ender Dragon", "Exit
//! portal", "End Gateway", "End Crystal"), kept as 26.3 keeps it in
//! `dimensions/minecraft/the_end/data/minecraft/ender_dragon_fight.dat`
//! (older worlds have it as `DragonFight` in `level.dat`; it is read from
//! there when the file is missing):
//! - while a player is within 192 blocks of the portal the fight runs: the
//!   arena is scanned once (an exit portal is made if there is none; a
//!   dragon without an active portal is a fresh fight), a dragon is made
//!   when the fight has none, and the crystals on the pillars are counted
//!   every five seconds;
//! - a dragon killed opens the exit portal, makes the next of the 20 end
//!   gateways (their order shuffled by the seed) and, the first time, puts
//!   the egg on top of the portal;
//! - four crystals on the sides of the exit portal bring it back: the
//!   pillars are rebuilt one by one with invulnerable crystals, then the
//!   four crystals explode and a new dragon comes; destroying one of the four
//!   stops it;
//! - the gateways on the main island lead 1024 blocks out (shortened or
//!   lengthened to the edge of the outer islands), where a gateway back is
//!   made.

use std::path::PathBuf;

use mc_entity::{Entity, Kind, Species};
use mc_features::end::Spike;
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;

use crate::context::{Event, Pose, ServerContext};
use crate::level::WorldLevel;
use crate::tick::lock;

const FILE: &str = "dimensions/minecraft/the_end/data/minecraft/ender_dragon_fight.dat";
/// Where the fight and its sounds are centred.
pub const CENTRE: [f64; 3] = [0.0, 128.0, 0.0];
/// Players within this far of [`CENTRE`] take part.
const RANGE: f64 = 192.0;
const GATEWAYS: i32 = 20;

/// The stages of bringing the dragon back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Start,
    PreparingToSummonPillars,
    SummoningPillars,
    SummoningDragon,
    End,
}

impl Stage {
    const NAMES: [(Stage, &'static str); 5] = [
        (Stage::Start, "start"),
        (
            Stage::PreparingToSummonPillars,
            "preparing_to_summon_pillars",
        ),
        (Stage::SummoningPillars, "summoning_pillars"),
        (Stage::SummoningDragon, "summoning_dragon"),
        (Stage::End, "end"),
    ];

    fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(s, _)| *s == self)
            .map_or("start", |n| n.1)
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::NAMES.iter().find(|(_, n)| *n == name).map(|n| n.0)
    }
}

#[derive(Debug)]
pub struct Fight {
    file: Option<PathBuf>,
    data_version: i32,
    pub dragon_killed: bool,
    pub previously_killed: bool,
    pub needs_state_scanning: bool,
    /// The fight's dragon.
    pub dragon: Option<u128>,
    /// The centre of the exit portal (its bottom layer).
    pub exit_portal: Option<[i32; 3]>,
    /// Gateways still to come, by index on the ring; the last comes next.
    pub gateways: Vec<i32>,
    pub respawn: Option<Stage>,
    pub respawn_time: i32,
    /// The four crystals on the portal (UUIDs).
    pub respawn_crystals: Vec<u128>,
    pub crystals_alive: i32,
    /// A crystal was placed: look for the four.
    check_respawn: bool,
    since_crystals: i32,
    since_dragon_seen: i32,
    seed: i64,
}

fn uuid_tag(u: u128) -> Tag {
    Tag::IntArray(vec![
        (u >> 96) as i32,
        (u >> 64) as i32,
        (u >> 32) as i32,
        u as i32,
    ])
}

fn uuid_of(t: Option<&Tag>) -> Option<u128> {
    match t? {
        Tag::IntArray(v) if v.len() == 4 => Some(
            v.iter()
                .fold(0u128, |a, &p| (a << 32) | u128::from(p as u32)),
        ),
        _ => None,
    }
}

fn flag(c: &Compound, key: &str) -> Option<bool> {
    match c.get(key)? {
        Tag::Byte(b) => Some(*b != 0),
        Tag::Int(b) => Some(*b != 0),
        _ => None,
    }
}

fn int(c: &Compound, key: &str) -> Option<i32> {
    match c.get(key)? {
        Tag::Int(v) => Some(*v),
        Tag::Short(v) => Some(i32::from(*v)),
        Tag::Byte(v) => Some(i32::from(*v)),
        _ => None,
    }
}

/// A block position stored as `[I; x, y, z]` or `{X, Y, Z}`.
fn block_pos(t: Option<&Tag>) -> Option<[i32; 3]> {
    match t? {
        Tag::IntArray(v) if v.len() == 3 => Some([v[0], v[1], v[2]]),
        Tag::Compound(c) => Some([int(c, "X")?, int(c, "Y")?, int(c, "Z")?]),
        _ => None,
    }
}

/// The gateway ring's indices in the order a world with this seed uses
/// them (the last first).
pub fn shuffled_gateways(seed: i64) -> Vec<i32> {
    use mc_worldgen::random::{Legacy, RandomSource};
    let mut list: Vec<i32> = (0..GATEWAYS).collect();
    let mut r = Legacy::new(seed);
    for j in (2..=list.len()).rev() {
        let k = r.next_int_bound(j as i32) as usize;
        list.swap(j - 1, k);
    }
    list
}

/// Where gateway `i` of the ring stands.
pub fn gateway_pos(i: i32) -> [i32; 3] {
    let angle = 2.0 * (-std::f64::consts::PI + std::f64::consts::PI / 20.0 * f64::from(i));
    [
        (96.0 * angle.cos()).floor() as i32,
        75,
        (96.0 * angle.sin()).floor() as i32,
    ]
}

impl Fight {
    /// A world's fight: from its file, from an old `level.dat`, or new.
    pub fn load(ctx: &ServerContext) -> Self {
        let folder = ctx.world.folder().map(|f| f.to_path_buf());
        let seed = ctx.world.seed().unwrap_or(0);
        let mut fight = Self {
            file: folder.as_ref().map(|f| f.join(FILE)),
            data_version: ctx.data.version.world_version,
            dragon_killed: false,
            previously_killed: false,
            needs_state_scanning: true,
            dragon: None,
            exit_portal: None,
            gateways: shuffled_gateways(seed),
            respawn: None,
            respawn_time: 0,
            respawn_crystals: Vec::new(),
            crystals_alive: 0,
            check_respawn: false,
            since_crystals: 0,
            since_dragon_seen: 0,
            seed,
        };
        let stored = fight
            .file
            .as_ref()
            .and_then(|f| crate::world::read_nbt_gz(f).ok())
            .and_then(|root| match root.get("data") {
                Some(Tag::Compound(d)) => Some((d.clone(), false)),
                _ => None,
            })
            .or_else(|| {
                let level = crate::world::read_nbt_gz(&folder.as_ref()?.join("level.dat")).ok()?;
                match level.get("Data") {
                    Some(Tag::Compound(d)) => match d.get("DragonFight") {
                        Some(Tag::Compound(f)) => Some((f.clone(), true)),
                        _ => None,
                    },
                    _ => None,
                }
            });
        if let Some((d, old)) = stored {
            fight.read(&d, old);
        }
        fight
    }

    /// The fields of the file (`old`: the camel-case names of `level.dat`).
    pub fn read(&mut self, d: &Compound, old: bool) {
        let key = |new: &'static str, camel: &'static str| if old { camel } else { new };
        if let Some(v) = flag(d, key("needs_state_scanning", "NeedsStateScanning")) {
            self.needs_state_scanning = v;
        }
        if let Some(v) = flag(d, key("dragon_killed", "DragonKilled")) {
            self.dragon_killed = v;
        }
        if let Some(v) = flag(d, key("previously_killed", "PreviouslyKilled")) {
            self.previously_killed = v;
        }
        self.dragon = uuid_of(d.get(key("dragon_uuid", "Dragon")));
        self.exit_portal = block_pos(d.get(key("exit_portal_location", "ExitPortalLocation")));
        match d.get(key("gateways", "Gateways")) {
            Some(Tag::IntArray(l)) => self.gateways = l.clone(),
            Some(Tag::List(l)) => {
                self.gateways = l
                    .iter()
                    .filter_map(|t| match t {
                        Tag::Int(v) => Some(*v),
                        _ => None,
                    })
                    .collect();
            }
            _ => {}
        }
        if let Some(Tag::String(s)) = d.get("respawn_stage") {
            self.respawn = Stage::from_name(s);
        } else if old && flag(d, "IsRespawning") == Some(true) {
            self.respawn = Some(Stage::Start);
        }
        self.respawn_time = int(d, "respawn_time").unwrap_or(0);
        if let Some(Tag::List(l)) = d.get("respawn_crystals") {
            self.respawn_crystals = l.iter().filter_map(|t| uuid_of(Some(t))).collect();
        }
    }

    /// The fields as the file has them.
    pub fn write(&self) -> Compound {
        let mut d = Compound::new();
        d.insert(
            "needs_state_scanning",
            Tag::Byte(i8::from(self.needs_state_scanning)),
        );
        d.insert("dragon_killed", Tag::Byte(i8::from(self.dragon_killed)));
        d.insert(
            "previously_killed",
            Tag::Byte(i8::from(self.previously_killed)),
        );
        if let Some(u) = self.dragon {
            d.insert("dragon_uuid", uuid_tag(u));
        }
        if let Some(p) = self.exit_portal {
            d.insert("exit_portal_location", Tag::IntArray(p.to_vec()));
        }
        d.insert("gateways", Tag::IntArray(self.gateways.clone()));
        if let Some(stage) = self.respawn {
            d.insert("respawn_stage", Tag::String(stage.name().into()));
            d.insert("respawn_time", Tag::Int(self.respawn_time));
            d.insert(
                "respawn_crystals",
                Tag::List(self.respawn_crystals.iter().map(|&u| uuid_tag(u)).collect()),
            );
        }
        d
    }

    pub fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(self.write()));
        root.insert("DataVersion", Tag::Int(self.data_version));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::world::write_nbt_gz(file, &root) {
            tracing::warn!("saving the dragon fight: {e:#}");
        }
    }

    /// The pillars of this world.
    pub fn spikes(&self) -> Vec<Spike> {
        mc_features::end::spikes_for_seed(self.seed)
    }
}

/// Runs `f` on the End's fight (read on first use).
fn with_fight<R>(ctx: &ServerContext, f: impl FnOnce(&mut Fight) -> R) -> R {
    let mut state = lock(&ctx.bosses);
    let fight = state.fight.get_or_insert_with(|| Fight::load(ctx));
    f(fight)
}

/// The fight's dragon UUID and whether the egg has been won, for the
/// dragon's tick (`None` outside the End).
pub fn fight_dragon(ctx: &ServerContext) -> Option<(Option<u128>, bool)> {
    if ctx.world.dimension != "minecraft:the_end" {
        return None;
    }
    Some(with_fight(ctx, |f| (f.dragon, f.previously_killed)))
}

/// The crystals alive on the pillars (0 outside the End).
pub fn crystals_alive(ctx: &ServerContext) -> Option<i32> {
    if ctx.world.dimension != "minecraft:the_end" {
        return None;
    }
    Some(with_fight(ctx, |f| f.crystals_alive))
}

/// Players taking part: in the End, alive, within 192 blocks of the portal.
fn players_near(ctx: &ServerContext) -> Vec<(u128, [f64; 3])> {
    let vitals = lock(&ctx.vitals);
    lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter(|p| !vitals.get(&p.profile.uuid).is_some_and(|v| v.dead))
        .filter_map(|p| p.pose.map(|q| (p.profile.uuid, [q.x, q.y, q.z])))
        .filter(|(_, p)| super::dist_sq(*p, CENTRE) <= RANGE * RANGE)
        .collect()
}

/// One tick of the fight.
pub fn tick(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut Vec<Entity>,
    fresh: &mut Vec<Entity>,
    now: i64,
) {
    let _ = now;
    if players_near(ctx).is_empty() {
        return;
    }
    let mut state = lock(&ctx.bosses);
    let fight = state.fight.get_or_insert_with(|| Fight::load(ctx));
    // The dragons of the End: the fight's one shows its bar.
    for e in entities.iter_mut() {
        if let Kind::Mob(m) = &mut e.kind
            && let Some(d) = m.dragon_mut()
        {
            d.in_fight = fight.dragon == Some(e.uuid);
        }
    }
    let mut changed = false;
    if fight.needs_state_scanning {
        scan_state(ctx, level, fight, entities);
        fight.needs_state_scanning = false;
        changed = true;
    }
    if std::mem::take(&mut fight.check_respawn) {
        changed |= try_respawn(ctx, level, fight, entities);
    }
    if let Some(stage) = fight.respawn {
        let time = fight.respawn_time;
        fight.respawn_time += 1;
        changed |= respawn_tick(ctx, level, fight, entities, fresh, stage, time);
    }
    if !fight.dragon_killed {
        let seen = fight.dragon.is_some_and(|u| {
            entities
                .iter()
                .chain(fresh.iter())
                .any(|e| e.uuid == u && !e.removed)
        });
        if seen {
            fight.since_dragon_seen = 0;
        } else {
            fight.since_dragon_seen += 1;
        }
        if fight.dragon.is_none() || fight.since_dragon_seen >= 1200 {
            find_or_create_dragon(ctx, fight, entities, fresh);
            fight.since_dragon_seen = 0;
            changed = true;
        }
        fight.since_crystals += 1;
        if fight.since_crystals >= 100 {
            fight.crystals_alive = count_crystals(fight, entities);
            fight.since_crystals = 0;
        }
    }
    if changed {
        fight.save();
    }
}

fn dragons(entities: &[Entity]) -> impl Iterator<Item = &Entity> {
    entities.iter().filter(|e| {
        !e.removed && matches!(&e.kind, Kind::Mob(m) if m.species == Species::EnderDragon)
    })
}

/// The first look at the arena: is the portal open (the dragon was beaten
/// before), is there a dragon?
fn scan_state(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    fight: &mut Fight,
    entities: &mut [Entity],
) {
    let active = has_active_portal(ctx, fight);
    if active {
        fight.previously_killed = true;
    } else {
        fight.previously_killed = false;
        if find_exit_portal(ctx, fight).is_none() {
            spawn_exit_portal(ctx, level, fight, false);
        }
    }
    let first = entities.iter_mut().find(|e| {
        !e.removed && matches!(&e.kind, Kind::Mob(m) if m.species == Species::EnderDragon)
    });
    match first {
        None => fight.dragon_killed = true,
        Some(d) => {
            fight.dragon = Some(d.uuid);
            fight.dragon_killed = false;
            if !active {
                // A fresh fight: this one goes, a new one comes.
                d.removed = true;
                let _ = ctx.events.send(Event::EntitiesRemoved(vec![d.id]));
                fight.dragon = None;
            }
        }
    }
    if !fight.previously_killed && fight.dragon_killed {
        fight.dragon_killed = false;
    }
}

fn find_or_create_dragon(
    ctx: &ServerContext,
    fight: &mut Fight,
    entities: &[Entity],
    fresh: &mut Vec<Entity>,
) {
    match dragons(entities).next() {
        Some(d) => fight.dragon = Some(d.uuid),
        None => {
            let d = new_dragon(ctx);
            fight.dragon = Some(d.uuid);
            fresh.push(d);
        }
    }
}

/// A dragon for the fight: at (0, 128, 0), circling.
pub fn new_dragon(ctx: &ServerContext) -> Entity {
    let mut e = crate::mobs::new_mob(ctx, Species::EnderDragon, CENTRE);
    if let Kind::Mob(m) = &mut e.kind {
        m.yaw = (super::rnd() * 360.0) as f32;
        m.head_yaw = m.yaw;
        if let Some(d) = m.dragon_mut() {
            d.phase = mc_entity::boss::Phase::HoldingPattern;
            d.in_fight = true;
            d.part_base = e.id + 1;
        }
    }
    e
}

/// The box above a pillar where its crystal is.
fn spike_box(s: &Spike, min_y: i32) -> mc_entity::Aabb {
    mc_entity::Aabb {
        min: [
            f64::from(s.x - s.radius),
            f64::from(min_y),
            f64::from(s.z - s.radius),
        ],
        max: [
            f64::from(s.x + s.radius),
            f64::from(s.height + 10),
            f64::from(s.z + s.radius),
        ],
    }
}

fn inside(e: &Entity, b: &mc_entity::Aabb) -> bool {
    let a = e.aabb();
    (0..3).all(|i| a.min[i] < b.max[i] && a.max[i] > b.min[i])
}

fn count_crystals(fight: &Fight, entities: &[Entity]) -> i32 {
    let min_y = -64;
    fight
        .spikes()
        .iter()
        .map(|s| {
            let b = spike_box(s, min_y);
            entities
                .iter()
                .filter(|e| !e.removed && matches!(e.kind, Kind::Crystal(_)) && inside(e, &b))
                .count() as i32
        })
        .sum()
}

/// The highest block that stops motion at a column (its y), and whether
/// leaves count.
fn top(ctx: &ServerContext, x: i32, z: i32, leaves: bool) -> i32 {
    let (min, max) = (ctx.world.min_y, ctx.world.min_y + ctx.world.height as i32);
    let blocks = &ctx.data.blocks;
    for y in (min..max).rev() {
        let Some(s) = ctx.world.block(Position::new(x, y, z), blocks) else {
            continue;
        };
        if blocks.is_air(s) {
            continue;
        }
        let name = blocks.block_name(s).unwrap_or("");
        if !leaves && name.ends_with("_leaves") {
            continue;
        }
        if ctx.rules.blocks_motion(s)
            || blocks.property(s, "waterlogged") == Some("true")
            || name == "minecraft:water"
            || name == "minecraft:lava"
        {
            return y;
        }
    }
    min - 1
}

/// The first free y above the motion-blocking blocks of a column.
pub fn surface(ctx: &ServerContext, x: i32, z: i32) -> i32 {
    top(ctx, x, z, true) + 1
}

fn is_air(ctx: &ServerContext, p: Position) -> bool {
    ctx.world
        .block(p, &ctx.data.blocks)
        .is_none_or(|s| ctx.data.blocks.is_air(s))
}

fn is(ctx: &ServerContext, p: Position, name: &str) -> bool {
    let blocks = &ctx.data.blocks;
    ctx.world
        .block(p, blocks)
        .and_then(|s| blocks.block_name(s))
        == Some(name)
}

/// The exit portal: its centre pillar of bedrock with the ring around it.
/// Looks down the middle column; remembers what it finds.
fn find_exit_portal(ctx: &ServerContext, fight: &mut Fight) -> Option<[i32; 3]> {
    let looks = |o: [i32; 3]| {
        let p = |dx: i32, dy: i32, dz: i32| Position::new(o[0] + dx, o[1] + dy, o[2] + dz);
        (0..4).all(|dy| is(ctx, p(0, dy, 0), "minecraft:bedrock"))
            && [(3, 0), (-3, 0), (0, 3), (0, -3)]
                .iter()
                .all(|&(dx, dz)| is(ctx, p(dx, 0, dz), "minecraft:bedrock"))
    };
    if let Some(o) = fight.exit_portal
        && looks(o)
    {
        return Some(o);
    }
    let start = surface(ctx, 0, 0);
    for y in (ctx.world.min_y..=start).rev() {
        let o = [0, y, 0];
        if looks(o) {
            fight.exit_portal = Some(o);
            return Some(o);
        }
    }
    None
}

fn has_active_portal(ctx: &ServerContext, fight: &mut Fight) -> bool {
    let Some(o) = find_exit_portal(ctx, fight) else {
        return false;
    };
    (-2..=2).any(|dx| {
        (-2..=2).any(|dz| {
            is(
                ctx,
                Position::new(o[0] + dx, o[1], o[2] + dz),
                "minecraft:end_portal",
            )
        })
    })
}

fn state(ctx: &ServerContext, name: &str) -> u32 {
    ctx.data.blocks.default_state(name).unwrap_or(0)
}

/// Builds the exit portal at the fight's place (found from the surface at
/// the centre the first time): open (`active`) or not.
pub fn spawn_exit_portal(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    fight: &mut Fight,
    active: bool,
) {
    let o = match fight.exit_portal {
        Some(o) => o,
        None => {
            let mut y = top(ctx, 0, 0, false);
            while is(ctx, Position::new(0, y, 0), "minecraft:bedrock") && y > 63 {
                y -= 1;
            }
            let y = y.max(ctx.world.min_y + 1);
            let o = [0, y, 0];
            fight.exit_portal = Some(o);
            o
        }
    };
    build_podium(ctx, level, o, active);
}

/// The exit portal's blocks around `o` (the end podium).
pub fn build_podium(ctx: &ServerContext, level: &mut WorldLevel, o: [i32; 3], active: bool) {
    let (bedrock, end_stone, air, portal) = (
        state(ctx, "minecraft:bedrock"),
        state(ctx, "minecraft:end_stone"),
        ctx.shapes.air(),
        state(ctx, "minecraft:end_portal"),
    );
    for y in o[1] - 1..=o[1] + 32 {
        for x in o[0] - 4..=o[0] + 4 {
            for z in o[2] - 4..=o[2] + 4 {
                let d2 = (x - o[0]).pow(2) + (y - o[1]).pow(2) + (z - o[2]).pow(2);
                let near = f64::from(d2) < 2.5 * 2.5;
                if !near && f64::from(d2) >= 3.5 * 3.5 {
                    continue;
                }
                let p = Position::new(x, y, z);
                let to = if y < o[1] {
                    if near { bedrock } else { end_stone }
                } else if y > o[1] {
                    air
                } else if !near {
                    bedrock
                } else if active {
                    portal
                } else {
                    air
                };
                level.set_block(p, to);
            }
        }
    }
    for dy in 0..4 {
        level.set_block(Position::new(o[0], o[1] + dy, o[2]), bedrock);
    }
    let torch = state(ctx, "minecraft:wall_torch");
    for (dx, dz, facing) in [
        (0, -1, "north"),
        (1, 0, "east"),
        (0, 1, "south"),
        (-1, 0, "west"),
    ] {
        level.set_block(
            Position::new(o[0] + dx, o[1] + 2, o[2] + dz),
            ctx.shapes.with(torch, "facing", facing),
        );
    }
}

/// A crystal was placed in the End: the fight looks for the four next tick.
pub fn try_respawn_later(ctx: &ServerContext) {
    with_fight(ctx, |f| f.check_respawn = true);
}

/// Four crystals on the sides of the portal (and no dragon): the respawn
/// begins. Returns whether it did.
fn try_respawn(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    fight: &mut Fight,
    entities: &[Entity],
) -> bool {
    if !fight.dragon_killed || fight.respawn.is_some() {
        return false;
    }
    if fight.exit_portal.is_none() && find_exit_portal(ctx, fight).is_none() {
        spawn_exit_portal(ctx, level, fight, true);
    }
    let Some(o) = fight.exit_portal else {
        return false;
    };
    let mut found = Vec::new();
    for (dx, dz) in [(0, -2), (2, 0), (0, 2), (-2, 0)] {
        let (x, y, z) = (
            f64::from(o[0] + dx),
            f64::from(o[1] + 1),
            f64::from(o[2] + dz),
        );
        let b = mc_entity::Aabb {
            min: [x, y, z],
            max: [x + 1.0, y + 1.0, z + 1.0],
        };
        let here: Vec<u128> = entities
            .iter()
            .filter(|e| !e.removed && matches!(e.kind, Kind::Crystal(_)) && inside(e, &b))
            .map(|e| e.uuid)
            .collect();
        if here.is_empty() {
            return false;
        }
        found.extend(here);
    }
    // The old portal becomes end stone, then is built anew, closed.
    let end_stone = state(ctx, "minecraft:end_stone");
    for y in o[1] - 1..=o[1] + 4 {
        for x in o[0] - 3..=o[0] + 3 {
            for z in o[2] - 3..=o[2] + 3 {
                let p = Position::new(x, y, z);
                if is(ctx, p, "minecraft:bedrock") || is(ctx, p, "minecraft:end_portal") {
                    level.set_block(p, end_stone);
                }
            }
        }
    }
    // The egg goes too.
    for dy in 4..=6 {
        let p = Position::new(o[0], o[1] + dy, o[2]);
        if is(ctx, p, "minecraft:dragon_egg") {
            level.set_block(p, ctx.shapes.air());
        }
    }
    fight.respawn = Some(Stage::Start);
    fight.respawn_time = 0;
    spawn_exit_portal(ctx, level, fight, false);
    fight.respawn_crystals = found;
    true
}

fn set_beam(
    ctx: &ServerContext,
    entities: &mut [Entity],
    fresh: &mut [Entity],
    uuids: &[u128],
    beam: Option<[i32; 3]>,
) {
    for e in entities.iter_mut().chain(fresh.iter_mut()) {
        if uuids.contains(&e.uuid)
            && let Kind::Crystal(c) = &mut e.kind
            && c.beam != beam
        {
            c.beam = beam;
            crystal_data(ctx, e);
        }
    }
}

/// Tells clients a crystal's fields changed.
fn crystal_data(ctx: &ServerContext, e: &Entity) {
    if let Kind::Crystal(c) = &e.kind {
        crate::fx::emit(
            ctx,
            crate::fx::Fx::Data(mc_protocol::packets::play::SetEntityData {
                entity_id: e.id,
                entries: super::crystal::metadata(c),
            }),
        );
    }
}

const BEAM_CENTRE: [i32; 3] = [0, 128, 0];

/// One tick of the respawn at `stage`, `time` ticks into it. Returns
/// whether the stage changed.
fn respawn_tick(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    fight: &mut Fight,
    entities: &mut Vec<Entity>,
    fresh: &mut Vec<Entity>,
    stage: Stage,
    time: i32,
) -> bool {
    let crystals = fight.respawn_crystals.clone();
    let centre = [0.0, 128.0, 0.0];
    let mut next = None;
    match stage {
        Stage::Start => {
            set_beam(ctx, entities, fresh, &crystals, Some(BEAM_CENTRE));
            next = Some(Stage::PreparingToSummonPillars);
        }
        Stage::PreparingToSummonPillars => {
            if time < 100 {
                if matches!(time, 0 | 50 | 51 | 52) || time >= 95 {
                    crate::fx::level_event(ctx, 3001, centre, 0);
                }
            } else {
                next = Some(Stage::SummoningPillars);
            }
        }
        Stage::SummoningPillars => {
            let (start, end) = (time % 40 == 0, time % 40 == 39);
            if start || end {
                let spikes = fight.spikes();
                let j = (time / 40) as usize;
                if let Some(s) = spikes.get(j) {
                    if start {
                        set_beam(
                            ctx,
                            entities,
                            fresh,
                            &crystals,
                            Some([s.x, s.height + 1, s.z]),
                        );
                    } else {
                        rebuild_spike(ctx, level, entities, fresh, s);
                    }
                } else if start {
                    next = Some(Stage::SummoningDragon);
                }
            }
        }
        Stage::SummoningDragon => {
            if time >= 100 {
                next = Some(Stage::End);
                reset_spike_crystals(ctx, fight, entities, fresh);
                let mut blasts = Vec::new();
                for e in entities.iter_mut().chain(fresh.iter_mut()) {
                    if crystals.contains(&e.uuid) && matches!(e.kind, Kind::Crystal(_)) {
                        if let Kind::Crystal(c) = &mut e.kind {
                            c.beam = None;
                        }
                        e.removed = true;
                        blasts.push(e.pos);
                    }
                }
                // Explosions that break nothing.
                for b in blasts {
                    let _ = ctx.events.send(Event::Explosion {
                        center: b,
                        radius: super::crystal::POWER,
                        block_count: 0,
                        particle: ctx.explosion_particle,
                        sound: ctx.explosion_sound,
                        knockback: Vec::new(),
                    });
                    hurt_near(ctx, entities, b);
                }
            } else if time >= 80 {
                crate::fx::level_event(ctx, 3001, centre, 0);
            } else if time == 0 {
                set_beam(ctx, entities, fresh, &crystals, Some(BEAM_CENTRE));
            } else if time < 5 {
                crate::fx::level_event(ctx, 3001, centre, 0);
            }
        }
        Stage::End => {}
    }
    let Some(next) = next else {
        return false;
    };
    fight.respawn_time = 0;
    if next == Stage::End {
        fight.respawn = None;
        fight.respawn_crystals.clear();
        fight.dragon_killed = false;
        let d = new_dragon(ctx);
        fight.dragon = Some(d.uuid);
        // Everyone at the fight summoned it.
        let s = crate::criteria::entity(ctx, &d);
        for uuid in crate::criteria::players_near(ctx, CENTRE, 192.0) {
            crate::criteria::with_entity(ctx, uuid, "minecraft:summoned_entity", &s);
        }
        fresh.push(d);
    } else {
        fight.respawn = Some(next);
    }
    true
}

/// The summoning crystals' last blasts hurt players and mobs but break no
/// block.
fn hurt_near(ctx: &ServerContext, entities: &mut [Entity], at: [f64; 3]) {
    let reach = f64::from(super::crystal::POWER) * 2.0;
    for e in entities.iter_mut().filter(|e| !e.removed) {
        let d = super::dist_sq(e.pos, at).sqrt() / reach;
        if d > 1.0 || !matches!(e.kind, Kind::Mob(_)) {
            continue;
        }
        let i = 1.0 - d;
        let h = crate::mobs::hurt::Harm::new(
            ((i * i + i) / 2.0 * 7.0 * reach + 1.0).floor() as f32,
            "minecraft:explosion",
        );
        crate::mobs::hurt::harm(ctx, e, &h);
    }
    let players: Vec<(u128, [f64; 3])> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension && p.game_mode != 3)
        .filter_map(|p| p.pose.map(|q| (p.profile.uuid, [q.x, q.y, q.z])))
        .collect();
    for (uuid, p) in players {
        let d = super::dist_sq(p, at).sqrt() / reach;
        if d > 1.0 {
            continue;
        }
        let i = 1.0 - d;
        crate::vitals::try_hurt_by_entity(
            ctx,
            uuid,
            ((i * i + i) / 2.0 * 7.0 * reach + 1.0).floor() as f32,
            "minecraft:explosion",
            "death.attack.explosion",
            None,
            None,
        );
    }
}

/// The pillar crystals may be hurt again and stop beaming.
fn reset_spike_crystals(
    ctx: &ServerContext,
    fight: &Fight,
    entities: &mut [Entity],
    fresh: &mut [Entity],
) {
    let boxes: Vec<_> = fight.spikes().iter().map(|s| spike_box(s, -64)).collect();
    for e in entities.iter_mut().chain(fresh.iter_mut()) {
        if e.removed || !boxes.iter().any(|b| inside(e, b)) {
            continue;
        }
        if let Kind::Crystal(c) = &mut e.kind {
            c.invulnerable = false;
            if c.beam.take().is_some() {
                crystal_data(ctx, e);
            }
        }
    }
}

/// A pillar during the respawn: everything within 10 blocks of its top
/// goes, a power 5 blast, and the pillar is built again with an
/// invulnerable crystal beaming at the centre.
fn rebuild_spike(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut Vec<Entity>,
    fresh: &mut Vec<Entity>,
    s: &Spike,
) {
    let air = ctx.shapes.air();
    for x in s.x - 10..=s.x + 10 {
        for y in s.height - 10..=s.height + 10 {
            for z in s.z - 10..=s.z + 10 {
                let p = Position::new(x, y, z);
                if !is_air(ctx, p) {
                    level.set_block(p, air);
                }
            }
        }
    }
    let blast = crate::explosion::Blast {
        center: [
            f64::from(s.x) + 0.5,
            f64::from(s.height),
            f64::from(s.z) + 0.5,
        ],
        power: 5.0,
        source: 0,
        owner: None,
        mob: false,
        dangerous: false,
        by: None,
        fire: false,
        respawn: None,
    };
    crate::explosion::explode_all(ctx, level, entities, &[blast]);
    build_spike(ctx, level, s);
    fresh.push(super::crystal::new(
        ctx,
        [
            f64::from(s.x) + 0.5,
            f64::from(s.height + 1),
            f64::from(s.z) + 0.5,
        ],
        true,
        Some(BEAM_CENTRE),
        true,
    ));
}

/// An obsidian pillar (with its cage), as world generation builds it.
pub fn build_spike(ctx: &ServerContext, level: &mut WorldLevel, s: &Spike) {
    let (obsidian, air) = (state(ctx, "minecraft:obsidian"), ctx.shapes.air());
    let r2 = f64::from(s.radius * s.radius + 1);
    for z in s.z - s.radius..=s.z + s.radius {
        for y in ctx.world.min_y..=s.height + 10 {
            for x in s.x - s.radius..=s.x + s.radius {
                let (dx, dz) = (f64::from(x - s.x), f64::from(z - s.z));
                let p = Position::new(x, y, z);
                if dx * dx + dz * dz <= r2 && y < s.height {
                    if !is(ctx, p, "minecraft:obsidian") {
                        level.set_block(p, obsidian);
                    }
                } else if y > 65 && !is_air(ctx, p) {
                    level.set_block(p, air);
                }
            }
        }
    }
    if s.guarded {
        let bars = state(ctx, "minecraft:iron_bars");
        let b = |v: bool| if v { "true" } else { "false" };
        for dx in -2i32..=2 {
            for dz in -2i32..=2 {
                for dy in 0..=3 {
                    let (edge_x, edge_z, top) = (dx.abs() == 2, dz.abs() == 2, dy == 3);
                    if !(edge_x || edge_z || top) {
                        continue;
                    }
                    let ns = edge_x || top;
                    let we = edge_z || top;
                    let mut st = ctx.shapes.with(bars, "north", b(ns && dz != -2));
                    st = ctx.shapes.with(st, "south", b(ns && dz != 2));
                    st = ctx.shapes.with(st, "west", b(we && dx != -2));
                    st = ctx.shapes.with(st, "east", b(we && dx != 2));
                    level.set_block(Position::new(s.x + dx, s.height + dy, s.z + dz), st);
                }
            }
        }
    }
    level.set_block(
        Position::new(s.x, s.height, s.z),
        state(ctx, "minecraft:bedrock"),
    );
    level.set_block(
        Position::new(s.x, s.height + 1, s.z),
        state(ctx, "minecraft:fire"),
    );
}

/// A crystal was destroyed in the End.
pub fn crystal_destroyed(
    ctx: &ServerContext,
    entities: &mut [Entity],
    uuid: u128,
    id: i32,
    pos: [f64; 3],
    by: Option<u128>,
) {
    if ctx.world.dimension != "minecraft:the_end" {
        return;
    }
    let mut state = lock(&ctx.bosses);
    let fight = state.fight.get_or_insert_with(|| Fight::load(ctx));
    if fight.respawn.is_some() && fight.respawn_crystals.contains(&uuid) {
        // One of the four: the respawn stops.
        fight.respawn = None;
        fight.respawn_time = 0;
        fight.respawn_crystals.clear();
        let mut none = Vec::new();
        reset_spike_crystals(ctx, fight, entities, &mut none);
        let mut level = WorldLevel::new(ctx);
        spawn_exit_portal(ctx, &mut level, fight, true);
        let changes = level.into_changes();
        if !changes.is_empty() {
            let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
        }
        fight.save();
        return;
    }
    fight.crystals_alive = count_crystals(fight, entities);
    let dragon = fight.dragon;
    drop(state);
    if let Some(e) = dragon.and_then(|u| entities.iter_mut().find(|e| e.uuid == u && !e.removed)) {
        super::dragon::crystal_destroyed(ctx, e, id, pos, by);
    }
}

/// The fight's dragon finished dying: the portal opens, a gateway comes
/// and, the first time, the egg.
pub fn dragon_killed(ctx: &ServerContext, level: &mut WorldLevel, dragon: u128) {
    if ctx.world.dimension != "minecraft:the_end" {
        return;
    }
    let mut state = lock(&ctx.bosses);
    let fight = state.fight.get_or_insert_with(|| Fight::load(ctx));
    if fight.dragon != Some(dragon) {
        return;
    }
    spawn_exit_portal(ctx, level, fight, true);
    spawn_new_gateway(ctx, level, fight);
    if !fight.previously_killed {
        let y = surface(ctx, 0, 0);
        level.set_block(Position::new(0, y, 0), state_egg(ctx));
    }
    fight.previously_killed = true;
    fight.dragon_killed = true;
    fight.save();
}

fn state_egg(ctx: &ServerContext) -> u32 {
    state(ctx, "minecraft:dragon_egg")
}

fn spawn_new_gateway(ctx: &ServerContext, level: &mut WorldLevel, fight: &mut Fight) {
    let Some(i) = fight.gateways.pop() else {
        return;
    };
    let p = gateway_pos(i);
    crate::fx::level_event(
        ctx,
        3000,
        [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])],
        0,
    );
    build_gateway(ctx, level, p, None, false);
}

/// An end gateway in its bedrock frame at `p`, leading to `exit`.
pub fn build_gateway(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    p: [i32; 3],
    exit: Option<[i32; 3]>,
    exact: bool,
) {
    let (bedrock, air) = (state(ctx, "minecraft:bedrock"), ctx.shapes.air());
    for dz in -1..=1 {
        for dy in -2..=2 {
            for dx in -1..=1 {
                let q = Position::new(p[0] + dx, p[1] + dy, p[2] + dz);
                let (on_x, on_y, on_z) = (dx == 0, dy == 0, dz == 0);
                let ends = dy == 2 || dy == -2;
                let to = if on_x && on_y && on_z {
                    state(ctx, "minecraft:end_gateway")
                } else if on_y {
                    air
                } else if (ends && on_x && on_z) || ((on_x || on_z) && !ends) {
                    bedrock
                } else {
                    air
                };
                level.set_block(q, to);
            }
        }
    }
    let mut data = Compound::new();
    data.insert("Age", Tag::Long(0));
    if let Some(e) = exit {
        data.insert("exit_portal", Tag::IntArray(e.to_vec()));
    }
    if exact {
        data.insert("ExactTeleport", Tag::Byte(1));
    }
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: p[0],
            y: p[1],
            z: p[2],
            id: "minecraft:end_gateway".into(),
            data,
        },
        &ctx.data.blocks,
    );
}

/// Players stepping into a gateway go through it (40 ticks before it
/// works for them again).
pub fn gateways(ctx: &ServerContext, level: &mut WorldLevel, now: i64) {
    let players: Vec<(u128, Pose, i32)> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension && p.game_mode != 3)
        .filter_map(|p| p.pose.map(|q| (p.profile.uuid, q, p.entity_id)))
        .collect();
    for (uuid, pose, _) in players {
        let feet = [
            pose.x.floor() as i32,
            pose.y.floor() as i32,
            pose.z.floor() as i32,
        ];
        let Some(g) = [0, 1]
            .iter()
            .map(|dy| Position::new(feet[0], feet[1] + dy, feet[2]))
            .find(|&p| is(ctx, p, "minecraft:end_gateway"))
        else {
            continue;
        };
        {
            let mut state = lock(&ctx.bosses);
            if state.cooldowns.get(&uuid).is_some_and(|&t| t > now) {
                state.cooldowns.insert(uuid, now + 40);
                continue;
            }
            state.cooldowns.insert(uuid, now + 40);
        }
        if let Some(to) = gateway_exit(ctx, level, [g.x, g.y, g.z]) {
            crate::fx::level_event(
                ctx,
                3000,
                [f64::from(g.x), f64::from(g.y), f64::from(g.z)],
                0,
            );
            let _ = ctx.events.send(Event::Teleport {
                uuid,
                pose: Pose {
                    x: to[0],
                    y: to[1],
                    z: to[2],
                    ..pose
                },
            });
        }
    }
}

/// Where a gateway sends someone: its exit (found and linked the first
/// time), then a free spot near it unless the teleport is exact.
pub fn gateway_exit(ctx: &ServerContext, level: &mut WorldLevel, g: [i32; 3]) -> Option<[f64; 3]> {
    let at = Position::new(g[0], g[1], g[2]);
    let be = ctx.world.block_entity(at, &ctx.data.blocks)?;
    let exact = matches!(be.data.get("ExactTeleport"), Some(Tag::Byte(1)));
    let exit = match block_pos(be.data.get("exit_portal")) {
        Some(e) => e,
        None => {
            let e = find_exit(ctx, level, g);
            let mut data = be.data.clone();
            data.insert("exit_portal", Tag::IntArray(e.to_vec()));
            ctx.world
                .set_block_entity(mc_world::BlockEntity { data, ..be }, &ctx.data.blocks);
            // And a gateway back, there.
            build_gateway(ctx, level, e, Some(g), true);
            e
        }
    };
    if exact {
        return Some([
            f64::from(exit[0]) + 0.5,
            f64::from(exit[1]),
            f64::from(exit[2]) + 0.5,
        ]);
    }
    let spot = landing(ctx, exit);
    Some([
        f64::from(spot[0]) + 0.5,
        f64::from(spot[1]),
        f64::from(spot[2]) + 0.5,
    ])
}

/// Whether a chunk has any block above y 15 (outer islands).
fn chunk_has_land(ctx: &ServerContext, cx: i32, cz: i32) -> bool {
    for x in cx * 16..cx * 16 + 16 {
        for z in cz * 16..cz * 16 + 16 {
            if top(ctx, x, z, true) > 15 {
                return true;
            }
        }
    }
    false
}

/// The outer-island end of a central gateway: 1024 blocks out from the
/// centre through it, moved in or out by 16 to the islands' edge, the
/// highest end stone with room above in that chunk (or a new island), then
/// the highest block near it, ten above.
fn find_exit(ctx: &ServerContext, level: &mut WorldLevel, g: [i32; 3]) -> [i32; 3] {
    let len = f64::from(g[0]).hypot(f64::from(g[2])).max(1e-9);
    let dir = [f64::from(g[0]) / len, f64::from(g[2]) / len];
    let at = |l: f64| ((dir[0] * l).floor() as i32, (dir[1] * l).floor() as i32);
    let mut l = 1024.0;
    let chunk = |l: f64| {
        let (x, z) = at(l);
        (x >> 4, z >> 4)
    };
    let (cx, cz) = chunk(l);
    if chunk_has_land(ctx, cx, cz) {
        while l > 768.0 {
            l -= 16.0;
            let (cx, cz) = chunk(l);
            if !chunk_has_land(ctx, cx, cz) {
                break;
            }
        }
    } else {
        while l < 1280.0 {
            l += 16.0;
            let (cx, cz) = chunk(l);
            if chunk_has_land(ctx, cx, cz) {
                break;
            }
        }
    }
    let (x, z) = at(l);
    let (cx, cz) = (x >> 4, z >> 4);
    // End stone at y 30 or more with two free blocks above.
    let mut best: Option<[i32; 3]> = None;
    for bx in cx * 16..cx * 16 + 16 {
        for bz in cz * 16..cz * 16 + 16 {
            for y in (30..=top(ctx, bx, bz, true)).rev() {
                let p = Position::new(bx, y, bz);
                let free = |dy: i32| {
                    let s = ctx
                        .world
                        .block(Position::new(bx, y + dy, bz), &ctx.data.blocks)
                        .unwrap_or(0);
                    !ctx.rules.blocks_motion(s)
                };
                if is(ctx, p, "minecraft:end_stone") && free(1) && free(2) {
                    let d =
                        |q: [i32; 3]| (q[0] - x).pow(2) + (q[1] - g[1]).pow(2) + (q[2] - z).pow(2);
                    if best.is_none_or(|b| d([bx, y, bz]) < d(b)) {
                        best = Some([bx, y, bz]);
                    }
                    break;
                }
            }
        }
    }
    let base = match best {
        Some(_) => [cx * 16 + 15, 75, cz * 16 + 15],
        None => {
            // A small island of end stone at y 75.
            let p = [x, 75, z];
            let stone = state(ctx, "minecraft:end_stone");
            let mut f = 4.0f64 + f64::from(super::below(3));
            let mut dy = 0;
            while f > 0.5 {
                let r = f.ceil() as i32;
                for dx in -r..=r {
                    for dz in -r..=r {
                        if f64::from(dx * dx + dz * dz) <= (f + 1.0) * (f + 1.0) {
                            level.set_block(Position::new(p[0] + dx, p[1] + dy, p[2] + dz), stone);
                        }
                    }
                }
                f -= f64::from(super::below(2)) + 0.5;
                dy -= 1;
            }
            p
        }
    };
    // The highest full block within 16, north and west first.
    let mut high = base;
    let mut high_y = i32::MIN;
    for dz in -16..=16 {
        for dx in -16..=16 {
            let (bx, bz) = (base[0] + dx, base[2] + dz);
            let y = top(ctx, bx, bz, true);
            if y > high_y {
                high_y = y;
                high = [bx, y, bz];
            }
        }
    }
    if high_y == i32::MIN || high_y < ctx.world.min_y {
        high = base;
    }
    [high[0], high[1] + 10, high[2]]
}

/// A spot to stand near a gateway's exit: from the top down within five
/// blocks, a block with two free above that is not right by the gateway.
fn landing(ctx: &ServerContext, e: [i32; 3]) -> [i32; 3] {
    let blocks = &ctx.data.blocks;
    let top_y = (ctx.world.min_y + ctx.world.height as i32 - 1).min(255);
    for y in (ctx.world.min_y..=top_y).rev() {
        for dx in -5i32..=5 {
            for dz in -5i32..=5 {
                // Not the gateway's own column, nor right by it.
                if dx == 0 && dz == 0 || dx.abs() <= 1 && dz.abs() <= 1 && (y - e[1]).abs() <= 1 {
                    continue;
                }
                let (x, z) = (e[0] + dx, e[2] + dz);
                let s = |dy: i32| {
                    ctx.world
                        .block(Position::new(x, y + dy, z), blocks)
                        .unwrap_or(0)
                };
                if ctx.rules.blocks_motion(s(0))
                    && !ctx.rules.blocks_motion(s(1))
                    && !ctx.rules.blocks_motion(s(2))
                {
                    return [x, y + 1, z];
                }
            }
        }
    }
    [e[0], e[1] + 2, e[2]]
}
