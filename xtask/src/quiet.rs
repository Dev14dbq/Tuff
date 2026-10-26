//! A quiet machine for `cargo xtask bench`. macOS cannot reserve cores for a
//! process, so the benchmark makes the machine quiet instead and checks it:
//!
//! - `Guard::new` refuses to run on battery and keeps the Mac awake
//!   (`caffeinate`) while the benchmark runs;
//! - `Guard::wait` holds every run until no Minecraft client is open and the
//!   other processes used at most the allowed background (in cores, from
//!   `top`) on average over the last 10 s;
//! - `Watch` samples the background every second all through a run, leaving
//!   out the benchmark itself and the server it started; a run whose mean
//!   background went over the limit is redone, and the peak is reported.
//!
//! With `--noisy` the checks only report.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// How long `wait` waits for a quiet machine before giving up.
const PATIENCE: Duration = Duration::from_secs(15 * 60);

pub struct Guard {
    background: f64,
    noisy: bool,
    _awake: Option<std::process::Child>,
}

impl Guard {
    pub fn new(background: f64, noisy: bool) -> Result<Self> {
        if cfg!(target_os = "macos") {
            let power = run("pmset", &["-g", "batt"]);
            if !power.contains("AC Power") {
                complain(
                    noisy,
                    "on battery: plug in the power (the CPU slows down on battery)",
                )?;
            }
        }
        let awake = Command::new("caffeinate")
            .args(["-dimsu", "-w", &std::process::id().to_string()])
            .spawn()
            .ok();
        Ok(Self {
            background,
            noisy,
            _awake: awake,
        })
    }

    /// The machine and the rules of the runs, for the table.
    pub fn conditions(&self, cores: usize) -> String {
        let sysctl = |key: &str| run("sysctl", &["-n", key]).trim().to_owned();
        let java = run_stderr("java", &["-version"]);
        let java = java.lines().next().unwrap_or("?").trim();
        let memory = sysctl("hw.memsize").parse::<u64>().unwrap_or(0) >> 30;
        format!(
            "{}, {} GB, macOS {}, {java}; every server limited to {cores} threads \
             (ActiveProcessorCount / MC_THREADS) and 2 GB heap; background allowed \
             {:.2} cores{}",
            sysctl("machdep.cpu.brand_string"),
            memory,
            run("sw_vers", &["-productVersion"]).trim(),
            self.background,
            if self.noisy { " (not enforced)" } else { "" },
        )
    }

    /// Waits until no Minecraft client runs and the background over the
    /// last 10 s averages within the limit.
    pub fn wait(&self) -> Result<()> {
        let start = Instant::now();
        let mut window: std::collections::VecDeque<f64> = Default::default();
        let mut said = String::new();
        let mut last_said = Instant::now();
        let me = [std::process::id()];
        loop {
            let client = minecraft_client();
            let (busy, top) = background(&me);
            window.push_back(busy);
            if window.len() > 10 {
                window.pop_front();
            }
            let mean = window.iter().sum::<f64>() / window.len() as f64;
            let why = match &client {
                Some(c) => format!("a Minecraft client is running ({c})"),
                None if mean > self.background => {
                    format!("{mean:.2} cores busy on average, now {top}")
                }
                None if window.len() < 10 => continue,
                None => return Ok(()),
            };
            if self.noisy {
                eprintln!("bench: not quiet ({why}), running anyway (--noisy)");
                return Ok(());
            }
            if window.len() < 10 && client.is_none() {
                continue;
            }
            // Say why at the start, when the reason changes, and every 30 s.
            let kind = if client.is_some() { "client" } else { "busy" };
            if kind != said || last_said.elapsed() >= Duration::from_secs(30) {
                eprintln!("bench: waiting for a quiet machine: {why}");
                said = kind.to_owned();
                last_said = Instant::now();
            }
            if start.elapsed() > PATIENCE {
                bail!(
                    "the machine did not get quiet in {} min: {why}",
                    PATIENCE.as_secs() / 60
                );
            }
        }
    }
}

fn complain(noisy: bool, what: &str) -> Result<()> {
    if noisy {
        eprintln!("bench: {what} (--noisy: going on)");
        Ok(())
    } else {
        bail!("{what}")
    }
}

/// Samples the background during a run.
pub struct Watch {
    ignore: Arc<Mutex<Vec<u32>>>,
    /// The busiest sample with who it was, and the sum and count of all.
    busiest: Arc<Mutex<(f64, String, f64, usize)>>,
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    pub fn start(_guard: &Guard) -> Self {
        let ignore = Arc::new(Mutex::new(vec![std::process::id()]));
        let busiest = Arc::new(Mutex::new((0.0f64, String::new(), 0.0f64, 0usize)));
        let done = Arc::new(AtomicBool::new(false));
        let thread = {
            let (ignore, busiest, done) = (ignore.clone(), busiest.clone(), done.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let pids = ignore.lock().unwrap().clone();
                    let (busy, top) = background(&pids);
                    let mut max = busiest.lock().unwrap();
                    max.2 += busy;
                    max.3 += 1;
                    if busy > max.0 {
                        max.0 = busy;
                        max.1 = top;
                    }
                }
            })
        };
        Self {
            ignore,
            busiest,
            done,
            thread: Some(thread),
        }
    }

    /// Leaves `pid` (a server of the benchmark) out of the background.
    pub fn ignore(&self, pid: u32) {
        self.ignore.lock().unwrap().push(pid);
    }

    /// Stops watching: the mean background over the run and the busiest
    /// moment (in cores), with who it was then.
    pub fn stop(mut self) -> Background {
        self.done.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let (peak, who, sum, n) = self.busiest.lock().unwrap().clone();
        Background {
            mean: sum / n.max(1) as f64,
            peak,
            who,
        }
    }
}

/// What else ran during a benchmark run, in cores.
#[derive(Clone, Default)]
pub struct Background {
    pub mean: f64,
    pub peak: f64,
    pub who: String,
}

/// The CPU the other processes use over one second, in cores, and the
/// busiest three of them. `top` itself and `ignore` (with their children)
/// are left out.
fn background(ignore: &[u32]) -> (f64, String) {
    let ignore = with_children(ignore);
    let mut rows: Vec<(f64, String)> = Vec::new();
    if cfg!(target_os = "macos") {
        // Two samples one second apart; the second one is the CPU use over
        // that second.
        let out = run(
            "top",
            &[
                "-l",
                "2",
                "-s",
                "1",
                "-n",
                "60",
                "-o",
                "cpu",
                "-stats",
                "pid,cpu,command",
            ],
        );
        let last = out.rsplit("PID").next().unwrap_or("");
        for line in last.lines().skip(1) {
            let mut fields = line.split_whitespace();
            let (Some(pid), Some(cpu)) = (fields.next(), fields.next()) else {
                continue;
            };
            let (Ok(pid), Ok(cpu)) = (pid.parse::<u32>(), cpu.parse::<f64>()) else {
                continue;
            };
            let name: Vec<&str> = fields.collect();
            let name = name.join(" ");
            if ignore.contains(&pid) || name == "top" {
                continue;
            }
            rows.push((cpu / 100.0, name));
        }
    } else {
        // Elsewhere `ps` gives the CPU share over each process's lifetime:
        // rougher, but it still finds a busy neighbour.
        std::thread::sleep(Duration::from_secs(1));
        for line in run("ps", &["-eo", "pid=,pcpu=,comm="]).lines() {
            let mut fields = line.split_whitespace();
            let (Some(Ok(pid)), Some(Ok(cpu))) = (
                fields.next().map(str::parse::<u32>),
                fields.next().map(str::parse::<f64>),
            ) else {
                continue;
            };
            if !ignore.contains(&pid) {
                rows.push((cpu / 100.0, fields.collect::<Vec<_>>().join(" ")));
            }
        }
    }
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    let total = rows.iter().map(|r| r.0).sum();
    let top = rows
        .iter()
        .take(3)
        .map(|(c, n)| format!("{n} {c:.2}"))
        .collect::<Vec<_>>()
        .join(", ");
    (total, top)
}

/// `pids` and every process below them.
fn with_children(pids: &[u32]) -> Vec<u32> {
    let table: Vec<(u32, u32)> = run("ps", &["-Ao", "pid=,ppid="])
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
        })
        .collect();
    let mut all = pids.to_vec();
    let mut i = 0;
    while i < all.len() {
        let parent = all[i];
        for &(pid, ppid) in &table {
            if ppid == parent && !all.contains(&pid) {
                all.push(pid);
            }
        }
        i += 1;
    }
    all
}

/// A running Minecraft client (any launcher), described briefly.
fn minecraft_client() -> Option<String> {
    run("ps", &["-Ao", "pid=,command="])
        .lines()
        .find_map(|line| {
            let client = line.contains("net.minecraft.client.main.Main")
                || (line.contains("java") && line.contains("--gameDir"))
                || (line.contains("java") && line.contains("--accessToken"));
            client.then(|| {
                let pid = line.split_whitespace().next().unwrap_or("?");
                format!("pid {pid}")
            })
        })
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn run_stderr(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
        .unwrap_or_default()
}
