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
        _ => return None,
    })
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
