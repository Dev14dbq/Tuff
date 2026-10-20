//! Numeric providers written out in a command (`/compute`): the arithmetic
//! ones, evaluated as vanilla does with ints or floats.

use serde_json::Value;

use crate::util::random_u64;

/// A value in the arithmetic of one provider kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Int(i32),
    Float(f32),
}

pub enum Failure {
    /// Something the provider cannot do (shown as the exception text).
    Invalid(String),
    /// A kind this evaluator does not know.
    Unknown,
}

type Out = Result<Num, Failure>;

fn invalid<T>(m: &str) -> Result<T, Failure> {
    Err(Failure::Invalid(m.to_owned()))
}

fn float_of(n: Num) -> f32 {
    match n {
        Num::Int(i) => i as f32,
        Num::Float(f) => f,
    }
}

fn check(f: f32) -> Out {
    if f.is_infinite() {
        invalid("Infinity")
    } else if f.is_nan() {
        invalid("NaN")
    } else {
        Ok(Num::Float(f))
    }
}

/// Evaluates `v`; `float` says which kind the provider is.
pub fn eval(v: &Value, float: bool) -> Out {
    if let Some(n) = v.as_f64() {
        return Ok(if float {
            Num::Float(n as f32)
        } else {
            Num::Int(n as i32)
        });
    }
    let Some(kind) = v["type"]
        .as_str()
        .map(|t| t.trim_start_matches("minecraft:"))
    else {
        return Err(Failure::Unknown);
    };
    let sub = |k: &str| eval(&v[k], float);
    let list = |k: &str| -> Result<Vec<Num>, Failure> {
        v[k].as_array()
            .ok_or(Failure::Unknown)?
            .iter()
            .map(|e| eval(e, float))
            .collect()
    };
    let int2 = |f: &dyn Fn(i32, i32) -> Option<i32>, g: &dyn Fn(f32, f32) -> f32| -> Out {
        match (sub("left")?, sub("right")?) {
            (Num::Int(a), Num::Int(b)) => f(a, b)
                .map(Num::Int)
                .map_or_else(|| invalid("/ by zero"), Ok),
            (a, b) => check(g(float_of(a), float_of(b))),
        }
    };
    Ok(match kind {
        "constant" => return eval(&v["value"], float),
        "add" | "mul" => {
            let items = list("inputs")?;
            let add = kind == "add";
            if items.iter().all(|n| matches!(n, Num::Int(_))) {
                let mut acc: i32 = if add { 0 } else { 1 };
                for n in items {
                    if let Num::Int(i) = n {
                        acc = if add {
                            acc.wrapping_add(i)
                        } else {
                            acc.wrapping_mul(i)
                        };
                    }
                }
                Num::Int(acc)
            } else {
                let mut acc: f32 = if add { 0.0 } else { 1.0 };
                for n in items {
                    acc = if add {
                        acc + float_of(n)
                    } else {
                        acc * float_of(n)
                    };
                }
                return check(acc);
            }
        }
        "sub" => return int2(&|a, b| Some(a.wrapping_sub(b)), &|a, b| a - b),
        "div" => {
            return int2(&|a, b| (b != 0).then(|| a.wrapping_div(b)), &|a, b| a / b);
        }
        "mod" => return int2(&|a, b| (b != 0).then(|| a.wrapping_rem(b)), &|a, b| a % b),
        "floor_div" => {
            return int2(
                &|a, b| {
                    (b != 0).then(|| {
                        a.wrapping_div_euclid(b) - i32::from(b < 0 && a.rem_euclid(b) != 0)
                    })
                },
                &|a, b| (a / b).floor(),
            );
        }
        "floor_mod" => {
            return int2(&|a, b| (b != 0).then(|| ((a % b) + b) % b), &|a, b| {
                a - (a / b).floor() * b
            });
        }
        "min" | "max" => {
            let items = list("inputs")?;
            let first = *items.first().ok_or(Failure::Unknown)?;
            let pick = |a: Num, b: Num| {
                let (x, y) = (float_of(a), float_of(b));
                if (kind == "min") == (y < x) { b } else { a }
            };
            items.into_iter().fold(first, pick)
        }
        "avg" => {
            let items = list("inputs")?;
            if items.is_empty() {
                return Err(Failure::Unknown);
            }
            let sum: f32 = items.iter().map(|n| float_of(*n)).sum();
            let avg = sum / items.len() as f32;
            if float {
                Num::Float(avg)
            } else {
                Num::Int(avg as i32)
            }
        }
        "abs" => match sub("input")? {
            Num::Int(i) => Num::Int(i.wrapping_abs()),
            Num::Float(f) => Num::Float(f.abs()),
        },
        "negate" => match sub("input")? {
            Num::Int(i) => Num::Int(i.wrapping_neg()),
            Num::Float(f) => Num::Float(-f),
        },
        "pow" => {
            let r = f64::from(float_of(sub("base")?)).powf(f64::from(float_of(sub("exponent")?)));
            if float {
                return check(r as f32);
            } else {
                Num::Int(r as i32)
            }
        }
        "sqrt" => return check(float_of(sub("input")?).sqrt()),
        "sin" => return check(float_of(sub("input")?).sin()),
        "floor" => return check(float_of(sub("input")?).floor()),
        "round" => return check(float_of(sub("input")?).round()),
        "from_int" => Num::Float(float_of(sub("input")?)),
        "from_float" => Num::Int(float_of(eval(&v["input"], true)?).floor() as i32),
        "length" => {
            let sum: f32 = list("inputs")?.iter().map(|n| float_of(*n).powi(2)).sum();
            return check(sum.sqrt());
        }
        "uniform" => {
            let (lo, hi) = (sub("min")?, sub("max")?);
            match (lo, hi) {
                (Num::Int(a), Num::Int(b)) if !float => {
                    let span = i64::from(b) - i64::from(a) + 1;
                    if span <= 0 {
                        return invalid("bound must be positive");
                    }
                    Num::Int((i64::from(a) + (random_u64() % span as u64) as i64) as i32)
                }
                (a, b) => {
                    let (a, b) = (float_of(a), float_of(b));
                    Num::Float(a + (random_u64() as f64 / u64::MAX as f64) as f32 * (b - a))
                }
            }
        }
        "binomial" => {
            let n = float_of(sub("n")?) as i32;
            let p = f64::from(float_of(sub("p")?));
            let hits = (0..n.max(0))
                .filter(|_| (random_u64() as f64 / u64::MAX as f64) < p)
                .count() as i32;
            if float {
                Num::Float(hits as f32)
            } else {
                Num::Int(hits)
            }
        }
        _ => return Err(Failure::Unknown),
    })
}
