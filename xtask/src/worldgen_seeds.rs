//! Several seeds in one run for `worldgen-ref`, `worldgen-diff` and
//! `worldgen-compare`: `--seeds 1,2,3` or `--seed-count N` (N seeds counting
//! up from `--seed`), and one table at the end.

use anyhow::{Context, Result, bail};

/// The seeds to go through: `list` (`1,2,3`) if given, else `count` seeds
/// from `first`, else just `first`.
pub fn seeds(first: i64, list: Option<&str>, count: Option<usize>) -> Result<Vec<i64>> {
    if let Some(list) = list {
        let seeds = list
            .split(',')
            .map(|s| {
                s.trim()
                    .parse()
                    .with_context(|| format!("`{s}` is not a seed"))
            })
            .collect::<Result<Vec<i64>>>()?;
        if seeds.is_empty() {
            bail!("--seeds needs at least one seed");
        }
        return Ok(seeds);
    }
    match count {
        Some(0) => bail!("--seed-count must be at least 1"),
        Some(n) => Ok((0..n as i64).map(|i| first.wrapping_add(i)).collect()),
        None => Ok(vec![first]),
    }
}

/// What one seed's comparison found: `(equal, total)` of blocks and of biome
/// cells, `None` where the run did not compare them, or why it failed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Row {
    pub seed: i64,
    pub blocks: Option<(usize, usize)>,
    pub biomes: Option<(usize, usize)>,
    pub error: Option<String>,
}

fn percent(part: Option<(usize, usize)>) -> String {
    match part {
        Some((same, total)) if total > 0 => format!("{:.3}%", 100.0 * same as f64 / total as f64),
        _ => "-".into(),
    }
}

/// The summary table: one line per seed (seed, blocks equal, biomes equal)
/// and, for several seeds, the totals over all of them.
pub fn table(rows: &[Row]) -> String {
    let mut out = format!("{:>22} {:>12} {:>12}\n", "seed", "blocks", "biomes");
    for row in rows {
        match &row.error {
            Some(why) => out.push_str(&format!("{:>22} failed: {why}\n", row.seed)),
            None => out.push_str(&format!(
                "{:>22} {:>12} {:>12}\n",
                row.seed,
                percent(row.blocks),
                percent(row.biomes)
            )),
        }
    }
    let sum = |pick: fn(&Row) -> Option<(usize, usize)>| {
        let parts: Vec<(usize, usize)> = rows.iter().filter_map(pick).collect();
        (!parts.is_empty()).then(|| {
            parts
                .into_iter()
                .fold((0, 0), |(a, b), (s, t)| (a + s, b + t))
        })
    };
    if rows.len() > 1 {
        out.push_str(&format!(
            "{:>22} {:>12} {:>12}\n",
            "all",
            percent(sum(|r| r.blocks)),
            percent(sum(|r| r.biomes))
        ));
    }
    out
}

/// Runs `one` for every seed (a failing seed does not stop the others) and
/// prints the table when there is more than one seed; fails if any did.
pub fn each(seeds: &[i64], mut one: impl FnMut(i64) -> Result<Row>) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    for &seed in seeds {
        if seeds.len() > 1 {
            println!("== seed {seed}");
        }
        match one(seed) {
            Ok(mut row) => {
                row.seed = seed;
                rows.push(row);
            }
            Err(err) if seeds.len() > 1 => rows.push(Row {
                seed,
                error: Some(format!("{err:#}")),
                ..Row::default()
            }),
            Err(err) => return Err(err),
        }
    }
    if seeds.len() > 1 {
        println!("\n{}", table(&rows));
        let failed = rows.iter().filter(|r| r.error.is_some()).count();
        if failed > 0 {
            bail!("{failed} of {} seeds failed", seeds.len());
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_come_from_a_list_a_count_or_the_single_seed() {
        assert_eq!(seeds(7, Some("1, 2,-3"), None).unwrap(), vec![1, 2, -3]);
        assert_eq!(seeds(7, None, Some(3)).unwrap(), vec![7, 8, 9]);
        assert_eq!(seeds(7, None, None).unwrap(), vec![7]);
        assert_eq!(seeds(1, Some("1"), Some(5)).unwrap(), vec![1], "list wins");
        assert!(seeds(7, Some("1,x"), None).is_err());
        assert!(seeds(7, None, Some(0)).is_err());
    }

    #[test]
    fn the_table_has_a_line_per_seed_and_totals() {
        let rows = [
            Row {
                seed: 1,
                blocks: Some((99, 100)),
                biomes: Some((10, 10)),
                error: None,
            },
            Row {
                seed: 2,
                blocks: Some((1, 100)),
                biomes: None,
                error: None,
            },
            Row {
                seed: 3,
                error: Some("no world".into()),
                ..Row::default()
            },
        ];
        let text = table(&rows);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5, "header, three seeds, totals");
        assert!(lines[1].contains("99.000%") && lines[1].contains("100.000%"));
        assert!(lines[2].contains("1.000%") && lines[2].ends_with('-'));
        assert!(lines[3].contains("failed: no world"));
        assert!(lines[4].starts_with("                   all"));
        assert!(lines[4].contains("50.000%"), "100 of 200 blocks");
        assert!(
            table(&rows[..1]).lines().count() == 2,
            "no totals for one seed"
        );
    }
}
