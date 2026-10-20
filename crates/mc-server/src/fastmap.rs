//! The order vanilla lists names kept in an open hash table (scoreboard
//! objectives, score holders, teams, team members): keys are placed by a
//! mixed `String.hashCode` with linear probing, the table doubles when it
//! fills and halves when it empties, and lists walk it front to back or
//! back to front. Only the keys are kept here; values live elsewhere.

#[derive(Clone, Debug)]
pub struct Order {
    table: Vec<Option<String>>,
    size: usize,
    fill: f32,
    min: usize,
    forward: bool,
}

fn java_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
}

fn mix(s: &str) -> usize {
    let x = (java_hash(s) as u32).wrapping_mul(0x9E37_79B9);
    (x ^ (x >> 16)) as usize
}

fn table_size(expected: usize, fill: f32) -> usize {
    ((expected as f32 / fill).ceil() as usize)
        .next_power_of_two()
        .max(2)
}

fn max_fill(n: usize, fill: f32) -> usize {
    ((n as f32 * fill).ceil() as usize).min(n - 1)
}

impl Order {
    /// A table made for 16 keys with load factor `fill`, listed
    /// `forward` or backwards.
    pub fn new(fill: f32, forward: bool) -> Self {
        let n = table_size(16, fill);
        Order {
            table: vec![None; n],
            size: 0,
            fill,
            min: n,
            forward,
        }
    }

    fn mask(&self) -> usize {
        self.table.len() - 1
    }

    fn find(&self, key: &str) -> Option<usize> {
        let mut p = mix(key) & self.mask();
        loop {
            match &self.table[p] {
                None => return None,
                Some(k) if k == key => return Some(p),
                Some(_) => p = (p + 1) & self.mask(),
            }
        }
    }

    pub fn insert(&mut self, key: &str) {
        if self.find(key).is_some() {
            return;
        }
        let mut p = mix(key) & self.mask();
        while self.table[p].is_some() {
            p = (p + 1) & self.mask();
        }
        self.table[p] = Some(key.to_owned());
        let full = self.size >= max_fill(self.table.len(), self.fill);
        self.size += 1;
        if full {
            self.rehash(table_size(self.size + 1, self.fill));
        }
    }

    pub fn remove(&mut self, key: &str) {
        let Some(mut pos) = self.find(key) else {
            return;
        };
        self.size -= 1;
        let mask = self.mask();
        // Moves later keys of the run back into the gap.
        loop {
            let last = pos;
            pos = (last + 1) & mask;
            loop {
                let Some(k) = &self.table[pos] else {
                    self.table[last] = None;
                    return self.shrink();
                };
                let slot = mix(k) & mask;
                let stays = if last <= pos {
                    last >= slot || slot > pos
                } else {
                    last >= slot && slot > pos
                };
                if stays {
                    break;
                }
                pos = (pos + 1) & mask;
            }
            self.table[last] = self.table[pos].take();
        }
    }

    fn shrink(&mut self) {
        let n = self.table.len();
        if n > self.min && self.size < max_fill(n, self.fill) / 4 && n > 16 {
            self.rehash(n / 2);
        }
    }

    fn rehash(&mut self, n: usize) {
        let mut table = vec![None; n];
        for key in self.table.iter().rev().flatten() {
            let mut p = mix(key) & (n - 1);
            while table[p].is_some() {
                p = (p + 1) & (n - 1);
            }
            table[p] = Some(key.clone());
        }
        self.table = table;
    }

    /// The keys in the order vanilla lists them.
    pub fn keys(&self) -> Vec<&str> {
        let keys = self.table.iter().flatten().map(String::as_str);
        if self.forward {
            keys.collect()
        } else {
            keys.rev().collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_as_vanilla() {
        // Objectives (seen on a vanilla server).
        let mut o = Order::new(0.5, false);
        for k in [
            "obj",
            "Obj2",
            "looooooooooooooooooooooooooooooooong",
            "a.b-c+d_e",
            "hp",
            "trig",
            "lvl",
            "xpo",
            "deaths",
            "kills",
            "tk",
            "tkills",
            "kbt",
            "jumps",
            "mined",
            "brk",
            "kld",
            "ac",
            "ar",
            "fd",
        ] {
            o.insert(k);
        }
        assert_eq!(
            o.keys(),
            [
                "ar",
                "obj",
                "looooooooooooooooooooooooooooooooong",
                "brk",
                "tkills",
                "jumps",
                "fd",
                "kbt",
                "Obj2",
                "kld",
                "hp",
                "xpo",
                "tk",
                "a.b-c+d_e",
                "mined",
                "kills",
                "deaths",
                "trig",
                "lvl",
                "ac"
            ]
        );
        // Teams.
        let mut t = Order::new(0.75, false);
        for k in [
            "red",
            "blue",
            "a.b-c+d_e",
            "looooooooooooooooooooooooooooooooong",
        ] {
            t.insert(k);
        }
        assert_eq!(
            t.keys(),
            [
                "blue",
                "looooooooooooooooooooooooooooooooong",
                "a.b-c+d_e",
                "red"
            ]
        );
        t.remove("blue");
        assert_eq!(t.keys()[2], "red");
    }
}

/// The order of a Java `HashSet` of names: by hash bucket, then as added.
/// The table only grows.
#[derive(Clone, Debug)]
pub struct JavaSet {
    keys: Vec<String>,
    cap: usize,
}

impl Default for JavaSet {
    fn default() -> Self {
        JavaSet {
            keys: Vec::new(),
            cap: 16,
        }
    }
}

impl JavaSet {
    pub fn insert(&mut self, key: &str) {
        if self.keys.iter().any(|k| k == key) {
            return;
        }
        self.keys.push(key.to_owned());
        while self.keys.len() * 4 > self.cap * 3 {
            self.cap *= 2;
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.keys.retain(|k| k != key);
    }

    pub fn keys(&self) -> Vec<&str> {
        let bucket = |k: &str| {
            let h = java_hash(k) as u32;
            ((h ^ (h >> 16)) as usize) & (self.cap - 1)
        };
        let mut keys: Vec<&str> = self.keys.iter().map(String::as_str).collect();
        keys.sort_by_key(|k| bucket(k));
        keys
    }
}
