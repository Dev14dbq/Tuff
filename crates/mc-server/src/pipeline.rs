//! Chunk generation in stages, as the game does it: terrain for a chunk and
//! its neighbours, features decorated over 3×3 chunk regions (so a tree at
//! an edge reaches into the next chunk), then the finished chunk.
//!
//! Before a chunk goes out it is settled, which the game leaves for later:
//! sand and gravel with nothing under them fall at once, and water and lava
//! that would flow (aquifer edges, springs, lakes) flow until they rest.
//! Flow may run into the neighbouring chunks that are still being
//! generated; where it reaches past them it continues when that chunk is
//! finished, from the fluid at its border; into chunks players already have
//! it goes as ordinary fluid ticks.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use mc_features::structure::{Beard, Start};
use mc_features::{Facts, Features, Heightmap, Heights, P, Region};
use mc_tick::{BlockPos, Dir, TickQueue};
use mc_world::{BlockEntity, Chunk, Section};
use mc_worldgen::density::Fast;
use mc_worldgen::generator::NoiseGenerator;
use mc_worldgen::materials::{zoom_seed, zoomed_cell};

/// A chunk being generated.
#[derive(Clone)]
struct Proto {
    chunk: Chunk,
    /// First free y per column, per [`Heightmap::ALL`] entry. Until the
    /// chunk is decorated only the world generation heightmaps follow
    /// changes; the others are taken from the blocks when its decoration
    /// starts and follow changes from then on, as the game's chunk
    /// statuses keep them.
    heights: Vec<[i32; 256]>,
    final_primed: bool,
    block_entities: HashMap<P, mc_nbt::Compound>,
    /// Asked for by features (springs, lakes): ticked when finished.
    ticks: Vec<P>,
    decorated: bool,
    /// A copy of a chunk that is already finished: read, never written.
    readonly: bool,
    /// Changed since it was last saved (only decorated chunks are saved).
    dirty: bool,
}

pub struct Pipeline {
    pub generator: NoiseGenerator,
    pub features: Features,
    seed: i64,
    zoom: i64,
    heights: Heights,
    air: u32,
    biome_fallback: u32,
    protos: Mutex<HashMap<(i32, i32), Proto, Fast>>,
    finished: Mutex<HashSet<(i32, i32)>>,
    /// Bare terrain, made once and kept while a chunk near it may still
    /// need it; a chunk asked for twice at once is made by one thread while
    /// the other waits.
    terrain: Mutex<HashMap<(i32, i32), TerrainCell, Fast>>,
    /// Structures starting in each chunk looked at so far.
    starts: Mutex<StartMap>,
    /// Chunks some thread is decorating or settling around: finishing
    /// chunks whose 5×5 areas do not overlap goes on at the same time.
    claims: crate::util::Claims,
    /// Stronghold rings, placed once before any start is looked for.
    rings: OnceLock<()>,
    /// Lava settles as in the Nether.
    fast_lava: bool,
    /// Entities features made (end crystals), waiting for the world.
    entities: Mutex<Vec<mc_nbt::Compound>>,
}

/// A chunk's bare terrain, made once.
type TerrainCell = Arc<OnceLock<Proto>>;

/// The structures starting in one chunk, worked out once by whichever
/// thread asks first; the others wait.
type StartCell = Arc<OnceLock<Vec<Arc<Start>>>>;

/// Structures starting in each chunk.
type StartMap = HashMap<(i32, i32), StartCell, Fast>;

/// The ground as the density alone gives it, for starting structures.
struct Columns<'a> {
    sampler: mc_worldgen::generator::ColumnSampler<'a>,
}

impl mc_features::structure::Ground for Columns<'_> {
    fn height(&mut self, x: i32, z: i32, ocean_floor: bool) -> i32 {
        self.sampler.height(x, z, ocean_floor)
    }

    fn biome(&mut self, qx: i32, qy: i32, qz: i32) -> u32 {
        self.sampler.biome(qx, qy, qz)
    }

    fn occupied(&mut self, x: i32, y: i32, z: i32, ocean_floor: bool) -> bool {
        self.sampler.occupied(x, y, z, ocean_floor)
    }
}

/// How far (in chunks) a structure's pieces may reach from its start.
const STRUCTURE_REACH: i32 = 8;

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("seed", &self.seed)
            .finish_non_exhaustive()
    }
}

/// What the world says about chunks outside the pipeline.
pub trait Finished {
    /// Whether the chunk is already finished (saved or loaded).
    fn is_finished(&self, x: i32, z: i32) -> bool;
    /// A block of a finished chunk that is loaded.
    fn block(&self, p: BlockPos) -> Option<u32>;
    /// A chunk saved while still being generated (decorated).
    fn stored_proto(&self, x: i32, z: i32) -> Option<Chunk>;
}

/// A finished chunk and the fluid ticks it leaves for chunks players have.
pub struct Done {
    pub chunk: Box<Chunk>,
    pub live_ticks: Vec<(BlockPos, u32)>,
}

impl Pipeline {
    pub fn new(
        generator: NoiseGenerator,
        features: Features,
        seed: i64,
        air: u32,
        biome_fallback: u32,
    ) -> Self {
        let heights = Heights {
            min_y: generator.min_y,
            height: generator.height,
            sea_level: generator.sea_level,
        };
        Self {
            generator,
            features,
            seed,
            zoom: zoom_seed(seed),
            heights,
            air,
            biome_fallback,
            protos: Mutex::new(HashMap::default()),
            finished: Mutex::new(HashSet::new()),
            terrain: Mutex::new(HashMap::default()),
            starts: Mutex::new(HashMap::default()),
            rings: OnceLock::new(),
            claims: crate::util::Claims::default(),
            fast_lava: false,
            entities: Mutex::new(Vec::new()),
        }
    }

    /// Entities features made since the last call.
    pub fn take_entities(&self) -> Vec<mc_nbt::Compound> {
        std::mem::take(&mut *self.entities.lock().expect("generated entities lock"))
    }

    /// Lava flows fast, far and thin (the dimension's `gameplay/fast_lava`).
    pub fn with_fast_lava(mut self, fast: bool) -> Self {
        self.fast_lava = fast;
        self
    }

    /// Works out which structures start in the chunks around `x, z` (within
    /// `radius`) that were not looked at yet.
    /// Places the stronghold rings, as the game does when the world loads;
    /// structure starts wait for it.
    pub fn prepare(&self) {
        self.ensure_rings();
    }

    fn ensure_rings(&self) {
        self.rings.get_or_init(|| {
            let biome = |qx, qy, qz| self.generator.noise_biome(qx, qy, qz);
            self.features.structures.prepare_rings(self.seed, &biome);
        });
    }

    /// The structure starts of the chunks within `radius` of chunk `x, z`,
    /// row by row.
    fn starts_in(&self, x: i32, z: i32, radius: i32) -> Vec<((i32, i32), StartCell)> {
        let cells: Vec<_> = {
            let mut starts = self.starts.lock().expect("starts lock");
            (z - radius..=z + radius)
                .flat_map(|cz| (x - radius..=x + radius).map(move |cx| (cx, cz)))
                .map(|p| (p, starts.entry(p).or_default().clone()))
                .collect()
        };
        // Threads asking for overlapping areas start at different places so
        // they share the work rather than wait on each other.
        let n = cells.len();
        let first = (x.wrapping_mul(31).wrapping_add(z) as u32 as usize) % n;
        let mut ground: Option<Columns> = None;
        for i in 0..n {
            let ((cx, cz), cell) = &cells[(first + i) % n];
            if cell.get().is_some() {
                continue;
            }
            cell.get_or_init(|| {
                let g = ground.get_or_insert_with(|| {
                    self.ensure_rings();
                    Columns {
                        sampler: self.generator.columns(),
                    }
                });
                self.features
                    .structures
                    .starts(self.seed, *cx, *cz, self.heights, g)
            });
        }
        cells
    }

    /// Structures that start within `radius` chunks of chunk `x, z`
    /// (name, start chunk, centre block), nearest first.
    pub fn starts_within(
        &self,
        x: i32,
        z: i32,
        radius: i32,
        name: Option<&str>,
    ) -> Vec<(String, (i32, i32), P)> {
        let mut out = Vec::new();
        for ((cx, cz), cell) in self.starts_in(x, z, radius) {
            {
                for s in cell.get().into_iter().flatten() {
                    let id = &self.features.structures.structures[s.structure].id;
                    if name.is_some_and(|n| n != id && Some(n) != id.strip_prefix("minecraft:")) {
                        continue;
                    }
                    out.push((id.clone(), (cx, cz), s.bbox.center()));
                }
            }
        }
        out.sort_by_key(|(_, (cx, cz), _)| (cx - x).pow(2) + (cz - z).pow(2));
        out
    }

    /// `/locate structure`: the nearest of the structures named `wanted`
    /// from block `pos`, as vanilla searches (see
    /// [`mc_features::structure::Structures::locate`]): its name and the
    /// block shown.
    pub fn locate(
        &self,
        wanted: &[String],
        pos: (i32, i32, i32),
        radius: i32,
    ) -> Option<(String, (i32, i32, i32))> {
        let structures = &self.features.structures;
        let wanted: HashSet<usize> = structures
            .structures
            .iter()
            .enumerate()
            .filter(|(_, s)| wanted.contains(&s.id))
            .map(|(i, _)| i)
            .collect();
        self.ensure_rings();
        let mut ground = Columns {
            sampler: self.generator.columns(),
        };
        // Starts already worked out are used; others only as far as vanilla
        // looks (where the start piece would go).
        let mut starts = |x: i32, z: i32| {
            let known = self
                .starts
                .lock()
                .expect("starts lock")
                .get(&(x, z))
                .and_then(|cell| cell.get().cloned());
            known.unwrap_or_else(|| {
                structures.start_pieces(self.seed, x, z, self.heights, &mut ground, &wanted)
            })
        };
        let (i, at) = structures.locate(self.seed, &wanted, pos, radius, &mut starts)?;
        Some((structures.structures[i].id.clone(), at))
    }

    /// Structures whose pieces may reach chunk `x, z`.
    fn starts_near(&self, x: i32, z: i32) -> Vec<Arc<Start>> {
        let mut out = Vec::new();
        for (_, cell) in self.starts_in(x, z, STRUCTURE_REACH) {
            out.extend(cell.get().into_iter().flatten().cloned());
        }
        out
    }

    pub fn facts(&self) -> &Facts {
        &self.features.facts
    }

    fn terrain(&self, x: i32, z: i32, near: &[Arc<Start>]) -> Proto {
        let beard = Beard::new(&self.features.structures, near, x, z);
        let made = if beard.is_empty() {
            self.generator.generate(x, z)
        } else {
            self.generator
                .generate_with(x, z, &|bx, by, bz| beard.compute(bx, by, bz))
        };
        let h = self.heights;
        let mut chunk = Chunk::new(
            x,
            z,
            h.min_y,
            h.height as u32,
            self.air,
            self.biome_fallback,
        );
        for (i, section) in chunk.sections.iter_mut().enumerate() {
            *section = Section::from_parts(
                made.blocks[i * 4096..(i + 1) * 4096].to_vec(),
                made.biomes[i * 64..(i + 1) * 64].to_vec(),
            );
        }
        let facts = self.facts();
        let mut heights = vec![[h.min_y; 256]; Heightmap::ALL.len()];
        for (m, map) in Heightmap::ALL.iter().enumerate() {
            for lz in 0..16 {
                for lx in 0..16 {
                    let mut y = h.min_y + h.height - 1;
                    let top = loop {
                        if y < h.min_y {
                            break h.min_y;
                        }
                        let s = made.blocks[(((y - h.min_y) * 16 + lz) * 16 + lx) as usize];
                        if map.counts(facts, s) {
                            break y + 1;
                        }
                        y -= 1;
                    };
                    heights[m][(lz * 16 + lx) as usize] = top;
                }
            }
        }
        Proto {
            chunk,
            heights,
            final_primed: false,
            block_entities: HashMap::new(),
            ticks: Vec::new(),
            decorated: false,
            readonly: false,
            dirty: false,
        }
    }

    /// A chunk saved while still being generated: decorated already.
    fn stored(&self, mut chunk: Chunk) -> Proto {
        let facts = self.facts();
        let block_entities = std::mem::take(&mut chunk.block_entities)
            .into_iter()
            .map(|be| {
                let mut nbt = be.data;
                nbt.insert("id", mc_nbt::Tag::String(be.id));
                (P::new(be.x, be.y, be.z), nbt)
            })
            .collect();
        let mut p = Proto {
            chunk,
            heights: vec![[self.heights.min_y; 256]; Heightmap::ALL.len()],
            final_primed: true,
            block_entities,
            ticks: Vec::new(),
            decorated: true,
            readonly: false,
            dirty: false,
        };
        for (m, map) in Heightmap::ALL.iter().enumerate() {
            for lz in 0..16 {
                for lx in 0..16 {
                    p.heights[m][lz * 16 + lx] = p.scan(facts, *map, lx, lz);
                }
            }
        }
        p
    }

    /// Chunks decorated but not finished that changed since the last call,
    /// to be saved as `minecraft:features` (so a restart loses nothing).
    pub fn unsaved(&self) -> Vec<Chunk> {
        let mut protos = self.protos.lock().expect("protos lock");
        protos
            .values_mut()
            .filter(|p| p.decorated && !p.readonly && p.dirty)
            .map(|p| {
                p.dirty = false;
                let mut c = p.chunk.clone();
                c.block_entities = p
                    .block_entities
                    .iter()
                    .filter_map(|(pos, nbt)| {
                        let mut data = nbt.clone();
                        let id = match data.remove("id") {
                            Some(mc_nbt::Tag::String(id)) => id,
                            _ => return None,
                        };
                        Some(BlockEntity {
                            x: pos.x,
                            y: pos.y,
                            z: pos.z,
                            id,
                            data,
                        })
                    })
                    .collect();
                c
            })
            .collect()
    }

    /// Generates chunk `x, z` with everything around it it needs.
    pub fn finish(&self, x: i32, z: i32, world: &dyn Finished) -> Done {
        // Terrain for the 5×5 around: decorating a neighbour reads its own
        // neighbours.
        let area: Vec<(i32, i32)> = (-2..=2)
            .flat_map(|dz| (-2..=2).map(move |dx| (x + dx, z + dz)))
            .collect();
        // Whether a chunk is finished already (asked without holding our
        // locks: the world has its own).
        let finals = |list: &[(i32, i32)]| -> Vec<bool> {
            let known: Vec<bool> = {
                let finished = self.finished.lock().expect("finished lock");
                list.iter().map(|p| finished.contains(p)).collect()
            };
            list.iter()
                .zip(known)
                .map(|(&p, known)| p != (x, z) && (known || world.is_finished(p.0, p.1)))
                .collect()
        };
        let absent: Vec<(i32, i32)> = {
            let protos = self.protos.lock().expect("protos lock");
            area.iter()
                .copied()
                .filter(|p| !protos.contains_key(p))
                .collect()
        };
        // Chunks saved half way come back as they were; the rest is made.
        // Terrain needs nothing shared: it is made without holding the
        // locks so other chunks can be finished meanwhile.
        let gather = |list: &[(i32, i32)]| -> Vec<((i32, i32), Proto)> {
            let mut missing = Vec::new();
            let mut got = Vec::new();
            for (p, is_final) in list.iter().zip(finals(list)) {
                match (!is_final).then(|| world.stored_proto(p.0, p.1)).flatten() {
                    Some(chunk) => got.push((*p, self.stored(chunk))),
                    None => missing.push(*p),
                }
            }
            got.extend(self.terrain_parallel(&missing));
            got
        };
        let mut made = gather(&absent);
        let _claim = self.claims.claim(area.clone());
        // Nothing in the area gets finished while we hold it. What was there
        // when we looked may have been finished and let go since (the same
        // chunk asked for twice at once): that is made again now.
        let mut protos: HashMap<(i32, i32), Proto, Fast> = loop {
            let made_pos: Vec<(i32, i32)> = made.iter().map(|(p, _)| *p).collect();
            let made_final = finals(&made_pos);
            let mut shared = self.protos.lock().expect("protos lock");
            let finished = self.finished.lock().expect("finished lock");
            for ((pos, mut proto), is_final) in made.into_iter().zip(made_final) {
                // Someone else may have made (and since changed) it meanwhile.
                if shared.contains_key(&pos) {
                    continue;
                }
                proto.readonly = is_final || finished.contains(&pos) && pos != (x, z);
                shared.insert(pos, proto);
            }
            let late: Vec<(i32, i32)> = area
                .iter()
                .copied()
                .filter(|p| !shared.contains_key(p))
                .collect();
            if late.is_empty() {
                // The area is ours: take its chunks out to work on them
                // unlocked.
                break area
                    .iter()
                    .filter_map(|p| shared.remove(p).map(|proto| (*p, proto)))
                    .collect();
            }
            drop(finished);
            drop(shared);
            made = gather(&late);
        };
        // Features for the 3×3.
        for dz in -1..=1 {
            for dx in -1..=1 {
                let pos = (x + dx, z + dz);
                let Some(proto) = protos.get(&pos) else {
                    continue;
                };
                if proto.decorated || proto.readonly {
                    continue;
                }
                // The biomes of the chunk and its neighbours, as the game
                // gathers them: a position near the edge may read a
                // neighbour's biome.
                let mut biomes = BTreeSet::new();
                for nz in -1..=1 {
                    for nx in -1..=1 {
                        if let Some(n) = protos.get(&(pos.0 + nx, pos.1 + nz)) {
                            biomes.extend(chunk_biomes(&n.chunk));
                        }
                    }
                }
                if let Some(p) = protos.get_mut(&pos) {
                    p.prime_final(self.facts());
                }
                let near = self.starts_near(pos.0, pos.1);
                let mut region = Gen {
                    protos: &mut protos,
                    center: pos,
                    pipeline: self,
                };
                self.features
                    .decorate(&mut region, pos.0, pos.1, &biomes, &near);
                if let Some(p) = protos.get_mut(&pos) {
                    p.decorated = true;
                    p.dirty = true;
                }
            }
        }
        // Settle: falling blocks, then fluids, then whatever fell into them.
        let facts = self.facts();
        settle_falling(facts, protos.get_mut(&(x, z)).expect("center generated"));
        let live_ticks = {
            let mut sim = Sim::new(&mut protos, (x, z), self, world);
            sim.seed_ticks();
            sim.run();
            sim.live
        };
        let mut proto = protos.remove(&(x, z)).expect("center generated");
        settle_falling(facts, &mut proto);
        bubble_columns(facts, &mut proto);
        {
            // Same order as everywhere: protos, then finished.
            let mut shared = self.protos.lock().expect("protos lock");
            let mut finished = self.finished.lock().expect("finished lock");
            for (pos, proto) in &protos {
                if proto.readonly {
                    finished.insert(*pos);
                }
            }
            finished.insert((x, z));
            // Terrain no unfinished chunk can ask for any more.
            let mut terrain = self.terrain.lock().expect("terrain lock");
            for dz in -2..=2 {
                for dx in -2..=2 {
                    let (px, pz) = (x + dx, z + dz);
                    if (-2..=2).all(|nz| (-2..=2).all(|nx| finished.contains(&(px + nx, pz + nz))))
                    {
                        terrain.remove(&(px, pz));
                    }
                }
            }
            drop(terrain);
            // Back with the others, but for copies nobody can need any more:
            // every neighbour is finished.
            for (pos, p) in protos {
                let done = p.readonly
                    && (-1..=1)
                        .all(|dz| (-1..=1).all(|dx| finished.contains(&(pos.0 + dx, pos.1 + dz))));
                if !done {
                    shared.insert(pos, p);
                }
            }
        }
        let mut chunk = proto.chunk;
        chunk.block_entities = proto
            .block_entities
            .into_iter()
            .filter_map(|(p, mut nbt)| {
                let id = match nbt.remove("id") {
                    Some(mc_nbt::Tag::String(id)) => id,
                    _ => return None,
                };
                Some(BlockEntity {
                    x: p.x,
                    y: p.y,
                    z: p.z,
                    id,
                    data: nbt,
                })
            })
            .collect();
        Done {
            chunk: Box::new(chunk),
            live_ticks,
        }
    }

    fn terrain_parallel(&self, list: &[(i32, i32)]) -> Vec<((i32, i32), Proto)> {
        let cells: Vec<TerrainCell> = {
            let mut terrain = self.terrain.lock().expect("terrain lock");
            list.iter()
                .map(|p| terrain.entry(*p).or_default().clone())
                .collect()
        };
        let make = |i: usize| {
            let (x, z) = list[i];
            cells[i]
                .get_or_init(|| {
                    let near = self.starts_near(x, z);
                    self.terrain(x, z, &near)
                })
                .clone()
        };
        let todo = cells.iter().filter(|c| c.get().is_none()).count();
        if todo <= 1 {
            return (0..list.len()).map(|i| (list[i], make(i))).collect();
        }
        let threads = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .min(todo);
        let next = std::sync::atomic::AtomicUsize::new(0);
        let out = Mutex::new(Vec::with_capacity(list.len()));
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= list.len() {
                            break;
                        }
                        let proto = make(i);
                        out.lock().expect("terrain lock").push((list[i], proto));
                    }
                });
            }
        });
        out.into_inner().expect("terrain lock")
    }
}

fn chunk_biomes(chunk: &Chunk) -> BTreeSet<u32> {
    let mut set = BTreeSet::new();
    for s in &chunk.sections {
        for i in 0..64 {
            set.insert(s.biomes.get(i));
        }
    }
    set
}

fn local(p: P) -> (usize, usize) {
    ((p.x & 15) as usize, (p.z & 15) as usize)
}

fn is_worldgen(map: Heightmap) -> bool {
    matches!(map, Heightmap::WorldSurfaceWg | Heightmap::OceanFloorWg)
}

impl Proto {
    /// The top of a column for a heightmap, from the blocks.
    fn scan(&self, facts: &Facts, map: Heightmap, lx: usize, lz: usize) -> i32 {
        let min_y = self.chunk.min_y;
        let mut y = min_y + self.chunk.height() as i32 - 1;
        while y >= min_y {
            if map.counts(facts, self.chunk.block(lx, y, lz)) {
                return y + 1;
            }
            y -= 1;
        }
        min_y
    }

    /// Takes the final heightmaps from the blocks (decoration starts).
    fn prime_final(&mut self, facts: &Facts) {
        for (m, map) in Heightmap::ALL.iter().enumerate() {
            if is_worldgen(*map) {
                continue;
            }
            for lz in 0..16 {
                for lx in 0..16 {
                    self.heights[m][lz * 16 + lx] = self.scan(facts, *map, lx, lz);
                }
            }
        }
        self.final_primed = true;
    }

    fn height(&self, facts: &Facts, map: Heightmap, lx: usize, lz: usize) -> i32 {
        let m = Heightmap::ALL.iter().position(|h| *h == map).unwrap_or(0);
        if !is_worldgen(map) && !self.final_primed {
            return self.scan(facts, map, lx, lz);
        }
        self.heights[m][lz * 16 + lx]
    }

    fn block(&self, p: P) -> u32 {
        let (lx, lz) = local(p);
        self.chunk.block(lx, p.y, lz)
    }

    fn set(&mut self, facts: &Facts, p: P, state: u32) -> u32 {
        let (lx, lz) = local(p);
        let old = self.chunk.set_block(lx, p.y, lz, state);
        if old == state {
            return old;
        }
        self.dirty = true;
        if facts.block(old) != facts.block(state) {
            self.block_entities.remove(&p);
        }
        let col = lz * 16 + lx;
        let min_y = self.chunk.min_y;
        for (m, map) in Heightmap::ALL.iter().enumerate() {
            if !is_worldgen(*map) && !self.final_primed {
                continue;
            }
            let top = self.heights[m][col];
            if map.counts(facts, state) {
                if p.y >= top {
                    self.heights[m][col] = p.y + 1;
                }
            } else if p.y == top - 1 {
                let mut y = p.y - 1;
                let new = loop {
                    if y < min_y {
                        break min_y;
                    }
                    if map.counts(facts, self.chunk.block(lx, y, lz)) {
                        break y + 1;
                    }
                    y -= 1;
                };
                self.heights[m][col] = new;
            }
        }
        old
    }
}

/// The region a chunk's features see.
struct Gen<'a> {
    protos: &'a mut HashMap<(i32, i32), Proto, Fast>,
    center: (i32, i32),
    pipeline: &'a Pipeline,
}

fn chunk_of(p: P) -> (i32, i32) {
    (p.x >> 4, p.z >> 4)
}

impl Gen<'_> {
    fn in_y(&self, y: i32) -> bool {
        let h = self.pipeline.heights;
        y >= h.min_y && y < h.min_y + h.height
    }

    fn writable(&self, c: (i32, i32)) -> bool {
        (c.0 - self.center.0).abs() <= 1 && (c.1 - self.center.1).abs() <= 1
    }
}

impl Region for Gen<'_> {
    fn block(&self, p: P) -> u32 {
        if !self.in_y(p.y) {
            return self.pipeline.air;
        }
        self.protos
            .get(&chunk_of(p))
            .map_or(self.pipeline.air, |c| c.block(p))
    }

    fn set(&mut self, p: P, state: u32) -> bool {
        let c = chunk_of(p);
        if !self.in_y(p.y) || !self.writable(c) {
            return false;
        }
        let facts = &self.pipeline.features.facts;
        match self.protos.get_mut(&c) {
            Some(proto) if !proto.readonly => {
                proto.set(facts, p, state);
                true
            }
            _ => false,
        }
    }

    fn height(&self, map: Heightmap, x: i32, z: i32) -> i32 {
        match self.protos.get(&(x >> 4, z >> 4)) {
            Some(c) => c.height(
                &self.pipeline.features.facts,
                map,
                (x & 15) as usize,
                (z & 15) as usize,
            ),
            None => self.pipeline.heights.min_y,
        }
    }

    fn biome(&self, p: P) -> u32 {
        let (qx, qy, qz) = zoomed_cell(self.pipeline.zoom, p.x, p.y, p.z);
        let h = self.pipeline.heights;
        let qy = qy.clamp(h.min_y >> 2, ((h.min_y + h.height) >> 2) - 1);
        if let Some(c) = self.protos.get(&(qx >> 2, qz >> 2)) {
            let section = ((qy << 2) - h.min_y) >> 4;
            if let Some(s) = c.chunk.sections.get(section as usize) {
                let i = (((qy & 3) << 4) | ((qz & 3) << 2) | (qx & 3)) as usize;
                return s.biomes.get(i);
            }
        }
        self.pipeline.generator.noise_biome(qx, qy, qz)
    }

    fn heights(&self) -> Heights {
        self.pipeline.heights
    }

    fn set_block_entity(&mut self, p: P, nbt: mc_nbt::Compound) {
        let c = chunk_of(p);
        if !self.writable(c) {
            return;
        }
        if let Some(proto) = self.protos.get_mut(&c)
            && !proto.readonly
        {
            proto.block_entities.insert(p, nbt);
        }
    }

    fn schedule_tick(&mut self, p: P) {
        if let Some(proto) = self.protos.get_mut(&chunk_of(p))
            && !proto.readonly
        {
            proto.ticks.push(p);
        }
    }

    fn seed(&self) -> i64 {
        self.pipeline.seed
    }

    fn add_entity(&mut self, nbt: mc_nbt::Compound) {
        self.pipeline
            .entities
            .lock()
            .expect("generated entities lock")
            .push(nbt);
    }
}

/// Drops every block that falls (sand, gravel...) onto what is below it.
fn settle_falling(facts: &Facts, proto: &mut Proto) {
    let ctx = mc_tick::Ctx {
        shapes: &facts.shapes,
        facts: &facts.rules,
    };
    let min = proto.chunk.min_y;
    let top = min + proto.chunk.height() as i32;
    for lz in 0..16 {
        for lx in 0..16 {
            let x = proto.chunk.x * 16 + lx;
            let z = proto.chunk.z * 16 + lz;
            for y in min + 1..top {
                let p = P::new(x, y, z);
                let s = proto.block(p);
                if !mc_tick::gravity::falls(ctx, s) {
                    continue;
                }
                let mut rest = y;
                while rest > min && mc_tick::gravity::free(ctx, proto.block(p.with_y(rest - 1))) {
                    rest -= 1;
                }
                if rest != y {
                    proto.set(facts, p, facts.air);
                    proto.set(facts, p.with_y(rest), s);
                }
            }
        }
    }
}

/// Water over magma pulls down and over soul sand pushes up: bubble
/// columns up to the water's surface.
fn bubble_columns(facts: &Facts, proto: &mut Proto) {
    let (Some(column), Some(magma), Some(soul_sand)) = (
        facts.default_state("minecraft:bubble_column"),
        facts.block_id("minecraft:magma_block"),
        facts.block_id("minecraft:soul_sand"),
    ) else {
        return;
    };
    let min = proto.chunk.min_y;
    let top = min + proto.chunk.height() as i32;
    for lz in 0..16 {
        for lx in 0..16 {
            let x = proto.chunk.x * 16 + lx;
            let z = proto.chunk.z * 16 + lz;
            for y in min..top - 1 {
                let p = P::new(x, y, z);
                let b = facts.block(proto.block(p));
                if b != magma && b != soul_sand {
                    continue;
                }
                let drag = if b == magma { "true" } else { "false" };
                let state = facts.with(column, "drag", drag);
                let mut q = p.above(1);
                while q.y < top
                    && facts.is(proto.block(q), "minecraft:water")
                    && facts.property(proto.block(q), "level") == Some("0")
                {
                    proto.set(facts, q, state);
                    q = q.above(1);
                }
            }
        }
    }
}

/// Fluids flowing to rest, as scheduled ticks run instantly.
struct Sim<'a> {
    protos: &'a mut HashMap<(i32, i32), Proto, Fast>,
    center: (i32, i32),
    pipeline: &'a Pipeline,
    world: &'a dyn Finished,
    time: i64,
    fluids: TickQueue,
    blocks: TickQueue,
    live: Vec<(BlockPos, u32)>,
}

const SIM_TICKS: i64 = 6000;
const SIM_STEPS: usize = 400_000;

impl<'a> Sim<'a> {
    fn new(
        protos: &'a mut HashMap<(i32, i32), Proto, Fast>,
        center: (i32, i32),
        pipeline: &'a Pipeline,
        world: &'a dyn Finished,
    ) -> Self {
        Self {
            protos,
            center,
            pipeline,
            world,
            time: 0,
            fluids: TickQueue::default(),
            blocks: TickQueue::default(),
            live: Vec::new(),
        }
    }

    fn ctx(&self) -> mc_tick::Ctx<'a> {
        let facts = &self.pipeline.features.facts;
        mc_tick::Ctx {
            shapes: &facts.shapes,
            facts: &facts.rules,
        }
    }

    fn writable(&self, p: BlockPos) -> bool {
        let c = (p.x >> 4, p.z >> 4);
        (c.0 - self.center.0).abs() <= 1
            && (c.1 - self.center.1).abs() <= 1
            && self.protos.get(&c).is_some_and(|p| !p.readonly)
    }

    /// Fluids of the chunk that would move, its features' requests, and
    /// fluid at the borders of its neighbours (flow that stopped there when
    /// this chunk was not ready).
    fn seed_ticks(&mut self) {
        let ctx = self.ctx();
        let (cx, cz) = self.center;
        let h = self.pipeline.heights;
        let mut seeds: Vec<BlockPos> = Vec::new();
        if let Some(proto) = self.protos.get_mut(&self.center) {
            seeds.extend(proto.ticks.drain(..).map(|p| BlockPos::new(p.x, p.y, p.z)));
        }
        let unstable = |level: &dyn mc_tick::Level, p: BlockPos| -> bool {
            let s = level.block(p);
            let Some((fluid, lvl)) = mc_tick::fluids::fluid_of(ctx, s) else {
                return false;
            };
            if lvl != 0 {
                return true;
            }
            [Dir::Down, Dir::North, Dir::South, Dir::West, Dir::East]
                .into_iter()
                .any(|d| {
                    let n = level.block(p.offset(d));
                    match mc_tick::fluids::fluid_of(ctx, n) {
                        Some((f, l)) => f != fluid || l != 0 && d == Dir::Down,
                        None => {
                            ctx.shapes.is_air(n)
                                || !ctx.facts.blocks_motion(n) && level_can_flow(ctx, n)
                        }
                    }
                })
        };
        for y in h.min_y..h.min_y + h.height {
            for lz in -1..17 {
                for lx in -1..17 {
                    let border = !(0..16).contains(&lx) || !(0..16).contains(&lz);
                    if border && (lx == -1 || lx == 16) && (lz == -1 || lz == 16) {
                        continue;
                    }
                    let p = BlockPos::new(cx * 16 + lx, y, cz * 16 + lz);
                    if unstable(self, p) {
                        seeds.push(p);
                    }
                }
            }
        }
        for p in seeds {
            if let Some((fluid, _)) = mc_tick::fluids::fluid_of(ctx, mc_tick::Level::block(self, p))
            {
                self.fluids.schedule(p, fluid as u32, 0, 0);
            }
        }
    }

    fn run(&mut self) {
        let ctx = self.ctx();
        let mut steps = 0;
        while self.time < SIM_TICKS && (!self.fluids.is_empty() || !self.blocks.is_empty()) {
            for (p, f) in self.fluids.take_due(self.time) {
                mc_tick::fluids::tick(self, ctx, p, f);
                steps += 1;
            }
            for (p, b) in self.blocks.take_due(self.time) {
                mc_tick::block_tick(self, ctx, p, b);
                steps += 1;
            }
            if steps > SIM_STEPS {
                break;
            }
            self.time += 1;
        }
    }
}

/// Fluid may flow into a passable block that does not hold water itself.
fn level_can_flow(ctx: mc_tick::Ctx, s: u32) -> bool {
    ctx.shapes.property(s, "waterlogged") != Some("true")
        && !matches!(
            ctx.shapes.block_name(s),
            Some(
                "minecraft:seagrass"
                    | "minecraft:tall_seagrass"
                    | "minecraft:kelp"
                    | "minecraft:kelp_plant"
                    | "minecraft:bubble_column"
                    | "minecraft:sugar_cane"
            )
        )
}

impl mc_tick::Level for Sim<'_> {
    fn block(&self, p: BlockPos) -> u32 {
        let h = self.pipeline.heights;
        if p.y < h.min_y || p.y >= h.min_y + h.height {
            return self.pipeline.air;
        }
        let c = (p.x >> 4, p.z >> 4);
        if let Some(proto) = self.protos.get(&c) {
            return proto.block(P::new(p.x, p.y, p.z));
        }
        // Finished chunks block flow unless they are loaded.
        self.world
            .block(p)
            .unwrap_or(self.pipeline.features.facts.stone)
    }

    fn set(&mut self, p: BlockPos, state: u32) -> bool {
        if !self.writable(p) || !self.in_world(p) {
            return false;
        }
        let facts = &self.pipeline.features.facts;
        let c = (p.x >> 4, p.z >> 4);
        if let Some(proto) = self.protos.get_mut(&c) {
            proto.set(facts, P::new(p.x, p.y, p.z), state);
            return true;
        }
        false
    }

    fn game_time(&self) -> i64 {
        self.time
    }

    fn schedule_fluid_tick(&mut self, p: BlockPos, fluid: u32, delay: i64) {
        if self.writable(p) {
            self.fluids.schedule(p, fluid, self.time + delay, 0);
            return;
        }
        let c = (p.x >> 4, p.z >> 4);
        match self.protos.get_mut(&c) {
            // Later, when that chunk is finished (border fluid is looked
            // at again then anyway).
            Some(proto) if !proto.readonly => proto.ticks.push(P::new(p.x, p.y, p.z)),
            Some(_) | None => {
                if self.world.block(p).is_some() {
                    self.live.push((p, fluid));
                }
            }
        }
    }

    fn schedule_block_tick(&mut self, p: BlockPos, block: u32, delay: i64, priority: i32) {
        if self.writable(p) {
            self.blocks.schedule(p, block, self.time + delay, priority);
        }
    }

    fn is_block_tick_scheduled(&self, p: BlockPos, block: u32) -> bool {
        self.blocks.is_scheduled(p, block)
    }

    fn in_world(&self, p: BlockPos) -> bool {
        let h = self.pipeline.heights;
        p.y >= h.min_y && p.y < h.min_y + h.height
    }

    fn fast_lava(&self) -> bool {
        self.pipeline.fast_lava
    }

    /// Falls straight to rest.
    fn spawn_falling_block(&mut self, p: BlockPos, state: u32) {
        let ctx = self.ctx();
        let h = self.pipeline.heights;
        let mut y = p.y;
        while y > h.min_y
            && mc_tick::gravity::free(
                ctx,
                mc_tick::Level::block(self, BlockPos::new(p.x, y - 1, p.z)),
            )
        {
            y -= 1;
        }
        mc_tick::Level::set(self, BlockPos::new(p.x, y, p.z), state);
    }
}
