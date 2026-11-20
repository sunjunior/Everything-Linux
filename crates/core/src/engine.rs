use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::RwLock;
use tracing::{debug, warn};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{EntryMeta, EntryType, QueryOptions, QueryResult, SlotId};
use crate::monitor::{FsEvent, Monitor, slot_for_path};
use crate::persist;
use crate::store::{SharedStore, Store};
use crate::walker::{WalkOptions, scan_root, scan_subtree};

pub struct Engine {
    pub store: SharedStore,
    pub config: Config,
    monitor: Option<Arc<Monitor>>,
    dirty: AtomicBool,
    need_full: AtomicBool,
    rescan_paths: RwLock<Vec<PathBuf>>,
    last_change: RwLock<std::time::Instant>,
    last_full_scan: RwLock<std::time::Instant>,
    last_save: RwLock<std::time::Instant>,
    generation: AtomicU64,
    full_rebuilds: AtomicU64,
    incremental_updates: AtomicU64,
    saved_generation: RwLock<u64>,
    internal: Vec<PathBuf>,
}

fn internal_paths(config: &Config) -> Vec<PathBuf> {
    let mut out = vec![
        config.snapshot_path.clone(),
        config.snapshot_path.with_extension("tmp"),
        config.socket_path.clone(),
    ];
    out.sort();
    out.dedup();
    out
}

impl Engine {
    pub fn open(mut config: Config, load_snapshot: bool) -> Result<Self> {
        let (store, loaded_gen) = if load_snapshot {
            match persist::load(&config.snapshot_path) {
                Ok(Some(v)) => v,
                Ok(None) => (Store::new(), 0),
                Err(e) => {
                    warn!("load snapshot failed: {e}; rebuilding from scratch");
                    (Store::new(), 0)
                }
            }
        } else {
            (Store::new(), 0)
        };
        if config.roots.is_empty() {
            config.roots = store.roots.iter().map(|r| r.path.clone()).collect();
        }
        if config.roots.is_empty() {
            config.roots = config.effective_roots();
        }
        let initial_gen = loaded_gen.max(1);
        let now = std::time::Instant::now();
        let internal = internal_paths(&config);
        Ok(Self {
            store: RwLock::new(store),
            config,
            monitor: None,
            dirty: AtomicBool::new(false),
            need_full: AtomicBool::new(false),
            rescan_paths: RwLock::new(Vec::new()),
            last_change: RwLock::new(now),
            last_full_scan: RwLock::new(now),
            last_save: RwLock::new(now),
            generation: AtomicU64::new(initial_gen),
            full_rebuilds: AtomicU64::new(0),
            incremental_updates: AtomicU64::new(0),
            saved_generation: RwLock::new(loaded_gen),
            internal,
        })
    }

    fn is_internal(&self, path: &Path) -> bool {
        self.internal.iter().any(|p| p == path)
    }

    pub fn start_monitor(&mut self) -> Result<()> {
        if !self.config.watch_enabled {
            return Ok(());
        }
        let m = Arc::new(Monitor::new()?);
        {
            let s = self.store.read();
            let dirs: Vec<PathBuf> = (0..s.len())
                .filter(|&i| s.is_live(i as u32) && s.kind(i as u32) == EntryType::Dir)
                .filter_map(|i| slot_path(&s, i as u32))
                .filter(|p| !self.is_internal(p))
                .collect();
            for d in dirs {
                if let Err(e) = m.add_watch(&d) {
                    warn!("watch {} failed: {e}", d.display());
                    self.rescan_paths.write().push(d);
                }
            }
        }
        self.monitor = Some(m);
        Ok(())
    }

    pub fn monitor(&self) -> Option<&Arc<Monitor>> {
        self.monitor.as_ref()
    }

    pub fn take_monitor(&mut self) -> Option<Arc<Monitor>> {
        self.monitor.take()
    }

    pub fn scan_configured_roots(&mut self) -> Result<()> {
        let roots = self.config.roots.clone();
        let mut changed = false;
        for root in roots {
            changed |= self.add_root(&root)?;
        }
        if changed {
            self.rebuild();
            let _ = self.save();
        }
        Ok(())
    }

    pub fn add_root(&mut self, path: &Path) -> Result<bool> {
        let abs = std::fs::canonicalize(path)
            .map_err(|e| Error::Other(anyhow::anyhow!("cannot access {}: {e}", path.display())))?;
        if self.store.read().roots.iter().any(|r| r.path == abs) {
            return Ok(false);
        }
        let opts = self.walk_options();
        let monitor = self.monitor.clone();
        let failed = Arc::new(RwLock::new(Vec::new()));
        let failed_t = failed.clone();
        let on_dir = move |p: &Path| {
            if let Some(m) = &monitor {
                if m.add_watch(p).is_err() {
                    failed_t.write().push(p.to_path_buf());
                }
            }
        };
        scan_root(&abs, &self.store, &opts, Some(&on_dir))?;
        for p in failed.read().iter() {
            self.rescan_paths.write().push(p.clone());
        }
        if !self.config.roots.contains(&abs) {
            self.config.roots.push(abs);
        }
        self.need_full.store(true, Ordering::SeqCst);
        self.mark_dirty();
        Ok(true)
    }

    pub fn remove_root(&mut self, path: &Path) -> Result<()> {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let slot = {
            let s = self.store.read();
            s.roots.iter().find(|r| r.path == abs).map(|r| r.slot)
        };
        if let Some(slot) = slot {
            self.store.write().remove_subtree(slot);
            self.mark_dirty();
            self.config.roots.retain(|r| *r != abs);
        }
        Ok(())
    }

    pub fn reindex(&mut self) -> Result<()> {
        let roots = self.config.roots.clone();
        {
            let mut s = self.store.write();
            *s = Store::new();
        }
        self.rescan_paths.write().clear();
        for root in &roots {
            self.add_root(root)?;
        }
        self.rebuild();
        self.save()?;
        *self.last_full_scan.write() = std::time::Instant::now();
        Ok(())
    }

    pub fn rebuild(&self) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.dirty.store(false, Ordering::SeqCst);
        self.need_full.store(false, Ordering::SeqCst);
        *self.last_change.write() = std::time::Instant::now();
        self.full_rebuilds.fetch_add(1, Ordering::SeqCst);
        debug!("full index rebuild -> generation {generation}");
    }

    /// Publish the mutations accumulated in a batch.
    pub fn incremental_rebuild(&self) {
        if !self.dirty.load(Ordering::SeqCst) {
            return;
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.dirty.store(false, Ordering::SeqCst);
        *self.last_change.write() = std::time::Instant::now();
        self.incremental_updates.fetch_add(1, Ordering::SeqCst);
        debug!("incremental index update -> generation {generation}");
    }

    pub fn set_auto_root(&mut self, frontend: &Path) -> Result<bool> {
        if !self.config.auto_root {
            return Ok(false);
        }
        let dir = frontend
            .parent()
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        if self.config.exe_path.as_deref() == Some(frontend)
            && self.config.roots == vec![dir.clone()]
        {
            return Ok(false);
        }
        self.config.exe_path = Some(frontend.to_path_buf());
        self.config.roots = vec![dir];
        self.reindex()?;
        Ok(true)
    }

    pub fn save(&self) -> Result<()> {
        let generation = self.generation.load(Ordering::SeqCst);
        persist::save(&self.store.read(), &self.config.snapshot_path, generation)?;
        *self.last_save.write() = std::time::Instant::now();
        *self.saved_generation.write() = generation;
        persist::trim_allocator();
        Ok(())
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.generation.load(Ordering::SeqCst) != *self.saved_generation.read()
    }

    fn maybe_save(&self) {
        if self.config.snapshot_interval_secs == 0 {
            return;
        }
        let generation = self.generation.load(Ordering::SeqCst);
        if generation == *self.saved_generation.read() {
            return;
        }
        if self.last_save.read().elapsed().as_secs() >= self.config.snapshot_interval_secs {
            let _ = self.save();
        }
    }

    pub fn query(&self, q: &str, opts: QueryOptions) -> Result<QueryResult> {
        let generation = self.generation.load(Ordering::SeqCst);
        let store = self.store.read();
        crate::query::run(&store, generation, q, opts)
    }

    pub fn path(&self, slot: SlotId, buf: &mut Vec<u8>) {
        self.store.read().path(slot, buf);
    }

    /// Run a query and return slot ids without rendering paths. Frontends that
    /// render only visible rows can resolve paths lazily via `slot_path`.
    pub fn search_slots(&self, q: &str, opts: QueryOptions) -> Result<QueryResult> {
        let generation = self.generation.load(Ordering::SeqCst);
        let store = self.store.read();
        crate::query::run(&store, generation, q, opts)
    }

    pub fn slot_path(&self, slot: SlotId) -> Option<PathBuf> {
        self.store.read().path_of(slot)
    }

    /// Run a query and render matching paths while holding the store lock once.
    pub fn search_paths(&self, q: &str, opts: QueryOptions) -> Result<(Vec<PathBuf>, QueryResult)> {
        use std::os::unix::ffi::OsStringExt;
        let generation = self.generation.load(Ordering::SeqCst);
        let store = self.store.read();
        let result = crate::query::run(&store, generation, q, opts)?;
        let mut paths = Vec::with_capacity(result.slots.len());
        let mut buf = Vec::new();
        for &slot in &result.slots {
            store.path(slot, &mut buf);
            paths.push(PathBuf::from(std::ffi::OsString::from_vec(std::mem::take(
                &mut buf,
            ))));
        }
        Ok((paths, result))
    }

    pub fn stats(&self) -> (u64, u64, u64) {
        let s = self.store.read();
        (
            s.file_count(),
            s.dir_count(),
            self.generation.load(Ordering::SeqCst),
        )
    }

    pub fn watch_count(&self) -> usize {
        self.monitor.as_ref().map(|m| m.watch_count()).unwrap_or(0)
    }

    pub fn update_counters(&self) -> (u64, u64) {
        (
            self.full_rebuilds.load(Ordering::SeqCst),
            self.incremental_updates.load(Ordering::SeqCst),
        )
    }

    fn walk_options(&self) -> WalkOptions<'_> {
        WalkOptions {
            exclude: &self.config.exclude,
            skip: &self.internal,
            index_hidden: self.config.index_hidden,
            follow_symlinks: self.config.follow_symlinks,
            threads: self.config.threads,
        }
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        *self.last_change.write() = std::time::Instant::now();
    }

    /// Called by the daemon every ~10ms.
    pub fn tick(&mut self) -> Result<()> {
        let Some(monitor) = self.monitor.clone() else {
            return Ok(());
        };
        let mut overflow = false;
        while let Some(ev) = monitor.try_recv() {
            match ev {
                FsEvent::Overflow => overflow = true,
                FsEvent::Create(path) => {
                    if !self.is_internal(&path) {
                        self.handle_create(&path, &monitor, false);
                    }
                }
                FsEvent::CreateDir(path) => {
                    if !self.is_internal(&path) {
                        self.handle_create(&path, &monitor, true);
                    }
                }
                FsEvent::Modify(path) => {
                    if !self.is_internal(&path) {
                        self.handle_modify(&path);
                    }
                }
                FsEvent::Delete(path) => {
                    if !self.is_internal(&path) {
                        self.handle_delete(&path);
                    }
                }
                FsEvent::Rename { from, to } => {
                    if !self.is_internal(&from) && !self.is_internal(&to) {
                        self.handle_rename(&from, &to, &monitor);
                    }
                }
                FsEvent::Rescan(path) => {
                    if !self.is_internal(&path) {
                        if path.exists() {
                            let _ = monitor.add_watch_recursive(&path);
                        }
                        self.rescan_paths.write().push(path);
                    }
                }
                FsEvent::WatchFailed(path) => {
                    if !self.is_internal(&path) {
                        self.rescan_paths.write().push(path);
                    }
                }
            }
        }

        let rescan = std::mem::take(&mut *self.rescan_paths.write());
        if !rescan.is_empty() {
            for path in &rescan {
                self.reconcile_path(path, &monitor);
            }
            self.need_full.store(true, Ordering::SeqCst);
            self.mark_dirty();
        }

        let periodic = self.config.rescan_interval_secs > 0
            && self.last_full_scan.read().elapsed().as_secs() >= self.config.rescan_interval_secs;
        if periodic {
            debug!("periodic full reconciliation");
        }

        if overflow || periodic {
            self.reindex()?;
        } else if self.dirty.load(Ordering::SeqCst)
            && self.last_change.read().elapsed().as_millis() >= self.config.debounce_ms as u128
        {
            if self.need_full.load(Ordering::SeqCst) {
                self.rebuild();
            } else {
                self.incremental_rebuild();
            }
            self.maybe_save();
        }
        Ok(())
    }

    fn handle_create(&self, path: &Path, monitor: &Monitor, is_dir: bool) {
        let parent = path.parent().unwrap_or(Path::new("/"));
        let parent_slot = match slot_for_path(&self.store.read(), parent) {
            Some(s) => s,
            None => {
                debug!("create: no parent slot for {}", parent.display());
                return;
            }
        };
        let is_dir = is_dir
            || std::fs::symlink_metadata(path)
                .map(|m| m.is_dir())
                .unwrap_or(false);
        match crate::walker::add_single(path, parent_slot, &self.store) {
            Ok(Some(_slot)) => {
                self.mark_dirty();
                if is_dir {
                    let _ = monitor.add_watch_recursive(path);
                    let opts = self.walk_options();
                    let failed = Arc::new(RwLock::new(Vec::new()));
                    let failed_t = failed.clone();
                    let on_dir = move |p: &Path| {
                        if monitor.add_watch(p).is_err() {
                            failed_t.write().push(p.to_path_buf());
                        }
                    };
                    scan_subtree(path, _slot, &self.store, &opts, Some(&on_dir));
                    for p in failed.read().iter() {
                        self.rescan_paths.write().push(p.clone());
                    }
                }
            }
            Ok(None) => {}
            Err(e) => debug!("create {} failed: {e}", path.display()),
        }
    }

    fn handle_modify(&self, path: &Path) {
        let slot = match slot_for_path(&self.store.read(), path) {
            Some(s) => s,
            None => return,
        };
        if let Ok(md) = std::fs::symlink_metadata(path) {
            let kind = if md.is_dir() {
                EntryType::Dir
            } else if md.file_type().is_symlink() {
                EntryType::Symlink
            } else if md.is_file() {
                EntryType::File
            } else {
                EntryType::Other
            };
            let (created, accessed, mtime) = crate::walker::times_of(path, &md);
            let flags = self.store.read().entry(slot).map(|e| e.flags).unwrap_or(0);
            let meta = EntryMeta {
                kind,
                size: md.len(),
                mtime,
                created,
                accessed,
                flags,
            };
            self.store.write().set_meta(slot, meta);
            self.mark_dirty();
        }
    }

    fn handle_delete(&self, path: &Path) {
        let slot = match slot_for_path(&self.store.read(), path) {
            Some(s) => s,
            None => return,
        };
        self.store.write().remove_subtree(slot);
        self.mark_dirty();
    }

    fn handle_rename(&self, from: &Path, to: &Path, monitor: &Monitor) {
        let from_slot = slot_for_path(&self.store.read(), from);
        if let Some(s) = from_slot {
            self.store.write().remove_subtree(s);
            self.mark_dirty();
        }
        self.handle_create(to, monitor, false);
    }

    /// Re-scan a single directory subtree and reconcile it with the store.
    fn reconcile_path(&self, path: &Path, monitor: &Monitor) {
        if !path.is_dir() {
            return;
        }
        let existing = slot_for_path(&self.store.read(), path);
        if let Some(slot) = existing {
            self.store.write().remove_subtree(slot);
            self.mark_dirty();
        }
        let parent = path.parent().unwrap_or(Path::new("/"));
        let parent_slot_opt = slot_for_path(&self.store.read(), parent);
        if let Some(parent_slot) = parent_slot_opt {
            if let Ok(Some(new_slot)) = crate::walker::add_single(path, parent_slot, &self.store) {
                let opts = self.walk_options();
                let on_dir = |p: &Path| {
                    let _ = monitor.add_watch(p);
                };
                let _ = monitor.add_watch_recursive(path);
                scan_subtree(path, new_slot, &self.store, &opts, Some(&on_dir));
                self.mark_dirty();
            }
        }
    }
}

fn slot_path(store: &Store, slot: u32) -> Option<PathBuf> {
    store.path_of(slot)
}
