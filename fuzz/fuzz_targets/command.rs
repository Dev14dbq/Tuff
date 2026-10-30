//! A command line from a client (at most 32767 characters, like the chat
//! command packet): parsed at operator level, its error worked out, and
//! completed the way suggestions are.

#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use mc_command::env::DataEnv;
use mc_command::suggest::Known;
use mc_command::{Tree, dispatch, suggest};

struct Nobody;

impl Known for Nobody {
    fn players(&self) -> Vec<String> {
        vec!["Steve".into()]
    }
    fn ask(&self, _: &[String], _: &str) -> Vec<String> {
        Vec::new()
    }
}

fn commands() -> Option<&'static (Tree, DataEnv)> {
    static COMMANDS: OnceLock<Option<(Tree, DataEnv)>> = OnceLock::new();
    COMMANDS
        .get_or_init(|| {
            let data = tuff_fuzz::data_dir()?;
            Some((Tree::load(&data).ok()?, DataEnv::load(&data)))
        })
        .as_ref()
}

fuzz_target!(|data: &[u8]| {
    let Some((tree, env)) = commands() else {
        return;
    };
    let Ok(line) = std::str::from_utf8(data) else {
        return;
    };
    if line.encode_utf16().count() > 32767 {
        return;
    }
    let parse = dispatch::parse(tree, line, 4, env);
    let _ = parse.error(line);
    let _ = suggest::complete(tree, line, 4, env, env, &Nobody);
});
