//! Endermites (minecraft.wiki, "Endermite"): a player's ender pearl that
//! lands spawns one with a 5% chance, at the place the player threw it
//! from. It goes away after 2400 ticks unless named or kept (see
//! `mc_entity::mob::ENDERMITE_LIFE`), and endermen go after it.

use mc_entity::Species;

use super::new_mob;
use crate::context::ServerContext;

/// The chance of an endermite when a pearl lands.
const CHANCE: f64 = 0.05;

/// A thrown pearl landed and its thrower was at `from`: now and then an
/// endermite appears there.
pub fn from_pearl(ctx: &ServerContext, from: [f64; 3]) {
    roll(ctx, from, crate::hits::rnd());
}

/// Spawns the endermite if `chance` (a random number in 0..1) is under
/// the pearl's 5% and monsters may spawn; whether it did.
fn roll(ctx: &ServerContext, at: [f64; 3], chance: f64) -> bool {
    if chance >= CHANCE || ctx.difficulty() == 0 || !ctx.game_rule_bool("spawn_monsters") {
        return false;
    }
    crate::tick::spawn_entity(ctx, new_mob(ctx, Species::Endermite, at));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick::lock;

    fn count(ctx: &ServerContext) -> usize {
        lock(&ctx.entities)
            .iter()
            .filter(
                |e| matches!(&e.kind, mc_entity::Kind::Mob(m) if m.species == Species::Endermite),
            )
            .count()
    }

    #[test]
    fn a_pearl_brings_an_endermite_one_time_in_twenty() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        ctx.difficulty
            .store(2, std::sync::atomic::Ordering::Relaxed);
        assert!(!roll(&ctx, [0.5, -60.0, 0.5], 0.06));
        assert_eq!(count(&ctx), 0);
        assert!(roll(&ctx, [0.5, -60.0, 0.5], 0.04));
        assert_eq!(count(&ctx), 1);
        // Not in peaceful.
        ctx.difficulty
            .store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(!roll(&ctx, [0.5, -60.0, 0.5], 0.0));
        assert_eq!(count(&ctx), 1);
    }
}
