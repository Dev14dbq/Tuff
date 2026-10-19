//! Commands of the `scores` group (see `run::run` for the ones done
//! elsewhere). Each returns `None` for paths it does not handle.

use super::{Result, Run};

pub fn run(_r: &mut Run, _path: &[&str]) -> Option<Result<i32>> {
    None
}
