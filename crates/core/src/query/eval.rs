use memchr::memmem::Finder;
use rayon::prelude::*;
use regex::RegexBuilder;

use crate::error::{Error, Result};
use crate::index::{fold_into, fold_name};
use crate::model::{EntryType, QueryOptions, QueryResult, SlotId, SortBy};
use crate::store::Store;

use super::filters::{Cmp, DateField, DateFilter, SizeFilter};
use super::parser::{Expr, Field, FilterSpec, KindFilter, MatchMode, Term};

pub type Matcher = Box<dyn Fn(&Ctx<'_>) -> bool + Send + Sync>;

pub struct Ctx<'a> {
    pub store: &'a Store,
    pub slot: SlotId,
    /// (original, folded) path bytes, filled only for path queries.
    pub path: Option<(&'a [u8], &'a [u8])>,
}

fn target<'a>(field: Field, c: &Ctx<'a>) -> &'a [u8] {
    match field {
        Field::Name => c.store.name(c.slot),
        Field::Path => c.path.map(|p| p.0).unwrap_or_default(),
    }
}

fn folded_target<'a>(field: Field, c: &Ctx<'a>) -> &'a [u8] {
    match field {
        Field::Name => c.store.folded(c.slot),
        Field::Path => c.path.map(|p| p.1).unwrap_or_default(),
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
        MatchMode::Contains => {
            if !t.whole_word {
                memchr::memmem::find(hay, needle).is_some()
            } else {
                find_all(hay, needle)
                    .into_iter()
                    .any(|(s, e)| whole_word_ok(hay, s, e))
            }
        }
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

fn wildcard_match(pat: &[u8], hay: &[u8]) -> bool {
    if let Some(v) = wildcard_simple(pat, hay) {
        return v;
    }
    let (mut p, mut h) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while h < hay.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == hay[h]) {
            p += 1;
            h += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            p += 1;
            mark = h;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            h = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

fn wildcard_simple(pat: &[u8], hay: &[u8]) -> Option<bool> {
    if pat == b"*" {
        return Some(true);
    }
    let literal = !pat.iter().any(|&b| b == b'*' || b == b'?');
    if literal {
        return Some(hay == pat);
    }
    if let Some(rest) = pat.strip_prefix(b"*") {
        if !rest.iter().any(|&b| b == b'*' || b == b'?') {
            return Some(hay.ends_with(rest));
        }
    }
    if let Some(rest) = pat.strip_suffix(b"*") {
        if !rest.iter().any(|&b| b == b'*' || b == b'?') {
            return Some(hay.starts_with(rest));
        }
    }
    if pat.len() > 2 && pat[0] == b'*' && pat[pat.len() - 1] == b'*' {
        let mid = &pat[1..pat.len() - 1];
        if !mid.iter().any(|&b| b == b'*' || b == b'?') {
            return Some(memchr::memmem::find(hay, mid).is_some());
        }
    }
    None
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
                if !kind_ok(kind, c.store.kind(c.slot)) {
                    return false;
                }
                let size = c.store.entry(c.slot).map(|e| e.size).unwrap_or(0);
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
                if !kind_ok(kind, c.store.kind(c.slot)) {
                    return false;
                }
                let e = c.store.entry(c.slot);
                let t = match (field, e) {
                    (DateField::Created, Some(e)) => e.created as i64,
                    (DateField::Modified, Some(e)) => e.mtime,
                    (DateField::Accessed, Some(e)) => e.accessed as i64,
                    (_, None) => 0,
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
                if !kind_ok(kind, c.store.kind(c.slot)) {
                    return false;
                }
                let name = c.store.name(c.slot);
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
    let finder: Option<Finder<'static>> = if t.mode == MatchMode::Contains && !whole_word {
        let needle: &[u8] = if case_sensitive {
            t.text.as_bytes()
        } else {
            &needle_fold
        };
        Some(Finder::new(needle).into_owned())
    } else {
        None
    };
    let wildcard_pat = if t.mode == MatchMode::Wildcard && !whole_word {
        Some(if case_sensitive {
            t.text.as_bytes().to_vec()
        } else {
            fold_name(t.text.as_bytes())
        })
    } else {
        None
    };
    let pattern = match t.mode {
        MatchMode::Regex | MatchMode::Wildcard if wildcard_pat.is_none() => {
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
        if !kind_ok(kind, c.store.kind(c.slot)) {
            return false;
        }
        if let Some(pat) = &wildcard_pat {
            let hay = if case_sensitive {
                target(field, c)
            } else {
                folded_target(field, c)
            };
            return wildcard_match(pat, hay);
        }
        if let Some(f) = &finder {
            let hay = if case_sensitive {
                target(field, c)
            } else {
                folded_target(field, c)
            };
            return f.find(hay).is_some();
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

pub fn run(store: &Store, generation: u64, query: &str, opts: QueryOptions) -> Result<QueryResult> {
    let start = std::time::Instant::now();
    let expr = super::parser::parse(query, opts)?;
    let matcher = compile_expr(&expr)?;
    let n = store.len();
    let use_path = uses_path(&expr);

    const CHUNK: usize = 1 << 16;
    let chunks = n.div_ceil(CHUNK);
    let partials: Vec<(Vec<SlotId>, usize)> = (0..chunks)
        .into_par_iter()
        .map(|ci| {
            let start = ci * CHUNK;
            let end = (start + CHUNK).min(n);
            let mut local = Vec::new();
            let mut count = 0usize;
            let mut path_buf = Vec::new();
            let mut fold_buf = Vec::new();
            for slot in start..end {
                let slot = slot as SlotId;
                if !store.is_live(slot) {
                    continue;
                }
                if use_path {
                    store.path(slot, &mut path_buf);
                    fold_into(&path_buf, &mut fold_buf);
                }
                let ctx = Ctx {
                    store,
                    slot,
                    path: if use_path {
                        Some((path_buf.as_slice(), fold_buf.as_slice()))
                    } else {
                        None
                    },
                };
                if matcher(&ctx) {
                    count += 1;
                    local.push(slot);
                }
            }
            (local, count)
        })
        .collect();

    let mut slots: Vec<SlotId> = Vec::new();
    let mut total = 0usize;
    for (mut v, c) in partials {
        total += c;
        slots.append(&mut v);
    }

    let limit = opts.limit;
    if limit > 0 && slots.len() > limit {
        match opts.sort {
            SortBy::Name => {
                slots.select_nth_unstable_by(limit, |&a, &b| {
                    store.folded(a).cmp(store.folded(b)).then_with(|| a.cmp(&b))
                });
            }
            SortBy::NameDesc => {
                slots.select_nth_unstable_by(limit, |&a, &b| {
                    store.folded(b).cmp(store.folded(a)).then_with(|| b.cmp(&a))
                });
            }
            SortBy::Size => {
                slots.select_nth_unstable_by(limit, |&a, &b| {
                    let sa = store.entry(a).map(|e| e.size).unwrap_or(0);
                    let sb = store.entry(b).map(|e| e.size).unwrap_or(0);
                    sa.cmp(&sb)
                });
            }
            SortBy::SizeDesc => {
                slots.select_nth_unstable_by(limit, |&a, &b| {
                    let sa = store.entry(a).map(|e| e.size).unwrap_or(0);
                    let sb = store.entry(b).map(|e| e.size).unwrap_or(0);
                    sb.cmp(&sa)
                });
            }
            SortBy::Mtime => {
                slots.select_nth_unstable_by(limit, |&a, &b| {
                    let ma = store.entry(a).map(|e| e.mtime).unwrap_or(0);
                    let mb = store.entry(b).map(|e| e.mtime).unwrap_or(0);
                    ma.cmp(&mb)
                });
            }
            SortBy::MtimeDesc => {
                slots.select_nth_unstable_by(limit, |&a, &b| {
                    let ma = store.entry(a).map(|e| e.mtime).unwrap_or(0);
                    let mb = store.entry(b).map(|e| e.mtime).unwrap_or(0);
                    mb.cmp(&ma)
                });
            }
            SortBy::Path | SortBy::PathDesc => {}
        }
        if !matches!(opts.sort, SortBy::Path | SortBy::PathDesc) {
            slots.truncate(limit);
        }
    }

    match opts.sort {
        SortBy::Name => slots.sort_unstable_by(|&a, &b| {
            store.folded(a).cmp(store.folded(b)).then_with(|| a.cmp(&b))
        }),
        SortBy::NameDesc => {
            slots.sort_unstable_by(|&a, &b| {
                store.folded(a).cmp(store.folded(b)).then_with(|| a.cmp(&b))
            });
            slots.reverse();
        }
        SortBy::Path | SortBy::PathDesc => {
            let mut buf = Vec::new();
            let mut keyed: Vec<(Vec<u8>, SlotId)> = slots
                .iter()
                .map(|&s| {
                    store.path(s, &mut buf);
                    (buf.clone(), s)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            slots = keyed.into_iter().map(|(_, s)| s).collect();
            if matches!(opts.sort, SortBy::PathDesc) {
                slots.reverse();
            }
        }
        SortBy::Size | SortBy::SizeDesc => {
            slots.sort_by(|&a, &b| {
                let sa = store.entry(a).map(|e| e.size).unwrap_or(0);
                let sb = store.entry(b).map(|e| e.size).unwrap_or(0);
                sa.cmp(&sb)
            });
            if matches!(opts.sort, SortBy::SizeDesc) {
                slots.reverse();
            }
        }
        SortBy::Mtime | SortBy::MtimeDesc => {
            slots.sort_by(|&a, &b| {
                let ma = store.entry(a).map(|e| e.mtime).unwrap_or(0);
                let mb = store.entry(b).map(|e| e.mtime).unwrap_or(0);
                ma.cmp(&mb)
            });
            if matches!(opts.sort, SortBy::MtimeDesc) {
                slots.reverse();
            }
        }
    }

    let truncated = limit > 0 && total > limit;
    if limit > 0 && slots.len() > limit {
        slots.truncate(limit);
    }

    Ok(QueryResult {
        slots,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        total_matched: total,
        generation,
        truncated,
    })
}
