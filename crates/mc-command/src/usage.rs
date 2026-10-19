//! Brigadier's smart usage, as `/help` prints it: `/time set
//! (<time>|<timemarker>)`, `/clear [<targets>]`.

use crate::tree::{Kind, ROOT, Tree};

fn text(tree: &Tree, node: usize) -> String {
    let n = &tree.nodes[node];
    match n.kind {
        Kind::Argument { .. } => format!("<{}>", n.name),
        _ => n.name.clone(),
    }
}

fn smart(tree: &Tree, node: usize, level: u8, optional: bool, deep: bool) -> Option<String> {
    let n = &tree.nodes[node];
    if n.level > level {
        return None;
    }
    let me = if optional {
        format!("[{}]", text(tree, node))
    } else {
        text(tree, node)
    };
    let child_optional = n.executable;
    let (open, close) = if child_optional {
        ("[", "]")
    } else {
        ("(", ")")
    };
    if !deep {
        if let Some(r) = n.redirect {
            let target = if r == ROOT {
                "...".to_owned()
            } else {
                format!("-> {}", text(tree, r))
            };
            return Some(format!("{me} {target}"));
        }
        let children: Vec<usize> = n
            .children
            .iter()
            .copied()
            .filter(|&c| tree.nodes[c].level <= level)
            .collect();
        if children.len() == 1 {
            if let Some(u) = smart(tree, children[0], level, child_optional, child_optional) {
                return Some(format!("{me} {u}"));
            }
        } else if children.len() > 1 {
            let mut usages: Vec<String> = Vec::new();
            for &c in &children {
                if let Some(u) = smart(tree, c, level, child_optional, true)
                    && !usages.contains(&u)
                {
                    usages.push(u);
                }
            }
            if usages.len() == 1 {
                let u = &usages[0];
                return Some(if child_optional {
                    format!("{me} [{u}]")
                } else {
                    format!("{me} {u}")
                });
            } else if usages.len() > 1 {
                let names: Vec<String> = children.iter().map(|&c| text(tree, c)).collect();
                return Some(format!("{me} {open}{}{close}", names.join("|")));
            }
        }
    }
    Some(me)
}

/// The usage of each child of `node` (for `/help`: the root's children).
pub fn smart_usage(tree: &Tree, node: usize, level: u8) -> Vec<(usize, String)> {
    let optional = tree.nodes[node].executable;
    tree.nodes[node]
        .children
        .iter()
        .filter_map(|&c| Some((c, smart(tree, c, level, optional, false)?)))
        .collect()
}
