use std::path::{Path, PathBuf};

use everything_core::config::Config;
use everything_core::sdk::LocalEngine;

fn temp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "everything-watch-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn config_for(root: &Path, base: &Path) -> Config {
    Config {
        roots: vec![root.to_path_buf()],
        exclude: Vec::new(),
        follow_symlinks: false,
        index_hidden: true,
        snapshot_path: base.join("db.eldb"),
        socket_path: base.join("sock"),
        watch_enabled: true,
        watch_limit: Some(2),
        watch_warn_percent: 50,
        debounce_ms: 0,
        rescan_interval_secs: 0,
        snapshot_interval_secs: 0,
        threads: 2,
        max_results: 1000,
        auto_root: false,
        exe_path: None,
    }
}

#[test]
fn watch_limit_caps_and_warns() {
    let base = temp_dir("limit");
    let root = base.join("root");
    for i in 0..8 {
        std::fs::create_dir_all(root.join(format!("dir-{i}"))).unwrap();
    }

    let sdk = LocalEngine::open(config_for(&root, &base), false).unwrap();
    let (count, limit, warn) = sdk.watch_status().expect("watch status");
    assert_eq!(limit, 2);
    assert!(count <= limit);
    assert!(warn, "usage should reach the 50% warning threshold");

    let _ = std::fs::remove_dir_all(&base);
}
