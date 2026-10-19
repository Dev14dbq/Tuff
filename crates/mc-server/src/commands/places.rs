//! Commands of the `places` group (see `run::run` for the ones done
//! elsewhere). Each returns `None` for paths it does not handle.

#![allow(dead_code)]

use super::{Result, Run, Store};
use crate::context::ServerContext;

pub fn run(_r: &mut Run, _path: &[&str]) -> Option<Result<i32>> {
    None
}

/// Puts a stored result into a boss bar.
pub fn store(_ctx: &ServerContext, _store: &Store, _value: i32) {}
