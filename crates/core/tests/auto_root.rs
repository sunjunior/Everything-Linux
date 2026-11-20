use std::path::{Path, PathBuf};

use everything_core::config::Config;
use everything_core::engine::Engine;
use everything_core::model::QueryOptions;

fn temp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "everything-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn make_root(base: &Path, name: &str, file: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), b"x").unwrap();
    dir
}

fn config_for(root: &Path, exe: &Path, base: &Path) -> Config {
    Config {
        roots: vec![root.to_path_buf()],
        exclude: Vec::new(),
        follow_symlinks: false,
        index_hidden: true,
        snapshot_path: base.join("db.eldb"),
        socket_path: base.join("sock"),
        watch_enabled: false,
        debounce_ms: 0,
        rescan_interval_secs: 0,
        snapshot_interval_secs: 0,
        threads: 2,
        max_results: 1000,
        auto_root: true,
        exe_path: Some(exe.to_path_buf()),
    }
}

fn count(engine: &Engine, query: &str) -> usize {
    engine
        .query(query, QueryOptions::default())
        .unwrap()
        .total_matched
}

#[test]
fn moving_the_frontend_reindexes_the_new_directory() {
    let base = temp_dir("auto-root-move");
    let a = make_root(&base, "A", "a-only.txt");
    let b = make_root(&base, "B", "b-only.txt");
    let old_exe = a.join("everything-gtk");
    let new_exe = b.join("everything-gtk");

    let mut engine = Engine::open(config_for(&a, &old_exe, &base), false).unwrap();
    engine.scan_configured_roots().unwrap();
    assert_eq!(engine.stats().0, 1);
    assert_eq!(count(&engine, "b-only"), 0);

    assert!(engine.set_auto_root(&new_exe).unwrap());
    assert_eq!(engine.config.roots, vec![b.clone()]);
    assert_eq!(engine.config.exe_path.as_deref(), Some(new_exe.as_path()));
    assert_eq!(engine.stats().0, 1);
    assert_eq!(count(&engine, "b-only"), 1);
    assert_eq!(count(&engine, "a-only"), 0);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn apply_exe_root_detects_move_and_is_idempotent() {
    let base = temp_dir("auto-root-config");
    let a = make_root(&base, "A", "a.txt");
    let b = make_root(&base, "B", "b.txt");
    let old_exe = a.join("everything-gtk");
    let new_exe = b.join("everything-gtk");
    let mut cfg = config_for(&a, &old_exe, &base);

    assert!(cfg.apply_exe_root(Some(&new_exe)));
    assert_eq!(cfg.roots, vec![b.clone()]);
    assert_eq!(cfg.exe_path.as_deref(), Some(new_exe.as_path()));
    assert!(!cfg.apply_exe_root(Some(&new_exe)));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn apply_exe_root_respects_manual_mode() {
    let base = temp_dir("auto-root-manual");
    let a = make_root(&base, "A", "a.txt");
    let b = make_root(&base, "B", "b.txt");
    let mut cfg = config_for(&a, &a.join("everything-gtk"), &base);
    cfg.auto_root = false;

    assert!(!cfg.apply_exe_root(Some(&b.join("everything-gtk"))));
    assert_eq!(cfg.roots, vec![a.clone()]);

    let _ = std::fs::remove_dir_all(&base);
}
