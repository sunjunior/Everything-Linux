use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub roots: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub follow_symlinks: bool,
    pub index_hidden: bool,
    pub snapshot_path: PathBuf,
    pub socket_path: PathBuf,
    pub watch_enabled: bool,
    pub debounce_ms: u64,
    pub rescan_interval_secs: u64,
    pub snapshot_interval_secs: u64,
    pub threads: usize,
    pub max_results: usize,
    /// Whether roots are auto-managed from the executable location.
    pub auto_root: bool,
    /// Executable path used when the auto roots were last resolved.
    pub exe_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            exclude: Vec::new(),
            follow_symlinks: false,
            index_hidden: true,
            snapshot_path: dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("everything-linux/db.eldb"),
            socket_path: dirs::runtime_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("everything-linux.sock"),
            watch_enabled: true,
            debounce_ms: 80,
            rescan_interval_secs: 600,
            snapshot_interval_secs: 5,
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            max_results: 100_000,
            auto_root: true,
            exe_path: None,
        }
    }
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path
}

impl Config {
    /// Index roots configured by the user, falling back to the directory that
    /// contains the executable. No absolute path is baked into the binary.
    pub fn effective_roots(&self) -> Vec<PathBuf> {
        if !self.roots.is_empty() {
            return self.roots.clone();
        }
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        vec![exe_dir]
    }

    /// Keep the default root in sync with the executable location.
    /// Returns true if roots were (re)set, which requires a fresh index.
    /// Keep the default root in sync with the front-end executable location.
    /// `frontend` is the GUI/CLI the user actually launched (its directory is
    /// the default root). Returns true if roots were (re)set.
    pub fn apply_exe_root(&mut self, frontend: Option<&std::path::Path>) -> bool {
        if !self.auto_root {
            return false;
        }
        let (exe, dir) = match frontend {
            Some(p) => {
                let dir = p
                    .parent()
                    .map(|d| d.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));
                (p.to_path_buf(), dir)
            }
            None => {
                // Standalone daemon: default to its working directory.
                let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                (dir.join("everythingd"), dir)
            }
        };
        if self.exe_path.as_ref() == Some(&exe) && self.roots == vec![dir.clone()] {
            return false;
        }
        self.roots = vec![dir];
        self.exe_path = Some(exe);
        true
    }

    /// Mark roots as explicitly configured (no longer following the executable).
    pub fn mark_manual_roots(&mut self) {
        self.auto_root = false;
        self.exe_path = None;
    }

    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("everything-linux/config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Config(format!("read {}: {e}", path.display())))?;
        let mut cfg: Self = toml::from_str(&text)
            .map_err(|e| Error::Config(format!("parse {}: {e}", path.display())))?;
        // A hand-edited config with roots but no recorded exe path is an
        // explicit user choice and must not be replaced when the binary moves.
        if !cfg.roots.is_empty() && cfg.exe_path.is_none() {
            cfg.auto_root = false;
        }
        cfg.roots = cfg.roots.into_iter().map(expand_tilde).collect();
        cfg.snapshot_path = expand_tilde(cfg.snapshot_path);
        cfg.socket_path = expand_tilde(cfg.socket_path);
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("serialize: {e}")))?;
        std::fs::write(&path, text)?;
        Ok(())
    }
}
