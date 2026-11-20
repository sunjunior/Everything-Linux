use std::time::Instant;

use everything_core::config::Config;
use everything_core::model::{QueryOptions, SortBy};
use everything_core::sdk::LocalEngine;

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn main() -> anyhow::Result<()> {
    let t = Instant::now();
    let mut config = Config::load()?;
    if std::env::var("BENCH_WATCH").ok().as_deref() == Some("0") {
        config.watch_enabled = false;
    }
    let engine = LocalEngine::open(config, true)?;
    let load_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("load_ms={load_ms:.1} rss_kb={}", rss_kb());

    let cases = [
        (
            "*.rs",
            QueryOptions {
                limit: 5000,
                ..Default::default()
            },
        ),
        (
            "main.rs",
            QueryOptions {
                limit: 5000,
                ..Default::default()
            },
        ),
        (
            "zzzzz-nomatch",
            QueryOptions {
                limit: 5000,
                ..Default::default()
            },
        ),
        (
            "path:everything-linux",
            QueryOptions {
                limit: 5000,
                ..Default::default()
            },
        ),
        (
            "*.txt",
            QueryOptions {
                limit: 5000,
                sort: SortBy::SizeDesc,
                ..Default::default()
            },
        ),
    ];
    for &(q, opts) in &cases {
        let _ = engine.search_paths(q, opts)?;
        let mut times = Vec::new();
        let (mut count, mut total) = (0, 0);
        for _ in 0..10 {
            let t = Instant::now();
            let hits = engine.search_paths(q, opts)?;
            let _strings: Vec<String> = hits
                .paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            count = hits.paths.len();
            total = hits.result.total_matched;
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "paths query={q}\tcount={count}\ttotal={total}\tmedian_ms={:.2}\tmin_ms={:.2}",
            times[times.len() / 2],
            times[0]
        );
    }

    for &(q, opts) in &cases {
        let _ = engine.search_slots(q, opts)?;
        let mut times = Vec::new();
        let (mut count, mut total) = (0, 0);
        for _ in 0..10 {
            let t = Instant::now();
            let result = engine.search_slots(q, opts)?;
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            count = result.slots.len();
            total = result.total_matched;
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "slots query={q}\tcount={count}\ttotal={total}\tmedian_ms={:.2}\tmin_ms={:.2}",
            times[times.len() / 2],
            times[0]
        );
    }
    Ok(())
}
