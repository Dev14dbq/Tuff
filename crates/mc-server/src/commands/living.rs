//! Commands of the `living` group (see `run::run` for the ones done
//! elsewhere). Each returns `None` for paths it does not handle.

use mc_nbt::{Compound, Tag};
use mc_protocol::text::translate;

use super::run::err;
use super::target::Target;
use super::{Result, Run};
use crate::effects::{self, Instance, Kind};

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
        let key = death_key(r, &kind);
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

/// Only players are living entities on this server so far.
fn living(t: &Target) -> bool {
    t.is_player()
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
        if living(t)
            && effects::give(
                r.ctx,
                t.uuid(),
                kind,
                Instance::new(amplifier, duration, particles),
            )
        {
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
        if living(t) && effects::remove(r.ctx, t.uuid(), kind) {
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
        r.out
            .events
            .push(crate::context::Event::Teleport { uuid: *uuid, pose });
    }
    r.success(
        translate("commands.rotate.success", vec![t.display()]),
        true,
    );
    Ok(1)
}
