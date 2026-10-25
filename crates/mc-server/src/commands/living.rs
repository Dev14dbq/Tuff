//! Commands of the `living` group (see `run::run` for the ones done
//! elsewhere). Each returns `None` for paths it does not handle.

use mc_nbt::{Compound, Tag};
use mc_protocol::text::translate;

use super::run::err;
use super::target::Target;
use super::{Result, Run};
use crate::effects::{self, Instance, Kind};
use crate::tick::lock;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["effect", "give", "targets", "effect", rest @ ..] => effect_give(r, rest),
        ["effect", "clear"] | ["effect", "clear", "targets"] => effect_clear(r, None),
        ["effect", "clear", "targets", "effect"] => match effect_kind(r) {
            Ok(k) => effect_clear(r, Some(k)),
            Err(e) => Err(e),
        },
        ["damage", "target", "amount", rest @ ..] => damage(r, rest),
        ["rotate", "target", rest @ ..] => rotate(r, rest),
        ["spreadplayers", "center", "spreadDistance", "maxRange", ..] => spread_players(r),
        ["spectate", rest @ ..] => spectate(r, rest),
        ["ride", "target", "dismount"] => ride_dismount(r),
        ["ride", "target", "mount", "vehicle"] => ride_mount(r),
        _ => return None,
    })
}

/// The death message key of a damage type (`death.attack.<message_id>`).
fn death_key(r: &Run, damage_type: &str) -> String {
    let path = damage_type
        .strip_prefix("minecraft:")
        .unwrap_or(damage_type);
    let file = r
        .ctx
        .data
        .data_pack()
        .root
        .join(format!("minecraft/damage_type/{path}.json"));
    let id = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["message_id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "generic".into());
    format!("death.attack.{id}")
}

fn damage(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let targets = r.targets("target", false)?;
    let Some(t) = targets.first() else {
        return Err(err("argument.entity.notfound.entity", Vec::new()));
    };
    let amount = r.float("amount").unwrap_or(0.0);
    let kind = r
        .string("damageType")
        .unwrap_or_else(|| "minecraft:generic".into());
    let by = if rest.contains(&"by") {
        r.targets("entity", false)?.first().map(|e| match e {
            Target::Player { name, .. } => name.clone(),
            _ => String::new(),
        })
    } else {
        None
    };
    let hurt = if t.is_player() {
        let key = crate::damage::with_attacker(
            &death_key(r, &kind),
            by.as_deref().is_some_and(|n| !n.is_empty()),
        );
        crate::vitals::try_hurt(
            r.ctx,
            t.uuid(),
            amount,
            &kind,
            &key,
            by.as_deref().filter(|n| !n.is_empty()),
        )
        .is_some()
    } else {
        // Other entities have no health yet: the hit lands.
        amount > 0.0
    };
    if !hurt {
        return Err(err("commands.damage.invulnerable", Vec::new()));
    }
    r.success(
        translate(
            "commands.damage.success",
            vec![Tag::Float(amount), t.display()],
        ),
        true,
    );
    Ok(1)
}

fn effect_kind(r: &Run) -> Result<&'static Kind> {
    let id = r.string("effect").unwrap_or_default();
    effects::kind_of(&id).ok_or_else(|| {
        err(
            "argument.resource.not_found",
            vec![Tag::from(id.as_str()), Tag::from("minecraft:mob_effect")],
        )
    })
}

fn effect_name(kind: &Kind) -> Tag {
    let mut c = Compound::new();
    c.insert("translate", Tag::String(kind.key()));
    Tag::Compound(c)
}

/// Gives a player or mob an effect; whether it took (other entities
/// take none).
fn give_to(r: &Run, t: &Target, kind: &'static Kind, i: Instance) -> bool {
    if t.is_player() {
        effects::give(r.ctx, t.uuid(), kind, i)
    } else {
        crate::mob_effects::with_mob(r.ctx, t.uuid(), |e| {
            crate::mob_effects::give(r.ctx, e, kind, i, None)
        })
        .unwrap_or(false)
    }
}

/// Takes effects from a player or mob; whether any went.
fn clear_from(r: &Run, t: &Target, kind: Option<&'static Kind>) -> bool {
    if t.is_player() {
        effects::remove(r.ctx, t.uuid(), kind)
    } else {
        crate::mob_effects::with_mob(r.ctx, t.uuid(), |e| {
            crate::mob_effects::remove(r.ctx, e, kind)
        })
        .unwrap_or(false)
    }
}

fn effect_give(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let kind = effect_kind(r)?;
    let targets = r.targets("targets", false)?;
    let duration = match rest.first() {
        Some(&"infinite") => -1,
        Some(_) => {
            let seconds = r.int("seconds").unwrap_or(30);
            if kind.instant {
                seconds
            } else {
                seconds.saturating_mul(20)
            }
        }
        None if kind.instant => 1,
        None => 600,
    };
    let amplifier = r.int("amplifier").unwrap_or(0);
    let particles = !r.boolean("hideParticles").unwrap_or(false);
    let mut done = 0;
    for t in &targets {
        if give_to(r, t, kind, Instance::new(amplifier, duration, particles)) {
            done += 1;
        }
    }
    if done == 0 {
        return Err(err("commands.effect.give.failed", Vec::new()));
    }
    // Whole seconds (instant and endless effects show 0).
    let seconds = Tag::Int(if duration < 0 { 0 } else { duration / 20 });
    let msg = if targets.len() == 1 {
        translate(
            "commands.effect.give.success.single",
            vec![effect_name(kind), targets[0].display(), seconds],
        )
    } else {
        translate(
            "commands.effect.give.success.multiple",
            vec![effect_name(kind), Tag::Int(targets.len() as i32), seconds],
        )
    };
    r.success(msg, true);
    Ok(done)
}

fn effect_clear(r: &mut Run, kind: Option<&'static Kind>) -> Result<i32> {
    let targets = r.targets_or_self("targets", false)?;
    let mut done = 0;
    for t in &targets {
        if clear_from(r, t, kind) {
            done += 1;
        }
    }
    let what = if kind.is_some() {
        "specific"
    } else {
        "everything"
    };
    if done == 0 {
        return Err(err(
            &format!("commands.effect.clear.{what}.failed"),
            Vec::new(),
        ));
    }
    let mut args = Vec::new();
    if let Some(k) = kind {
        args.push(effect_name(k));
    }
    let msg = if targets.len() == 1 {
        args.push(targets[0].display());
        translate(
            &format!("commands.effect.clear.{what}.success.single"),
            args,
        )
    } else {
        args.push(Tag::Int(targets.len() as i32));
        translate(
            &format!("commands.effect.clear.{what}.success.multiple"),
            args,
        )
    };
    r.success(msg, true);
    Ok(done)
}

fn rotate(r: &mut Run, rest: &[&str]) -> Result<i32> {
    use super::run::{EYE_HEIGHT, facing_point, look_at};
    let targets = r.targets("target", false)?;
    let Some(t) = targets.first() else {
        return Err(err("argument.entity.notfound.entity", Vec::new()));
    };
    if let Target::Player { uuid, pose, .. } = t {
        let mut pose = *pose;
        if rest.first() == Some(&"facing") {
            let at = facing_point(r)?;
            (pose.yaw, pose.pitch) = look_at([pose.x, pose.y + EYE_HEIGHT, pose.z], at);
        } else if let Some(mc_command::args::Arg::Pos2([yaw, pitch])) = r.arg("rotation") {
            let f = |c: &mc_command::args::Coord, base: f32| {
                if c.relative {
                    base + c.value as f32
                } else {
                    c.value as f32
                }
            };
            pose.yaw = f(yaw, r.src.rot[0]);
            pose.pitch = f(pitch, r.src.rot[1]).clamp(-90.0, 90.0);
        }
        r.teleport(*uuid, pose);
    }
    r.success(
        translate("commands.rotate.success", vec![t.display()]),
        true,
    );
    Ok(1)
}

fn spread_players(r: &mut Run) -> Result<i32> {
    use mc_command::args::Arg;
    let Some(Arg::Pos2([cx, cz])) = r.arg("center").cloned() else {
        return Err(err("command.failed", Vec::new()));
    };
    let base = |c: &mc_command::args::Coord, b: f64| if c.relative { b + c.value } else { c.value };
    let center = (base(&cx, r.src.pos[0]), base(&cz, r.src.pos[2]));
    let spread = f64::from(r.float("spreadDistance").unwrap_or(0.0));
    let range = f64::from(r.float("maxRange").unwrap_or(1.0));
    let world = &r.ctx.world;
    let top = world.min_y + world.height as i32;
    let max_height = r.int("maxHeight").unwrap_or(top);
    if max_height < world.min_y {
        return Err(err(
            "commands.spreadplayers.failed.invalid.height",
            vec![Tag::Int(max_height), Tag::Int(world.min_y)],
        ));
    }
    let teams = r.boolean("respectTeams").unwrap_or(false);
    let targets = r.targets("targets", false)?;
    let edge = 29_999_984.0;
    let (min_x, max_x) = ((center.0 - range).max(-edge), (center.0 + range).min(edge));
    let (min_z, max_z) = ((center.1 - range).max(-edge), (center.1 + range).min(edge));
    let mut rng = crate::util::random_u64();
    let mut unit = move || {
        // xorshift: where they land is random anyway.
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut at: Vec<(f64, f64)> = (0..targets.len())
        .map(|_| {
            (
                min_x + unit() * (max_x - min_x),
                min_z + unit() * (max_z - min_z),
            )
        })
        .collect();
    // The ground under a column (below `max_height`), if it can be stood on.
    let ground = |x: f64, z: f64| -> Option<i32> {
        let blocks = &r.ctx.data.blocks;
        let (x, z) = (x.floor() as i32, z.floor() as i32);
        let mut y = max_height.min(top) - 1;
        while y > world.min_y {
            let s = world.block(mc_protocol::Position::new(x, y, z), blocks)?;
            if !blocks.is_air(s) {
                let name = blocks.block_name(s).unwrap_or("");
                let bad = blocks.is_fluid(s) || name == "minecraft:fire";
                return (!bad).then_some(y + 1);
            }
            y -= 1;
        }
        None
    };
    let mut settled = false;
    for _ in 0..10_000 {
        let mut moved = false;
        for i in 0..at.len() {
            let (mut px, mut pz, mut n) = (0.0, 0.0, 0);
            for j in 0..at.len() {
                if i == j {
                    continue;
                }
                let (dx, dz) = (at[i].0 - at[j].0, at[i].1 - at[j].1);
                let d = (dx * dx + dz * dz).sqrt();
                if d < spread {
                    n += 1;
                    let d = d.max(1e-4);
                    px += dx / d;
                    pz += dz / d;
                }
            }
            if n > 0 {
                let len = (px * px + pz * pz).sqrt().max(1e-4);
                at[i].0 = (at[i].0 + px / len).clamp(min_x, max_x);
                at[i].1 = (at[i].1 + pz / len).clamp(min_z, max_z);
                moved = true;
            }
            if ground(at[i].0, at[i].1).is_none() {
                at[i] = (
                    min_x + unit() * (max_x - min_x),
                    min_z + unit() * (max_z - min_z),
                );
                moved = true;
            }
        }
        if !moved {
            settled = true;
            break;
        }
    }
    let kind = if teams { "teams" } else { "entities" };
    if !settled {
        return Err(err(
            &format!("commands.spreadplayers.failed.{kind}"),
            vec![
                Tag::Int(targets.len() as i32),
                Tag::Float(center.0 as f32),
                Tag::Float(center.1 as f32),
                Tag::String(format!("{spread:.2}")),
            ],
        ));
    }
    let mut total = 0.0;
    for (i, t) in targets.iter().enumerate() {
        let (x, z) = (at[i].0.floor() + 0.5, at[i].1.floor() + 0.5);
        let y = f64::from(ground(x, z).unwrap_or(max_height));
        if let Target::Player { uuid, pose, .. } = t {
            let pose = crate::context::Pose { x, y, z, ..*pose };
            r.teleport(*uuid, pose);
        } else if let Some(e) = crate::tick::lock(&r.ctx.entities)
            .iter_mut()
            .find(|e| e.uuid == t.uuid())
        {
            e.pos = [x, y, z];
        }
        let nearest = (0..at.len())
            .filter(|&j| j != i)
            .map(|j| ((at[i].0 - at[j].0).powi(2) + (at[i].1 - at[j].1).powi(2)).sqrt())
            .fold(f64::MAX, f64::min);
        if nearest < f64::MAX {
            total += nearest;
        }
    }
    let average = if at.len() > 1 {
        total / at.len() as f64
    } else {
        0.0
    };
    r.success(
        translate(
            &format!("commands.spreadplayers.success.{kind}"),
            vec![
                Tag::Int(targets.len() as i32),
                Tag::Float(center.0 as f32),
                Tag::Float(center.1 as f32),
                Tag::String(format!("{average:.2}")),
            ],
        ),
        true,
    );
    Ok(targets.len() as i32)
}

fn spectate(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let player = if rest.contains(&"player") {
        r.targets("player", true)?
    } else {
        r.targets_or_self("__self", true)?
    };
    let Some(Target::Player {
        uuid, game_mode, ..
    }) = player.first().cloned()
    else {
        return Err(err("argument.entity.notfound.player", Vec::new()));
    };
    let target = if rest.is_empty() {
        None
    } else {
        r.targets("target", false)?.first().cloned()
    };
    if let Some(t) = &target
        && t.uuid() == uuid
    {
        return Err(err("commands.spectate.self", Vec::new()));
    }
    if game_mode != 3 {
        return Err(err(
            "commands.spectate.not_spectator",
            vec![player[0].display()],
        ));
    }
    let own = crate::tick::lock(&r.ctx.players)
        .get(&uuid)
        .map_or(0, |p| p.entity_id);
    let camera = match &target {
        Some(Target::Player { uuid: u, .. }) => crate::tick::lock(&r.ctx.players)
            .get(u)
            .map_or(own, |p| p.entity_id),
        Some(Target::Entity { id, .. }) => *id,
        None => own,
    };
    super::run::direct(
        r,
        uuid,
        crate::context::Direct::Camera(mc_protocol::packets::play::SetCamera { entity_id: camera }),
    );
    let msg = match &target {
        Some(t) => translate("commands.spectate.success.started", vec![t.display()]),
        None => translate("commands.spectate.success.stopped", Vec::new()),
    };
    r.success(msg, false);
    Ok(1)
}

/// Nothing rides anything yet.
fn ride_dismount(r: &mut Run) -> Result<i32> {
    let t = r.targets("target", false)?;
    let Some(t) = t.first() else {
        return Err(err("argument.entity.notfound.entity", Vec::new()));
    };
    Err(err("commands.ride.not_riding", vec![t.display()]))
}

fn ride_mount(r: &mut Run) -> Result<i32> {
    let vehicle = r.targets("vehicle", false)?;
    let Some(v) = vehicle.first() else {
        return Err(err("argument.entity.notfound.entity", Vec::new()));
    };
    if v.is_player() {
        return Err(err(
            "commands.ride.mount.failure.cant_ride_players",
            Vec::new(),
        ));
    }
    let riders = r.targets("target", false)?;
    let Some(t) = riders.first() else {
        return Err(err("argument.entity.notfound.entity", Vec::new()));
    };
    let rider = match t {
        Target::Player { uuid, .. } => lock(&r.ctx.players).get(uuid).map(|p| p.entity_id),
        Target::Entity { id, .. } => Some(*id),
    };
    let (Some(rider), Target::Entity { id: vehicle, .. }) = (rider, v) else {
        return Err(err("commands.ride.mount.failure.generic", Vec::new()));
    };
    // Mobs only: boats and carts take riders their own way.
    let mounted = {
        let mut entities = lock(&r.ctx.entities);
        entities
            .iter_mut()
            .find(|e| e.id == *vehicle && !e.removed && matches!(e.kind, mc_entity::Kind::Mob(_)))
            .is_some_and(|e| crate::mobs::ride::mount(r.ctx, e, rider))
    };
    if !mounted {
        return Err(err("commands.ride.mount.failure.generic", Vec::new()));
    }
    r.success(
        translate(
            "commands.ride.mount.success",
            vec![t.display(), v.display()],
        ),
        true,
    );
    Ok(1)
}
