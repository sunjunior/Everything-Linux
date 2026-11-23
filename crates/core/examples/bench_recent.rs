use std::path::PathBuf;
use std::time::Instant;

use everything_core::config::Config;
use everything_core::engine::Engine;
use everything_core::model::{QueryOptions, SortBy};

fn median(mut v: Vec<f64>) -> (f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[v.len() / 2], v[0])
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let snapshot = args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: bench_recent <snapshot.eldb> [root]")
    })?;
    let root = args
        .next()
        .or_else(|| std::env::var("HOME").ok())
        .ok_or_else(|| anyhow::anyhow!("usage: bench_recent <snapshot.eldb> [root]"))?;
    let cfg = Config {
        roots: vec![PathBuf::from(root)],
        exclude: Vec::new(),
        follow_symlinks: false,
        index_hidden: true,
        snapshot_path: PathBuf::from(snapshot),
        socket_path: PathBuf::from("/tmp/bench-local/sock"),
        watch_enabled: false,
        watch_limit: None,
        watch_warn_percent: 80,
        debounce_ms: 80,
        rescan_interval_secs: 0,
        snapshot_interval_secs: 0,
        threads: 8,
        max_results: 100_000,
        auto_root: false,
        exe_path: None,
    };
    let engine = Engine::open(cfg, true)?;
    let (files, dirs, generation) = engine.stats();
    println!("files={files} dirs={dirs} generation={generation}");

    for q in ["*.rs", "main.rs", "zzzzz-nomatch", "path:everything-linux"] {
        for (label, sort) in [("name", SortBy::Name), ("recent", SortBy::RecentDesc)] {
            let opts = QueryOptions {
                limit: 5000,
                sort,
                ..Default::default()
            };
            let _ = engine.search_slots(q, opts)?;
            let mut times = Vec::new();
            let mut count = 0;
            let mut total = 0;
            for _ in 0..20 {
                let t = Instant::now();
                let result = engine.search_slots(q, opts)?;
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                count = result.slots.len();
                total = result.total_matched;
            }
            let (med, min) = median(times);
            println!("query={q}\tsort={label}\tcount={count}\ttotal={total}\tmedian_ms={med:.2}\tmin_ms={min:.2}");
        }
    }
    Ok(())
}
