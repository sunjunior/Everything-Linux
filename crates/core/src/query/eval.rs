use regex::RegexBuilder;

use crate::error::{Error, Result};
use crate::index::{fold_name, IndexSnapshot};
use crate::model::{EntryType, QueryOptions, QueryResult, SlotId, SortBy};

use super::parser::{Expr, Field, FilterSpec, KindFilter, MatchMode, Term};
use super::filters::{Cmp, DateField, DateFilter, SizeFilter};

pub type Matcher = Box<dyn Fn(&Ctx<'_>) -> bool + Send + Sync>;

pub struct Ctx<'a> {
    pub snap: &'a IndexSnapshot,
    pub slot: SlotId,
    pub paths: Option<&'a [(Vec<u8>, Vec<u8>)]>,
}

fn target<'a>(field: Field, c: &Ctx<'a>) -> &'a [u8] {
    match field {
        Field::Name => c.snap.name(c.slot),
        Field::Path => c
            .paths
            .and_then(|p| p.get(c.slot as usize))
            .map(|v| v.0.as_slice())
            .unwrap_or_default(),
    }
}

fn folded_target<'a>(field: Field, c: &Ctx<'a>) -> &'a [u8] {
    match field {
        Field::Name => c.snap.folded(c.slot),
        Field::Path => c
            .paths
            .and_then(|p| p.get(c.slot as usize))
            .map(|v| v.1.as_slice())
            .unwrap_or_default(),
    }
}

fn kind_ok(filter: KindFilter, kind: EntryType) -> bool {
    match filter {
        KindFilter::Any => true,
        KindFilter::File => kind != EntryType::Dir,
        KindFilter::Dir => kind == EntryType::Dir,
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn whole_word_ok(hay: &[u8], start: usize, end: usize) -> bool {
    let left_ok = start == 0 || !is_word_byte(hay[start - 1]);
    let right_ok = end >= hay.len() || !is_word_byte(hay[end]);
    left_ok && right_ok
}

fn find_all(hay: &[u8], needle: &[u8]) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return vec![(0, hay.len())];
    }
    let mut out = Vec::new();
    if needle.len() > hay.len() {
        return out;
    }
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            out.push((i, i + needle.len()));
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

fn byte_match(t: &Term, c: &Ctx<'_>, needle_fold: &[u8]) -> bool {
    let hay = if t.case_sensitive {
        target(t.field, c)
    } else {
        folded_target(t.field, c)
    };
    let needle = if t.case_sensitive {
        t.text.as_bytes()
    } else {
        needle_fold
    };
    if needle.is_empty() {
        return true;
    }
    match t.mode {
        MatchMode::Contains => find_all(hay, needle)
            .into_iter()
            .any(|(s, e)| !t.whole_word || whole_word_ok(hay, s, e)),
        MatchMode::StartsWith => {
            hay.starts_with(needle) && (!t.whole_word || whole_word_ok(hay, 0, needle.len()))
        }
        MatchMode::EndsWith => {
            hay.ends_with(needle)
                && (!t.whole_word || whole_word_ok(hay, hay.len() - needle.len(), hay.len()))
        }
        MatchMode::Exact => hay == needle,
        _ => false,
    }
}

fn wildcard_to_regex(s: &str) -> String {
    let mut out = String::from("(?s)^");
    for ch in s.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            c => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    out.push('$');
    out
}

fn cmp_u(c: Cmp, v: u64, target: u64) -> bool {
    match c {
        Cmp::Eq => v == target,
        Cmp::Gt => v > target,
        Cmp::Lt => v < target,
        Cmp::Ge => v >= target,
        Cmp::Le => v <= target,
    }
}

fn cmp_i(c: Cmp, v: i64, target: i64) -> bool {
    match c {
        Cmp::Eq => v == target,
        Cmp::Gt => v > target,
        Cmp::Lt => v < target,
        Cmp::Ge => v >= target,
        Cmp::Le => v <= target,
    }
}

fn compile_filter(kind: KindFilter, filter: &FilterSpec) -> Result<Matcher> {
    match filter {
        FilterSpec::Size(sf) => {
            let sf = *sf;
            Ok(Box::new(move |c: &Ctx<'_>| {
                if !kind_ok(kind, c.snap.kind(c.slot)) {
                    return false;
                }
                let size = c.snap.entry(c.slot).size;
                match sf {
                    SizeFilter::Compare(cmp, v) => cmp_u(cmp, size, v),
                    SizeFilter::Range(a, b) => size >= a && size <= b,
                }
            }))
        }
        FilterSpec::Date(field, df) => {
            let field = *field;
            let df = *df;
            Ok(Box::new(move |c: &Ctx<'_>| {
                if !kind_ok(kind, c.snap.kind(c.slot)) {
                    return false;
                }
                let e = c.snap.entry(c.slot);
                let t = match field {
                    DateField::Created => e.created,
                    DateField::Modified => e.mtime,
                    DateField::Accessed => e.accessed,
                };
                match df {
                    DateFilter::Compare(cmp, v) => cmp_i(cmp, t, v),
                    DateFilter::Range(a, b) => t >= a && t <= b,
                    DateFilter::Within(v) => t >= v,
                }
            }))
        }
        FilterSpec::Ext(exts) => {
            let exts = exts.clone();
            Ok(Box::new(move |c: &Ctx<'_>| {
                if !kind_ok(kind, c.snap.kind(c.slot)) {
                    return false;
                }
                let name = c.snap.name(c.slot);
                let ext = name
                    .iter()
                    .rposition(|&b| b == b'.')
                    .map(|p| &name[p + 1..])
                    .unwrap_or(&[]);
                exts.iter().any(|e| ext.eq_ignore_ascii_case(e.as_bytes()))
            }))
        }
    }
}

fn compile_term(t: &Term) -> Result<Matcher> {
    let t = t.clone();
    if let Some(filter) = &t.filter {
        return compile_filter(t.kind, filter);
    }
    let field = t.field;
    let kind = t.kind;
    let whole_word = t.whole_word;
    let case_sensitive = t.case_sensitive;

    let needle_fold = fold_name(t.text.as_bytes());
    let pattern = match t.mode {
        MatchMode::Regex | MatchMode::Wildcard => {
            let src = if t.mode == MatchMode::Wildcard {
                wildcard_to_regex(&t.text)
            } else {
                t.text.clone()
            };
            let mut b = RegexBuilder::new(&src);
            b.case_insensitive(!case_sensitive);
            let re = b
                .build()
                .map_err(|e| Error::Query(format!("invalid regex: {e}")))?;
            Some(re)
        }
        _ => None,
    };

    Ok(Box::new(move |c: &Ctx<'_>| {
        if !kind_ok(kind, c.snap.kind(c.slot)) {
            return false;
        }
        if let Some(re) = &pattern {
            let s = String::from_utf8_lossy(target(field, c));
            if !whole_word {
                return re.is_match(&s);
            }
            return re.find_iter(&s).any(|m| {
                let bytes = s.as_bytes();
                whole_word_ok(bytes, m.start(), m.end())
            });
        }
        byte_match(&t, c, &needle_fold)
    }))
}

fn compile_expr(e: &Expr) -> Result<Matcher> {
    match e {
        Expr::Term(t) => compile_term(t),
        Expr::And(parts) => {
            let ms: Vec<Matcher> = parts.iter().map(compile_expr).collect::<Result<_>>()?;
            Ok(Box::new(move |c| ms.iter().all(|m| m(c))))
        }
        Expr::Or(parts) => {
            let ms: Vec<Matcher> = parts.iter().map(compile_expr).collect::<Result<_>>()?;
            Ok(Box::new(move |c| ms.iter().any(|m| m(c))))
        }
        Expr::Not(inner) => {
            let m = compile_expr(inner)?;
            Ok(Box::new(move |c| !m(c)))
        }
    }
}

fn uses_path(e: &Expr) -> bool {
    match e {
        Expr::Term(t) => t.field == Field::Path,
        Expr::And(v) | Expr::Or(v) => v.iter().any(uses_path),
        Expr::Not(x) => uses_path(x),
    }
}

pub fn run(
    snap: &IndexSnapshot,
    query: &str,
    opts: QueryOptions,
) -> Result<QueryResult> {
    let start = std::time::Instant::now();
    let expr = super::parser::parse(query, opts)?;
    let matcher = compile_expr(&expr)?;

    let paths = if uses_path(&expr) {
        let mut v = vec![(Vec::new(), Vec::new()); snap.entries.len()];
        let mut buf = Vec::new();
        for slot in 0..snap.entries.len() {
            snap.path(slot as SlotId, &mut buf);
            v[slot].0 = buf.clone();
            v[slot].1 = fold_name(&buf);
        }
        Some(v)
    } else {
        None
    };

    let mut slots: Vec<SlotId> = Vec::new();
    let mut total = 0usize;
    let order: &[u32] = if matches!(opts.sort, SortBy::Name | SortBy::NameDesc) {
        &snap.by_name
    } else {
        &snap.by_name_cs
    };

    for &slot in order {
        let ctx = Ctx {
            snap,
            slot,
            paths: paths.as_deref(),
        };
        if matcher(&ctx) {
            total += 1;
            slots.push(slot);
        }
    }

    match opts.sort {
        SortBy::Name => {}
        SortBy::NameDesc => slots.reverse(),
        SortBy::Path | SortBy::PathDesc => {
            if paths.is_none() {
                let mut p: Vec<(Vec<u8>, Vec<u8>)> = vec![(Vec::new(), Vec::new()); snap.entries.len()];
                let mut buf = Vec::new();
                for slot in 0..snap.entries.len() {
                    snap.path(slot as SlotId, &mut buf);
                    p[slot].0 = buf.clone();
                }
                slots.sort_by(|&a, &b| p[a as usize].0.cmp(&p[b as usize].0));
            } else {
                let p = paths.as_ref().unwrap();
                slots.sort_by(|&a, &b| p[a as usize].0.cmp(&p[b as usize].0));
            }
            if matches!(opts.sort, SortBy::PathDesc) {
                slots.reverse();
            }
        }
        SortBy::Size | SortBy::SizeDesc => {
            slots.sort_by(|&a, &b| snap.entry(a).size.cmp(&snap.entry(b).size));
            if matches!(opts.sort, SortBy::SizeDesc) {
                slots.reverse();
            }
        }
        SortBy::Mtime | SortBy::MtimeDesc => {
            slots.sort_by(|&a, &b| snap.entry(a).mtime.cmp(&snap.entry(b).mtime));
            if matches!(opts.sort, SortBy::MtimeDesc) {
                slots.reverse();
            }
        }
    }

    let truncated = opts.limit > 0 && slots.len() > opts.limit;
    if truncated {
        slots.truncate(opts.limit);
    }
    let _ = snap;

    Ok(QueryResult {
        slots,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        total_matched: total,
        generation: snap.generation,
        truncated,
    })
}
