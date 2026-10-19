//! Commands of the `functions` group (see `run::run` for the ones done
//! elsewhere). Each returns `None` for paths it does not handle.

#![allow(dead_code)]

use mc_command::dispatch::Context;

use super::{Result, Run, Src, Store};
use crate::context::ServerContext;

pub fn run(_r: &mut Run, _path: &[&str]) -> Option<Result<i32>> {
    None
}

/// `execute if|unless data|function|...`; `None` for other tests.
pub fn condition(
    _ctx: &ServerContext,
    _c: &Context,
    _s: &Src,
    _rest: &[&str],
) -> Option<Result<bool>> {
    None
}

/// `execute store|on|summon|facing|...`; `None` for others.
pub fn modify(
    _ctx: &ServerContext,
    _c: &Context,
    _s: &Src,
    _path: &[&str],
) -> Option<Result<Vec<Src>>> {
    None
}

/// Puts a stored result into block, entity or storage data.
pub fn store(_ctx: &ServerContext, _store: &Store, _value: i32) {}
