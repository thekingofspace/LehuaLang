use chrono::format::{Item, StrftimeItems};
use chrono::{
    DateTime, Datelike, Days, Local, Months, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike,
    Utc,
};
use mlua::{IntoLua, Lua, MetaMethod, Table, UserData, UserDataMethods, Value};

use super::LibCtx;
use crate::error::LehuaError;

const DEFAULT_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

#[derive(Clone, Copy)]
pub struct Instant {
    pub micros: i64,
}

impl Instant {
    pub fn from_micros(micros: i64) -> mlua::Result<Self> {
        DateTime::from_timestamp_micros(micros)
            .map(|_| Instant { micros })
            .ok_or_else(|| LehuaError::msg("datetime value is out of range").into())
    }

    pub fn from_seconds(t: f64) -> mlua::Result<Self> {
        if !t.is_finite() {
            return Err(LehuaError::msg("timestamp must be a finite number").into());
        }
        Self::from_micros((t * 1e6).round() as i64)
    }

    pub fn now() -> Self {
        Instant {
            micros: Utc::now().timestamp_micros(),
        }
    }

    fn utc(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_micros(self.micros).unwrap()
    }

    pub fn to_seconds(&self) -> f64 {
        self.micros as f64 / 1e6
    }

    pub fn to_iso(&self) -> String {
        self.utc().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    pub fn parse_iso_like(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
            return Self::from_micros(dt.timestamp_micros()).ok();
        }
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
            .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f"))
        {
            return Self::from_micros(naive.and_utc().timestamp_micros()).ok();
        }
        if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
            let naive = date.and_hms_opt(0, 0, 0).unwrap();
            return Self::from_micros(naive.and_utc().timestamp_micros()).ok();
        }
        None
    }
}

pub fn instant_arg(v: &Value) -> Option<Instant> {
    v.as_userdata()
        .and_then(|u| u.borrow::<Instant>().ok())
        .map(|i| *i)
}

fn number_arg(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

impl UserData for Instant {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("toUnix", |_, this, ()| Ok(this.to_seconds()));

        m.add_method("toMillis", |_, this, ()| Ok(this.micros as f64 / 1e3));

        m.add_method("toISO", |_, this, ()| Ok(this.to_iso()));

        m.add_method(
            "format",
            |_, this, (fmt, utc): (Option<String>, Option<bool>)| {
                let fmt = fmt.unwrap_or_else(|| DEFAULT_FORMAT.to_string());
                let items = parse_format(&fmt)?;
                let dt = this.utc();
                let out = if utc.unwrap_or(false) {
                    dt.format_with_items(items.iter()).to_string()
                } else {
                    dt.with_timezone(&Local)
                        .format_with_items(items.iter())
                        .to_string()
                };
                Ok(out)
            },
        );

        m.add_method("components", |lua, this, utc: Option<bool>| {
            let dt = this.utc();
            if utc.unwrap_or(false) {
                components_table(lua, &dt)
            } else {
                components_table(lua, &dt.with_timezone(&Local))
            }
        });

        m.add_method("add", |_, this, (delta, utc): (Table, Option<bool>)| {
            let get = |name: &str| -> mlua::Result<f64> {
                Ok(delta.get::<Option<f64>>(name)?.unwrap_or(0.0))
            };
            let months = (get("years")? as i128) * 12 + get("months")? as i128;
            if months.unsigned_abs() > u32::MAX as u128 {
                return Err(LehuaError::msg("datetime add: months out of range").into());
            }
            let months = months as i64;
            let days = get("days")? as i64;
            let micros = (get("hours")? * 3_600e6
                + get("minutes")? * 60e6
                + get("seconds")? * 1e6
                + get("milliseconds")? * 1e3) as i64;
            let dt = this.utc();
            let out = if utc.unwrap_or(false) {
                shift(dt, months, days, micros)?.timestamp_micros()
            } else {
                shift(dt.with_timezone(&Local), months, days, micros)?.timestamp_micros()
            };
            Instant::from_micros(out)
        });

        m.add_method("offset", |_, this, ()| {
            Ok(this
                .utc()
                .with_timezone(&Local)
                .offset()
                .fix()
                .local_minus_utc())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.to_iso()));

        m.add_meta_method(MetaMethod::Eq, |_, this, other: Value| {
            Ok(instant_arg(&other).map(|o| o.micros == this.micros).unwrap_or(false))
        });

        m.add_meta_function(MetaMethod::Lt, |_, (a, b): (Value, Value)| {
            match (instant_arg(&a), instant_arg(&b)) {
                (Some(a), Some(b)) => Ok(a.micros < b.micros),
                _ => Err(LehuaError::msg("both sides of < must be datetimes").into()),
            }
        });

        m.add_meta_function(MetaMethod::Le, |_, (a, b): (Value, Value)| {
            match (instant_arg(&a), instant_arg(&b)) {
                (Some(a), Some(b)) => Ok(a.micros <= b.micros),
                _ => Err(LehuaError::msg("both sides of <= must be datetimes").into()),
            }
        });

        m.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            let (dt, n) = match (instant_arg(&a), instant_arg(&b)) {
                (Some(_), Some(_)) => {
                    return Err(LehuaError::msg(
                        "cannot add two datetimes; add seconds, or use dt:add{...}",
                    )
                    .into())
                }
                (Some(dt), None) => (dt, number_arg(&b)),
                (None, Some(dt)) => (dt, number_arg(&a)),
                (None, None) => return Err(LehuaError::msg("expected a datetime in +").into()),
            };
            let n = n.ok_or_else(|| {
                LehuaError::msg("a datetime can only be added to a number of seconds")
            })?;
            Instant::from_seconds(dt.to_seconds() + n)
        });

        m.add_meta_function(MetaMethod::Sub, |lua, (a, b): (Value, Value)| {
            let left = instant_arg(&a)
                .ok_or_else(|| LehuaError::msg("the left side of - must be a datetime"))?;
            if let Some(right) = instant_arg(&b) {
                let diff = left.micros as i128 - right.micros as i128;
                return (diff as f64 / 1e6).into_lua(lua);
            }
            let n = number_arg(&b).ok_or_else(|| {
                LehuaError::msg("subtract a datetime or a number of seconds from a datetime")
            })?;
            Instant::from_seconds(left.to_seconds() - n)?.into_lua(lua)
        });
    }
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set("now", lua.create_function(|_, ()| Ok(Instant::now()))?)?;

    t.set(
        "fromUnix",
        lua.create_function(|_, seconds: f64| Instant::from_seconds(seconds))?,
    )?;

    t.set(
        "fromMillis",
        lua.create_function(|_, ms: f64| Instant::from_seconds(ms / 1e3))?,
    )?;

    t.set(
        "fromISO",
        lua.create_function(|_, text: String| {
            Instant::parse_iso_like(&text).ok_or_else(|| {
                LehuaError::msg(format!("datetime.fromISO: could not parse '{text}'")).into()
            })
        })?,
    )?;

    t.set(
        "parse",
        lua.create_function(|_, (text, fmt, utc): (String, String, Option<bool>)| {
            let naive = NaiveDateTime::parse_from_str(&text, &fmt)
                .or_else(|_| {
                    NaiveDate::parse_from_str(&text, &fmt)
                        .map(|d| d.and_hms_opt(0, 0, 0).unwrap())
                })
                .map_err(|e| LehuaError::msg(format!("datetime.parse: {e}")))?;
            naive_to_instant(naive, utc.unwrap_or(false))
        })?,
    )?;

    t.set(
        "fromComponents",
        lua.create_function(|_, (c, utc): (Table, Option<bool>)| {
            let get = |name: &str, default: i64| -> mlua::Result<i64> {
                Ok(c.get::<Option<i64>>(name)?.unwrap_or(default))
            };
            let year = c
                .get::<Option<i64>>("year")?
                .ok_or_else(|| LehuaError::msg("datetime.fromComponents: 'year' is required"))?;
            if !(-262_143..=262_142).contains(&year) {
                return Err(LehuaError::msg("datetime.fromComponents: year out of range").into());
            }
            let date = NaiveDate::from_ymd_opt(
                year as i32,
                get("month", 1)? as u32,
                get("day", 1)? as u32,
            )
            .ok_or_else(|| LehuaError::msg("datetime.fromComponents: invalid date"))?;
            let naive = date
                .and_hms_milli_opt(
                    get("hour", 0)? as u32,
                    get("minute", 0)? as u32,
                    get("second", 0)? as u32,
                    get("millisecond", 0)? as u32,
                )
                .ok_or_else(|| LehuaError::msg("datetime.fromComponents: invalid time"))?;
            naive_to_instant(naive, utc.unwrap_or(false))
        })?,
    )?;

    t.set(
        "isDateTime",
        lua.create_function(|_, v: Value| Ok(instant_arg(&v).is_some()))?,
    )?;

    Ok(Value::Table(t))
}

fn parse_format(fmt: &str) -> mlua::Result<Vec<Item<'_>>> {
    let items: Vec<Item> = StrftimeItems::new(fmt).collect();
    if items.iter().any(|i| matches!(i, Item::Error)) {
        return Err(LehuaError::msg(format!("invalid datetime format string '{fmt}'")).into());
    }
    Ok(items)
}

fn naive_to_instant(naive: NaiveDateTime, utc: bool) -> mlua::Result<Instant> {
    let micros = if utc {
        naive.and_utc().timestamp_micros()
    } else {
        Local
            .from_local_datetime(&naive)
            .earliest()
            .ok_or_else(|| {
                LehuaError::msg("that local time does not exist (daylight saving gap)")
            })?
            .timestamp_micros()
    };
    Instant::from_micros(micros)
}

fn shift<Tz: TimeZone>(
    dt: DateTime<Tz>,
    months: i64,
    days: i64,
    micros: i64,
) -> mlua::Result<DateTime<Tz>> {
    let out_of_range = || mlua::Error::from(LehuaError::msg("datetime add: result out of range"));
    let mut dt = if months >= 0 {
        dt.checked_add_months(Months::new(months as u32))
    } else {
        dt.checked_sub_months(Months::new((-months) as u32))
    }
    .ok_or_else(out_of_range)?;
    dt = if days >= 0 {
        dt.checked_add_days(Days::new(days as u64))
    } else {
        dt.checked_sub_days(Days::new((-days) as u64))
    }
    .ok_or_else(out_of_range)?;
    dt.checked_add_signed(chrono::Duration::microseconds(micros))
        .ok_or_else(out_of_range)
}

fn components_table<Tz: TimeZone>(lua: &Lua, dt: &DateTime<Tz>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("year", dt.year())?;
    t.set("month", dt.month())?;
    t.set("day", dt.day())?;
    t.set("hour", dt.hour())?;
    t.set("minute", dt.minute())?;
    t.set("second", dt.second())?;
    t.set("millisecond", dt.timestamp_subsec_millis())?;
    t.set("weekday", dt.weekday().num_days_from_sunday() + 1)?;
    t.set("yearday", dt.ordinal())?;
    t.set("offset", dt.offset().fix().local_minus_utc())?;
    Ok(t)
}
