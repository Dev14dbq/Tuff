//! World generation: noise, density functions, biomes, surface rules, features, structures.
//!
//! For now only the superflat generator exists (ROADMAP Ф1); the noise
//! generator with vanilla parity is ROADMAP Ф4.

use mc_world::Chunk;

/// One horizontal layer of a flat world, from the bottom up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlatLayer {
    pub state: u32,
    pub thickness: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatGenerator {
    /// Bottom to top, starting at the world's `min_y`.
    pub layers: Vec<FlatLayer>,
    pub biome: u32,
    pub air: u32,
}

impl FlatGenerator {
    pub fn generate(&self, x: i32, z: i32, min_y: i32, height: u32) -> Chunk {
        let mut chunk = Chunk::new(x, z, min_y, height, self.air, self.biome);
        let mut y = min_y;
        for layer in &self.layers {
            for _ in 0..layer.thickness {
                if y >= min_y + height as i32 {
                    return chunk;
                }
                for bz in 0..16 {
                    for bx in 0..16 {
                        chunk.set_block(bx, y, bz, layer.state);
                    }
                }
                y += 1;
            }
        }
        chunk
    }

    /// Y of the first air block above the layers (where players stand).
    pub fn surface_y(&self, min_y: i32) -> i32 {
        min_y + self.layers.iter().map(|l| l.thickness as i32).sum::<i32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_flat_layers() {
        let generator = FlatGenerator {
            layers: vec![
                FlatLayer {
                    state: 1,
                    thickness: 1,
                },
                FlatLayer {
                    state: 2,
                    thickness: 2,
                },
                FlatLayer {
                    state: 3,
                    thickness: 1,
                },
            ],
            biome: 0,
            air: 0,
        };
        let chunk = generator.generate(3, -2, -64, 384);
        assert_eq!((chunk.x, chunk.z), (3, -2));
        assert_eq!(chunk.block(0, -64, 0), 1);
        assert_eq!(chunk.block(15, -63, 15), 2);
        assert_eq!(chunk.block(7, -62, 7), 2);
        assert_eq!(chunk.block(7, -61, 7), 3);
        assert_eq!(chunk.block(7, -60, 7), 0);
        assert_eq!(generator.surface_y(-64), -60);
    }
}
