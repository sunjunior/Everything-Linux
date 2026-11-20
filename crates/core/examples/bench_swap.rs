use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use everything_core::config::Config;
use everything_core::model::QueryOptions;
use everything_core::sdk::LocalEngine;

fn count(engine: &LocalEngine, q: &str) -> usize {
    engine
        .search_slots(
            q,
            QueryOptions {
                limit: 10,
                ..Default::default()
            },
        )
        .map(|r| r.total_matched)
        .unwrap_or(0)
}

fn main() -> anyhow::Result<()> {
    let base = std::env::temp_dir().join(format!("everything-swap-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    std::fs::copy("/tmp/bench-local/db.eldb", base.join("db.eldb"))?;

    let old_root = PathBuf::from("/home/sunjunior");
    let new_root = old_root.clone();
    let new_exe = old_root.join("everything-gtk-bench");
    let cfg = Config {
        roots: vec![old_root.clone()],
        exclude: Vec::new(),
        follow_symlinks: false,
        index_hidden: true,
        snapshot_path: base.join("db.eldb"),
        socket_path: base.join("sock"),
        watch_enabled: false,
        debounce_ms: 0,
        rescan_interval_secs: 0,
        snapshot_interval_secs: 0,
        threads: 8,
        max_results: 100000,
        auto_root: true,
        exe_path: Some(old_root.join("everything-gtk")),
    };
    let engine = Arc::new(LocalEngine::open(cfg, true)?);
    assert!(count(&engine, "everything-gtk") > 0);

    let stop = Arc::new(AtomicBool::new(false));
    let lat = Arc::new(Mutex::new(Vec::new()));
    let query_thread = {
        let engine = engine.clone();
        let stop = stop.clone();
        let lat = lat.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let t = Instant::now();
                let _ = count(&engine, "zzzzz-nomatch");
                lat.lock().push(t.elapsed().as_secs_f64() * 1000.0);
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };

    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    let changed = engine.set_auto_root(&new_exe)?;
    let switch_ms = t.elapsed().as_secs_f64() * 1000.0;
    std::thread::sleep(Duration::from_millis(100));
    stop.store(true, Ordering::SeqCst);
    let _ = query_thread.join();

    let mut samples = lat.lock().clone();
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples.get(samples.len() / 2).copied().unwrap_or(0.0);
    let p95 = samples
        .get(samples.len() * 95 / 100)
        .copied()
        .unwrap_or(0.0);
    let max = samples.last().copied().unwrap_or(0.0);
    let (files, dirs, generation) = engine.status();

    println!(
        "changed={changed} switch_ms={switch_ms:.1} new_root={}",
        new_root.display()
    );
    println!(
        "queries={} latency_ms median={median:.2} p95={p95:.2} max={max:.2}",
        samples.len()
    );
    println!(
        "after: files={files} dirs={dirs} gen={generation} everything_gtk={} a_only={}",
        count(&engine, "everything-gtk"),
        count(&engine, "a-only")
    );
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}
