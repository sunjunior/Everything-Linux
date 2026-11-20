use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::Mutex;

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
    engine: Arc<Mutex<Engine>>,
    running: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

impl LocalEngine {
    pub fn open(config: Config, load_snapshot: bool) -> Result<Self> {
        let mut engine = Engine::open(config, load_snapshot)?;
        engine.scan_configured_roots()?;
        engine.start_monitor()?;
        let engine = Arc::new(Mutex::new(engine));
        let running = Arc::new(AtomicBool::new(true));
        let ticker = {
            let engine = engine.clone();
            let running = running.clone();
            std::thread::Builder::new()
                .name("local-engine-tick".into())
                .spawn(move || {
                    while running.load(Ordering::SeqCst) {
                        if let Some(mut eng) = engine.try_lock() {
                            let _ = eng.tick();
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
                .ok()
        };
        Ok(Self {
            engine,
            running,
            ticker,
        })
    }

    pub fn search_slots(&self, q: &str, opts: QueryOptions) -> Result<QueryResult> {
        self.engine.lock().search_slots(q, opts)
    }

    pub fn path(&self, slot: u32) -> Option<PathBuf> {
        self.engine.lock().slot_path(slot)
    }

    pub fn search_paths(&self, q: &str, opts: QueryOptions) -> Result<SearchHits> {
        let engine = self.engine.lock();
        let (paths, result) = engine.search_paths(q, opts)?;
        Ok(SearchHits { paths, result })
    }

    pub fn status(&self) -> (u64, u64, u64) {
        self.engine.lock().stats()
    }

    pub fn watch_count(&self) -> usize {
        self.engine.lock().watch_count()
    }

    pub fn update_counters(&self) -> (u64, u64) {
        self.engine.lock().update_counters()
    }

    pub fn save(&self) -> Result<()> {
        self.engine.lock().save()
    }

    pub fn reindex(&self) -> Result<()> {
        self.engine.lock().reindex()
    }

    pub fn add_root(&self, path: &Path) -> Result<()> {
        let mut engine = self.engine.lock();
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
        let mut engine = self.engine.lock();
        engine.remove_root(path)?;
        engine.config.mark_manual_roots();
        engine.rebuild();
        engine.save()?;
        let _ = engine.config.save();
        Ok(())
    }

    pub fn set_auto_root(&self, frontend: &Path) -> Result<bool> {
        self.engine.lock().set_auto_root(frontend)
    }

    pub fn save_config(&self) -> Result<()> {
        self.engine.lock().config.save()
    }

    pub fn shutdown(&mut self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        let engine = self.engine.lock();
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
