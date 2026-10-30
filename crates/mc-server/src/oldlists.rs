//! Conversion of the player lists from before Minecraft 1.7.6 (`white-list.txt`,
//! `banned-players.txt`, `banned-ips.txt`, `ops.txt`) to the JSON files, once
//! on start, as vanilla's old-user converter does. Names become UUIDs through
//! Mojang's profile service in online mode and through offline UUIDs
//! otherwise. A converted file is renamed to `<name>.converted`; the server
//! does not start while an old file is still there.

use std::path::Path;
use std::time::Duration;

use tracing::{error, warn};

use crate::lists::{Ban, Lists, Named, Op};
use crate::util::{date_text, now_secs, offline_uuid, parse_date};

const OLD_IP_BANS: &str = "banned-ips.txt";
const OLD_PLAYER_BANS: &str = "banned-players.txt";
const OLD_OPS: &str = "ops.txt";
const OLD_WHITELIST: &str = "white-list.txt";

/// Turns names into profiles. A name that has no profile is left out of the
/// result; an `Err` means the service could not be asked.
pub trait Resolver {
    fn resolve(&mut self, names: &[String]) -> Result<Vec<(String, u128)>, String>;
}

/// Offline mode: every name has the UUID the offline login would give it.
#[derive(Debug)]
pub struct OfflineResolver;

impl Resolver for OfflineResolver {
    fn resolve(&mut self, names: &[String]) -> Result<Vec<(String, u128)>, String> {
        Ok(names
            .iter()
            .filter(|n| !n.is_empty())
            .map(|n| (n.clone(), offline_uuid(n)))
            .collect())
    }
}

/// Online mode: Mojang's bulk name lookup, ten names to a request.
#[derive(Debug)]
pub struct MojangResolver;

impl Resolver for MojangResolver {
    fn resolve(&mut self, names: &[String]) -> Result<Vec<(String, u128)>, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .into();
        let mut found = Vec::new();
        let names: Vec<&String> = names.iter().filter(|n| !n.is_empty()).collect();
        for chunk in names.chunks(10) {
            let body = serde_json::to_string(&chunk).map_err(|e| e.to_string())?;
            let mut response = agent
                .post("https://api.minecraftservices.com/minecraft/profile/lookup/bulk/byname")
                .header("Content-Type", "application/json")
                .send(body)
                .map_err(|e| e.to_string())?;
            let text = response
                .body_mut()
                .read_to_string()
                .map_err(|e| e.to_string())?;
            let list: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            for p in list.as_array().into_iter().flatten() {
                let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) else {
                    continue;
                };
                if let Ok(uuid) = u128::from_str_radix(id, 16) {
                    found.push((name.to_owned(), uuid));
                }
            }
        }
        Ok(found)
    }
}

/// Converts whatever old lists are in `dir`. Returns `Err` with the log
/// lines vanilla prints when an old file is left, which stops the start.
pub fn convert(dir: &Path, op_level: i32, resolver: &mut dyn Resolver) -> Result<(), String> {
    let present = |name: &str| dir.join(name).is_file();
    if present(OLD_PLAYER_BANS)
        || present(OLD_IP_BANS)
        || present(OLD_OPS)
        || present(OLD_WHITELIST)
    {
        let mut lists = Lists::load(dir);
        if present(OLD_PLAYER_BANS) {
            bans(dir, &mut lists, resolver);
        }
        if present(OLD_IP_BANS) {
            ip_bans(dir, &mut lists);
        }
        if present(OLD_OPS) {
            ops(dir, &mut lists, op_level, resolver);
        }
        if present(OLD_WHITELIST) {
            whitelist(dir, &mut lists, resolver);
        }
    }
    let left: Vec<&str> = [OLD_PLAYER_BANS, OLD_IP_BANS, OLD_OPS, OLD_WHITELIST]
        .into_iter()
        .filter(|n| present(n))
        .collect();
    if left.is_empty() {
        return Ok(());
    }
    warn!("**** FAILED TO START THE SERVER AFTER ACCOUNT CONVERSION!");
    warn!("** please remove the following files and restart the server:");
    for name in &left {
        warn!("* {name}");
    }
    Err(format!(
        "old user lists could not be converted: {}",
        left.join(", ")
    ))
}

fn rename_done(dir: &Path, name: &str) {
    let _ = std::fs::rename(dir.join(name), dir.join(format!("{name}.converted")));
}

/// `name|created|source|expires|reason`, keyed by the lower-case first field.
/// Lines starting with `#` and empty lines are skipped.
fn read_old_bans(dir: &Path, name: &str) -> Result<Vec<(String, Vec<String>)>, std::io::Error> {
    let text = std::fs::read_to_string(dir.join(name))?;
    let mut entries: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let parts: Vec<String> = line.split('|').map(str::to_owned).collect();
        let key = parts[0].to_lowercase();
        entries.retain(|(k, _)| *k != key);
        entries.push((key, parts));
    }
    Ok(entries)
}

fn ban_from(name: String, uuid: Option<u128>, parts: &[String]) -> Ban {
    let field = |i: usize| parts.get(i).map(String::as_str);
    let date = |i: usize| field(i).and_then(parse_date);
    Ban {
        uuid,
        name,
        created: date_text(date(1).unwrap_or_else(now_secs)),
        source: field(2).unwrap_or("(Unknown)").to_owned(),
        expires: date(3).map_or_else(|| "forever".to_owned(), date_text),
        reason: field(4).unwrap_or("").to_owned(),
    }
}

fn upsert_ban(list: &mut Vec<Ban>, ban: Ban) {
    list.retain(|b| b.name != ban.name || b.uuid != ban.uuid);
    list.push(ban);
}

fn bans(dir: &Path, lists: &mut Lists, resolver: &mut dyn Resolver) {
    let entries = match read_old_bans(dir, OLD_PLAYER_BANS) {
        Ok(e) => e,
        Err(e) => {
            warn!("Could not read old user banlist to convert it! {e}");
            return;
        }
    };
    let names: Vec<String> = entries.iter().map(|(k, _)| k.clone()).collect();
    let profiles = match resolver.resolve(&names) {
        Ok(p) => p,
        Err(e) => {
            error!("Conversion failed, please try again later: {e}");
            return;
        }
    };
    for (name, uuid) in profiles {
        lists.cache_profile(uuid, &name);
        let Some((_, parts)) = entries.iter().find(|(k, _)| *k == name.to_lowercase()) else {
            warn!("Could not convert user banlist entry for {name}");
            return;
        };
        upsert_ban(&mut lists.banned_players, ban_from(name, Some(uuid), parts));
    }
    lists.save_bans();
    rename_done(dir, OLD_PLAYER_BANS);
}

fn ip_bans(dir: &Path, lists: &mut Lists) {
    let entries = match read_old_bans(dir, OLD_IP_BANS) {
        Ok(e) => e,
        Err(e) => {
            warn!("Could not parse old ip banlist to convert it! {e}");
            return;
        }
    };
    for (ip, parts) in entries {
        upsert_ban(&mut lists.banned_ips, ban_from(ip, None, &parts));
    }
    lists.save_bans();
    rename_done(dir, OLD_IP_BANS);
}

/// Non-empty lines of an old ops or whitelist file, untrimmed as vanilla
/// passes them on.
fn read_names(dir: &Path, name: &str) -> Result<Vec<String>, std::io::Error> {
    Ok(std::fs::read_to_string(dir.join(name))?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

fn ops(dir: &Path, lists: &mut Lists, level: i32, resolver: &mut dyn Resolver) {
    let names = match read_names(dir, OLD_OPS) {
        Ok(n) => n,
        Err(e) => {
            warn!("Could not read old oplist to convert it! {e}");
            return;
        }
    };
    match resolver.resolve(&names) {
        Ok(profiles) => {
            for (name, uuid) in profiles {
                lists.cache_profile(uuid, &name);
                lists.ops.retain(|o| o.uuid != Some(uuid));
                lists.ops.push(Op {
                    uuid: Some(uuid),
                    name,
                    level,
                    bypasses_player_limit: false,
                });
            }
            lists.save_ops();
            rename_done(dir, OLD_OPS);
        }
        Err(e) => error!("Conversion failed, please try again later: {e}"),
    }
}

fn whitelist(dir: &Path, lists: &mut Lists, resolver: &mut dyn Resolver) {
    let names = match read_names(dir, OLD_WHITELIST) {
        Ok(n) => n,
        Err(e) => {
            warn!("Could not read old whitelist to convert it! {e}");
            return;
        }
    };
    match resolver.resolve(&names) {
        Ok(profiles) => {
            for (name, uuid) in profiles {
                lists.cache_profile(uuid, &name);
                lists.whitelist.retain(|n| n.uuid != Some(uuid));
                lists.whitelist.push(Named {
                    uuid: Some(uuid),
                    name,
                });
            }
            lists.save_whitelist();
            rename_done(dir, OLD_WHITELIST);
        }
        Err(e) => error!("Conversion failed, please try again later: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mc-oldlists-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Failing;
    impl Resolver for Failing {
        fn resolve(&mut self, _: &[String]) -> Result<Vec<(String, u128)>, String> {
            Err("down".into())
        }
    }

    #[test]
    fn old_lists_become_json_with_offline_uuids() {
        let dir = temp("all");
        std::fs::write(dir.join(OLD_WHITELIST), "Steve\nAlex\n").unwrap();
        std::fs::write(dir.join(OLD_OPS), "Steve\n").unwrap();
        std::fs::write(
            dir.join(OLD_PLAYER_BANS),
            "# comment\nGriefer|2020-01-02 03:04:05 +0000|Admin|forever|Griefing\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(OLD_IP_BANS),
            "10.0.0.1|2020-01-02 03:04:05 +0000|Admin|2999-01-01 00:00:00 +0000|Spam\n",
        )
        .unwrap();
        convert(&dir, 3, &mut OfflineResolver).unwrap();
        for old in [OLD_WHITELIST, OLD_OPS, OLD_PLAYER_BANS, OLD_IP_BANS] {
            assert!(!dir.join(old).exists());
            assert!(dir.join(format!("{old}.converted")).exists());
        }
        let lists = Lists::load(&dir);
        assert_eq!(lists.whitelist.len(), 2);
        assert_eq!(lists.whitelist[0].uuid, Some(offline_uuid("Steve")));
        assert_eq!(lists.ops[0].level, 3);
        assert!(!lists.ops[0].bypasses_player_limit);
        // Banned names are looked up in lower case, as vanilla does.
        let ban = &lists.banned_players[0];
        assert_eq!(ban.name, "griefer");
        assert_eq!(ban.uuid, Some(offline_uuid("griefer")));
        assert_eq!(ban.source, "Admin");
        assert_eq!(ban.reason, "Griefing");
        assert_eq!(ban.expires, "forever");
        assert_eq!(lists.banned_ips[0].name, "10.0.0.1");
        assert_eq!(
            parse_date(&lists.banned_ips[0].expires),
            parse_date("2999-01-01 00:00:00 +0000")
        );
        // The converted names are in the user cache.
        assert!(lists.usercache.iter().any(|(n, _)| n.name == "Alex"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn existing_json_entries_are_kept() {
        let dir = temp("merge");
        std::fs::write(
            dir.join("whitelist.json"),
            r#"[{"uuid":"00000000-0000-0000-0000-000000000009","name":"Kept"}]"#,
        )
        .unwrap();
        std::fs::write(dir.join(OLD_WHITELIST), "New\n").unwrap();
        convert(&dir, 4, &mut OfflineResolver).unwrap();
        let lists = Lists::load(&dir);
        assert_eq!(lists.whitelist.len(), 2);
        assert_eq!(lists.whitelist[0].name, "Kept");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_lookup_leaves_the_old_file_and_stops_the_start() {
        let dir = temp("fail");
        std::fs::write(dir.join(OLD_OPS), "Steve\n").unwrap();
        assert!(convert(&dir, 4, &mut Failing).is_err());
        assert!(dir.join(OLD_OPS).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_old_files_is_a_no_op() {
        let dir = temp("none");
        convert(&dir, 4, &mut OfflineResolver).unwrap();
        assert!(!dir.join("ops.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
