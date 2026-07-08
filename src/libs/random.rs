use std::cell::RefCell;

use mlua::{MetaMethod, Table, UserData, UserDataMethods, Value};

use super::LibCtx;
use crate::error::LehuaError;

#[derive(Clone)]
struct Rng {
    s: [u64; 4],
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

impl Rng {
    fn from_seed(seed: u64) -> Self {
        let mut state = seed;
        let mut s = [0u64; 4];
        for slot in &mut s {
            *slot = splitmix64(&mut state);
        }
        Rng { s }
    }

    fn from_entropy() -> mlua::Result<Self> {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes)
            .map_err(|e| LehuaError::msg(format!("random source failed: {e}")))?;
        Ok(Rng::from_seed(u64::from_le_bytes(bytes)))
    }

    fn next_u64(&mut self) -> u64 {
        let result = self.s[0]
            .wrapping_add(self.s[3])
            .rotate_left(23)
            .wrapping_add(self.s[0]);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    fn next_below(&mut self, bound: u64) -> u64 {
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let x = self.next_u64();
            if x >= threshold {
                return x % bound;
            }
        }
    }

    fn next_integer(&mut self, min: i64, max: i64) -> i64 {
        let span = (max as i128 - min as i128 + 1) as u128;
        if span > u64::MAX as u128 {
            return self.next_u64() as i64;
        }
        min.wrapping_add(self.next_below(span as u64) as i64)
    }
}

pub struct RandomObj {
    rng: RefCell<Rng>,
}

fn seed_to_u64(seed: f64) -> u64 {
    seed.to_bits()
}

impl UserData for RandomObj {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("NextInteger", |_, this, (min, max): (i64, i64)| {
            if min > max {
                return Err(LehuaError::msg("NextInteger: min must not be greater than max").into());
            }
            Ok(this.rng.borrow_mut().next_integer(min, max))
        });

        m.add_method(
            "NextNumber",
            |_, this, (min, max): (Option<f64>, Option<f64>)| {
                let x = this.rng.borrow_mut().next_f64();
                match (min, max) {
                    (None, None) => Ok(x),
                    (Some(min), Some(max)) => {
                        if min > max {
                            return Err(LehuaError::msg(
                                "NextNumber: min must not be greater than max",
                            )
                            .into());
                        }
                        Ok(min + x * (max - min))
                    }
                    _ => Err(LehuaError::msg(
                        "NextNumber takes no arguments or both min and max",
                    )
                    .into()),
                }
            },
        );

        m.add_method("NextBoolean", |_, this, chance: Option<f64>| {
            let p = chance.unwrap_or(0.5);
            Ok(this.rng.borrow_mut().next_f64() < p)
        });

        m.add_method("NextBytes", |lua, this, n: usize| {
            let mut out = Vec::new();
            out.try_reserve_exact(n)
                .map_err(|_| LehuaError::msg(format!("NextBytes: {n} bytes is too much")))?;
            let mut rng = this.rng.borrow_mut();
            while out.len() < n {
                let chunk = rng.next_u64().to_le_bytes();
                let take = (n - out.len()).min(8);
                out.extend_from_slice(&chunk[..take]);
            }
            lua.create_string(out)
        });

        m.add_method("Shuffle", |_, this, t: Table| {
            let len = t.raw_len();
            let mut rng = this.rng.borrow_mut();
            for i in (2..=len).rev() {
                let j = rng.next_below(i as u64) as usize + 1;
                if i != j {
                    let a: Value = t.raw_get(i)?;
                    let b: Value = t.raw_get(j)?;
                    t.raw_set(i, b)?;
                    t.raw_set(j, a)?;
                }
            }
            Ok(t)
        });

        m.add_method("Choice", |_, this, t: Table| {
            let len = t.raw_len();
            if len == 0 {
                return Ok(Value::Nil);
            }
            let i = this.rng.borrow_mut().next_below(len as u64) as usize + 1;
            t.raw_get::<Value>(i)
        });

        m.add_method("Clone", |_, this, ()| {
            Ok(RandomObj {
                rng: RefCell::new(this.rng.borrow().clone()),
            })
        });

        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Random"));
    }
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "new",
        lua.create_function(|_, seed: Option<f64>| {
            let rng = match seed {
                Some(s) => Rng::from_seed(seed_to_u64(s)),
                None => Rng::from_entropy()?,
            };
            Ok(RandomObj {
                rng: RefCell::new(rng),
            })
        })?,
    )?;

    Ok(Value::Table(t))
}
