use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Gt,
    Lt,
    Ge,
    Le,
}

#[derive(Clone, Copy, Debug)]
pub enum SizeFilter {
    Compare(Cmp, u64),
    Range(u64, u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateField {
    Created,
    Modified,
    Accessed,
}

#[derive(Clone, Copy, Debug)]
pub enum DateFilter {
    Compare(Cmp, i64),
    Range(i64, i64),
    Within(i64),
}

fn unit(s: &str) -> Option<u64> {
    match s.to_ascii_lowercase().as_str() {
        "" | "b" | "byte" | "bytes" => Some(1),
        "k" | "kb" | "kib" => Some(1024),
        "m" | "mb" | "mib" => Some(1024 * 1024),
        "g" | "gb" | "gib" => Some(1024 * 1024 * 1024),
        "t" | "tb" | "tib" => Some(1024u64 * 1024 * 1024 * 1024),
        _ => None,
    }
}

fn parse_bytes(s: &str) -> Result<u64> {
    let s = s.trim().replace(',', "");
    let pos = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, suffix) = s.split_at(pos);
    let mult = unit(suffix.trim()).ok_or_else(|| Error::Query(format!("bad size unit: {suffix}")))?;
    let value: f64 = num
        .parse()
        .map_err(|_| Error::Query(format!("bad size value: {s}")))?;
    Ok((value * mult as f64) as u64)
}

fn split_cmp(s: &str) -> (Cmp, &str) {
    for (p, c) in [
        (">=", Cmp::Ge),
        ("<=", Cmp::Le),
        (">", Cmp::Gt),
        ("<", Cmp::Lt),
        ("=", Cmp::Eq),
    ] {
        if let Some(rest) = s.strip_prefix(p) {
            return (c, rest.trim());
        }
    }
    (Cmp::Eq, s.trim())
}

pub fn parse_size(s: &str) -> Result<SizeFilter> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once("..").or_else(|| s.split_once('-')) {
        return Ok(SizeFilter::Range(parse_bytes(a)?, parse_bytes(b)?));
    }
    let (cmp, rest) = split_cmp(s);
    Ok(SizeFilter::Compare(cmp, parse_bytes(rest)?))
}

// Days from civil date (Howard Hinnant), UTC midnight.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_date_value(s: &str) -> Result<i64> {
    let s = s.trim();
    let now = now_secs();
    match s.to_ascii_lowercase().as_str() {
        "today" => Ok(now - now.rem_euclid(86400)),
        "yesterday" => Ok(now - now.rem_euclid(86400) - 86400),
        _ => {
            // YYYY-MM-DD
            let parts: Vec<&str> = s.split('-').collect();
            if parts.len() == 3 {
                let y: i64 = parts[0].parse().map_err(|_| Error::Query("bad year".into()))?;
                let m: i64 = parts[1].parse().map_err(|_| Error::Query("bad month".into()))?;
                let d: i64 = parts[2].parse().map_err(|_| Error::Query("bad day".into()))?;
                if (1..=12).contains(&m) && (1..=31).contains(&d) {
                    return Ok(days_from_civil(y, m, d) * 86400);
                }
            }
            // relative: 3d / 2w / 1m / 1y
            let pos = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let (num, suffix) = s.split_at(pos);
            let n: i64 = num.parse().map_err(|_| Error::Query("bad date".into()))?;
            let mult = match suffix.to_ascii_lowercase().as_str() {
                "d" | "day" | "days" => 86400,
                "w" | "week" | "weeks" => 7 * 86400,
                "m" | "month" | "months" => 30 * 86400,
                "y" | "year" | "years" => 365 * 86400,
                "h" | "hour" | "hours" => 3600,
                _ => return Err(Error::Query(format!("bad date suffix: {suffix}"))),
            };
            Ok(now - n * mult)
        }
    }
}

pub fn parse_date(s: &str) -> Result<DateFilter> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once("..") {
        return Ok(DateFilter::Range(parse_date_value(a)?, parse_date_value(b)?));
    }
    if let Some(rest) = s.strip_prefix("last") {
        return Ok(DateFilter::Within(parse_date_value(rest)?));
    }
    let (cmp, rest) = split_cmp(s);
    if cmp == Cmp::Eq && rest.starts_with("within:") {
        return Ok(DateFilter::Within(parse_date_value(
            rest.trim_start_matches("within:"),
        )?));
    }
    Ok(DateFilter::Compare(cmp, parse_date_value(rest)?))
}

pub fn parse_ext(s: &str) -> Vec<String> {
    s.split(|c| c == ';' || c == ',' || c == '|')
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}
