use std::path::{Path, PathBuf};

use everything_core::config::Config;
use everything_core::model::QueryOptions;
use everything_core::sdk::LocalEngine;

fn temp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "everything-sdk-{tag}-{}-{}",
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

#[test]
fn sdk_searches_and_switches_root_in_process() {
    let base = temp_dir("search");
    let a = make_root(&base, "A", "a-only.txt");
    let b = make_root(&base, "B", "b-only.txt");
    let old_exe = a.join("everything-gtk");
    let new_exe = b.join("everything-gtk");

    let sdk = LocalEngine::open(config_for(&a, &old_exe, &base), false).unwrap();
    let hits = sdk.search_paths("a-only", QueryOptions::default()).unwrap();
    assert_eq!(hits.result.total_matched, 1);
    assert_eq!(hits.paths, vec![a.join("a-only.txt")]);
    assert_eq!(sdk.status().0, 1);

    assert!(sdk.set_auto_root(&new_exe).unwrap());
    let hits = sdk.search_paths("b-only", QueryOptions::default()).unwrap();
    assert_eq!(hits.paths, vec![b.join("b-only.txt")]);
    assert_eq!(
        sdk.search_paths("a-only", QueryOptions::default())
            .unwrap()
            .paths
            .len(),
        0
    );
    assert_eq!(sdk.status().0, 1);

    let _ = std::fs::remove_dir_all(&base);
}
