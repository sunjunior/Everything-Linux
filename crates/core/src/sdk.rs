use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};

use crate::config::Config;
use crate::engine::Engine;
use crate::error::Result;
use crate::model::{QueryOptions, QueryResult};

pub struct SearchHits {
    pub paths: Vec<PathBuf>,
    pub result: QueryResult,
}

/// In-process frontend/backend coupling: owns the index, inotify monitor and
/// tick thread, and exposes a direct Rust API with no IPC or serialization.
pub struct LocalEngine {
    engine: Arc<RwLock<Arc<Mutex<Engine>>>>,
    running: Arc<AtomicBool>,
    rebuilding: Arc<AtomicBool>,
    root_generation: AtomicU64,
    ticker: Option<JoinHandle<()>>,
}

impl LocalEngine {
    pub fn open(config: Config, load_snapshot: bool) -> Result<Self> {
        let mut engine = Engine::open(config, load_snapshot)?;
        engine.scan_configured_roots()?;
        engine.start_monitor()?;
        let engine = Arc::new(RwLock::new(Arc::new(Mutex::new(engine))));
        let running = Arc::new(AtomicBool::new(true));
        let rebuilding = Arc::new(AtomicBool::new(false));
        let ticker = {
            let engine = engine.clone();
            let running = running.clone();
            let rebuilding = rebuilding.clone();
            std::thread::Builder::new()
                .name("local-engine-tick".into())
                .spawn(move || {
                    while running.load(Ordering::SeqCst) {
                        if !rebuilding.load(Ordering::SeqCst) {
                            let current = engine.read().clone();
                            if let Some(mut eng) = current.try_lock() {
                                let _ = eng.tick();
                            }
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
                .ok()
        };
        Ok(Self {
            engine,
            running,
            rebuilding,
            root_generation: AtomicU64::new(1),
            ticker,
        })
    }

    fn current(&self) -> Arc<Mutex<Engine>> {
        self.engine.read().clone()
    }

    pub fn generation(&self) -> u64 {
        self.current().lock().stats().2
    }

    /// Changes only when the whole engine is replaced (root switch), not on
    /// ordinary inotify increments. GUI rows use this to detect stale slots
    /// without invalidating the list on every filesystem event.
    pub fn root_generation(&self) -> u64 {
        self.root_generation.load(Ordering::SeqCst)
    }

    pub fn search_slots(&self, q: &str, opts: QueryOptions) -> Result<QueryResult> {
        self.current().lock().search_slots(q, opts)
    }

    pub fn path(&self, slot: u32) -> Option<PathBuf> {
        self.current().lock().slot_path(slot)
    }

    pub fn search_paths(&self, q: &str, opts: QueryOptions) -> Result<SearchHits> {
        let engine = self.current();
        let engine = engine.lock();
        let (paths, result) = engine.search_paths(q, opts)?;
        Ok(SearchHits { paths, result })
    }

    pub fn status(&self) -> (u64, u64, u64) {
        self.current().lock().stats()
    }

    pub fn watch_count(&self) -> usize {
        self.current().lock().watch_count()
    }

    pub fn watch_status(&self) -> Option<(usize, usize, bool)> {
        self.current().lock().watch_status()
    }

    pub fn update_counters(&self) -> (u64, u64) {
        self.current().lock().update_counters()
    }

    pub fn save(&self) -> Result<()> {
        self.current().lock().save()
    }

    pub fn reindex(&self) -> Result<()> {
        self.current().lock().reindex()
    }

    pub fn add_root(&self, path: &Path) -> Result<()> {
        let current = self.current();
        let mut engine = current.lock();
        let added = engine.add_root(path)?;
        engine.config.mark_manual_roots();
        if added {
            engine.rebuild();
            engine.save()?;
        }
        let _ = engine.config.save();
        Ok(())
    }

    pub fn remove_root(&self, path: &Path) -> Result<()> {
        let current = self.current();
        let mut engine = current.lock();
        engine.remove_root(path)?;
        engine.config.mark_manual_roots();
        engine.rebuild();
        engine.save()?;
        let _ = engine.config.save();
        Ok(())
    }

    /// Rebuild a new root in the background while the old index keeps serving
    /// queries; swap the engine atomically when the new one is ready.
    pub fn set_auto_root(&self, frontend: &Path) -> Result<bool> {
        let current = self.current();
        let mut cfg = current.lock().config.clone();
        if !cfg.auto_root {
            return Ok(false);
        }
        let dir = frontend
            .parent()
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        if cfg.exe_path.as_deref() == Some(frontend) && cfg.roots == vec![dir.clone()] {
            return Ok(false);
        }
        cfg.exe_path = Some(frontend.to_path_buf());
        cfg.roots = vec![dir];

        self.rebuilding.store(true, Ordering::SeqCst);
        drop(current.lock().take_monitor());

        let result = (|| -> Result<bool> {
            let mut new_engine = Engine::open(cfg.clone(), false)?;
            new_engine.scan_configured_roots()?;
            new_engine.start_monitor()?;
            let new_engine = Arc::new(Mutex::new(new_engine));
            *self.engine.write() = new_engine;
            self.root_generation.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })();

        if result.is_err() {
            let _ = current.lock().start_monitor();
        }
        self.rebuilding.store(false, Ordering::SeqCst);
        result
    }

    pub fn save_config(&self) -> Result<()> {
        self.current().lock().config.save()
    }

    pub fn shutdown(&mut self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        let engine = self.current();
        let engine = engine.lock();
        if engine.has_unsaved_changes() {
            let _ = engine.save();
        }
    }
}

impl Drop for LocalEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
