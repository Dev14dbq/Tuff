//! Finding the command a line means, as Brigadier does: from each node the
//! children that may follow are tried (the literal named by the next word,
//! else every argument in order), each successful branch is followed, and
//! of those the one that read the whole line without errors wins.
//! Redirects (`execute ... run`, `tp`) start a new context in the chain.

use crate::args::{self, Arg, Env};
use crate::error::{CommandError, builtin};
use crate::reader::Reader;
use crate::tree::{Kind, ROOT, Tree};

/// What was read for one part of the chain (up to a redirect).
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// Nodes in the order read, with where each started and ended.
    pub nodes: Vec<(usize, usize, usize)>,
    /// Arguments by name.
    pub args: Vec<(String, Arg)>,
    /// The executable node reached, if any.
    pub command: Option<usize>,
}

impl Context {
    pub fn arg(&self, name: &str) -> Option<&Arg> {
        self.args.iter().find(|(n, _)| n == name).map(|(_, a)| a)
    }

    /// The names of the nodes read (`["time", "set", "time"]`).
    pub fn path<'t>(&self, tree: &'t Tree) -> Vec<&'t str> {
        self.nodes
            .iter()
            .map(|&(n, _, _)| tree.nodes[n].name.as_str())
            .collect()
    }

    fn start(&self) -> Option<usize> {
        self.nodes.first().map(|n| n.1)
    }
}

/// A parsed line: the chain of contexts, where reading stopped and the
/// errors of the branches that failed there.
#[derive(Clone, Debug)]
pub struct Parse {
    pub chain: Vec<Context>,
    pub cursor: usize,
    pub errors: Vec<CommandError>,
}

pub fn parse(tree: &Tree, input: &str, level: u8, env: &dyn Env) -> Parse {
    parse_nodes(tree, ROOT, input, 0, vec![Context::default()], level, env)
}

fn relevant(tree: &Tree, node: usize, input: &str, cursor: usize) -> Vec<usize> {
    let children = &tree.nodes[node].children;
    let literals: Vec<usize> = children
        .iter()
        .copied()
        .filter(|&c| tree.nodes[c].kind == Kind::Literal)
        .collect();
    if !literals.is_empty() {
        let word = Reader::at(input, cursor).word();
        if let Some(&l) = literals.iter().find(|&&l| tree.nodes[l].name == word) {
            return vec![l];
        }
    }
    children
        .iter()
        .copied()
        .filter(|&c| matches!(tree.nodes[c].kind, Kind::Argument { .. }))
        .collect()
}

fn parse_nodes(
    tree: &Tree,
    node: usize,
    input: &str,
    cursor: usize,
    chain: Vec<Context>,
    level: u8,
    env: &dyn Env,
) -> Parse {
    let mut errors = Vec::new();
    let mut potentials: Vec<Parse> = Vec::new();
    for child in relevant(tree, node, input, cursor) {
        let n = &tree.nodes[child];
        if n.level > level {
            continue;
        }
        let mut r = Reader::at(input, cursor);
        let mut chain = chain.clone();
        let read = match &n.kind {
            Kind::Literal => {
                let end = cursor + n.name.len();
                if input.get(cursor..end) == Some(n.name.as_str())
                    && (end == input.len() || input[end..].starts_with(' '))
                {
                    r.cursor = end;
                    Ok(None)
                } else {
                    Err(r.error(builtin::literal(&n.name)))
                }
            }
            Kind::Argument { parser, properties } => {
                args::parse(parser, properties, &mut r, env).map(Some)
            }
            Kind::Root => Ok(None),
        };
        let read = read.and_then(|a| {
            if r.can_read() && r.peek() != ' ' {
                Err(r.error(builtin::separator()))
            } else {
                Ok(a)
            }
        });
        let arg = match read {
            Ok(a) => a,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let ctx = chain.last_mut().expect("context");
        ctx.nodes.push((child, cursor, r.cursor));
        if let Some(a) = arg {
            ctx.args.push((n.name.clone(), a));
        }
        if n.executable {
            ctx.command = Some(child);
        }
        let need = if n.redirect.is_some() { 1 } else { 2 };
        if r.can_read_n(need) {
            r.skip();
            if let Some(target) = n.redirect {
                chain.push(Context::default());
                return parse_nodes(tree, target, input, r.cursor, chain, level, env);
            }
            potentials.push(parse_nodes(tree, child, input, r.cursor, chain, level, env));
        } else {
            potentials.push(Parse {
                chain,
                cursor: r.cursor,
                errors: Vec::new(),
            });
        }
    }
    if potentials.is_empty() {
        return Parse {
            chain,
            cursor,
            errors,
        };
    }
    // Stable: the first of the best keeps the tree's order.
    potentials.sort_by_key(|p| (p.cursor < input.len(), !p.errors.is_empty()));
    potentials.swap_remove(0)
}

impl Parse {
    /// The error to show when the line cannot run, or `None` when it can.
    pub fn error(&self, input: &str) -> Option<CommandError> {
        if self.cursor < input.len() {
            if self.errors.len() == 1 {
                return Some(self.errors[0].clone());
            }
            let key = if self.chain[0].start().is_none() {
                builtin::unknown_command()
            } else {
                builtin::unknown_argument()
            };
            return Some(key.at(input, self.cursor));
        }
        if self.chain.last().and_then(|c| c.command).is_none() {
            return Some(builtin::unknown_command().at(input, self.cursor));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::AnyEnv;

    fn tree() -> Option<Tree> {
        let d =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        Tree::load(&d).ok()
    }

    #[test]
    fn lines() {
        let Some(t) = tree() else { return };
        let ok = |s: &str| {
            let p = parse(&t, s, 4, &AnyEnv);
            assert!(p.error(s).is_none(), "{s}: {:?}", p.error(s));
            p
        };
        let p = ok("time set 1000");
        assert_eq!(p.chain[0].path(&t), ["time", "set", "time"]);
        assert_eq!(p.chain[0].arg("time"), Some(&Arg::Time(1000)));
        let p = ok("time set day");
        assert_eq!(p.chain[0].path(&t), ["time", "set", "timemarker"]);
        let p = ok("execute as @a at @s run tp ~ ~1 ~");
        // execute as | at | run | tp → teleport | location
        assert_eq!(p.chain.len(), 5);
        let cmd = p.chain[4].command.unwrap();
        assert_eq!(t.names(cmd), ["teleport", "location"]);
        let key = |s: &str| {
            let e = parse(&t, s, 4, &AnyEnv).error(s).expect("an error");
            (e.message, e.at.map(|a| a.1))
        };
        let tr = |k: &str| crate::text::translate(k, Vec::new());
        assert_eq!(
            key("nosuchcommand"),
            (tr("command.unknown.command"), Some(0))
        );
        assert_eq!(key("time"), (tr("command.unknown.command"), Some(4)));
        assert_eq!(
            key("tp ~ ~ ~ extra"),
            (tr("command.unknown.argument"), Some(9))
        );
        assert!(
            parse(&t, "gamemode survival", 2, &AnyEnv)
                .error("gamemode survival")
                .is_none()
        );
        assert!(
            parse(&t, "gamemode survival", 0, &AnyEnv)
                .error("gamemode survival")
                .is_some()
        );
    }
}
