//! Commands as vanilla parses them (Brigadier): the tree it registers,
//! read from what a vanilla server sends; the reader and its errors; the
//! dispatcher that finds the command a line means; and English text for
//! the console.

pub mod args;
pub mod dispatch;
pub mod env;
pub mod error;
pub mod lang;
pub mod reader;
pub mod text;
pub mod tree;

pub use error::CommandError;
pub use tree::Tree;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    fn data() -> Option<PathBuf> {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        d.join("observed/commands.bin").is_file().then_some(d)
    }

    /// An operator gets the tree vanilla sends, byte for byte.
    #[test]
    fn packet_matches_vanilla() {
        let Some(d) = data() else { return };
        let tree = crate::Tree::load(&d).unwrap();
        let vanilla = std::fs::read(d.join("observed/commands.bin")).unwrap();
        let ours = tree.packet(4);
        assert_eq!(ours.len(), vanilla.len());
        assert!(ours == vanilla, "packets differ");
    }

    /// Lines vanilla could not parse fail here with the same two lines.
    #[test]
    fn parse_errors_match_vanilla() {
        let Some(d) = data() else { return };
        let tree = crate::Tree::load(&d).unwrap();
        let env = crate::env::DataEnv::load(&d);
        let observed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(d.join("observed/commands.json")).unwrap(),
        )
        .unwrap();
        let mut bad = Vec::new();
        for l in observed["lines"].as_array().unwrap() {
            let line = l["line"].as_str().unwrap();
            if line.starts_with('>') {
                continue;
            }
            let replies: Vec<&serde_json::Value> = l["replies"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| &r["chat"])
                .collect();
            let context = replies
                .get(1)
                .is_some_and(|r| r["extra"][0]["click_event"].is_object());
            let parse = crate::dispatch::parse(&tree, line, 4, &env);
            let ours = parse.error(line);
            if context {
                let got: Vec<serde_json::Value> = ours
                    .map(|e| e.lines().iter().map(crate::text::to_json).collect())
                    .unwrap_or_default();
                if got.len() != 2 || got[0] != *replies[0] || got[1] != *replies[1] {
                    bad.push(format!(
                        "{line}\n  vanilla {:?}\n  ours    {got:?}",
                        &replies[..2]
                    ));
                }
            } else if let Some(e) = ours {
                bad.push(format!("{line}: vanilla ran it, ours {:?}", e.message));
            }
        }
        assert!(
            bad.is_empty(),
            "{} lines differ:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }
}
