use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use tracing::{debug, warn};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::{build_snapshot, IndexSnapshot};
use crate::model::{EntryType, QueryOptions, QueryResult, SlotId};
use crate::monitor::{slot_for_path, FsEvent, Monitor};
use crate::persist::{self, SharedIndex};
use crate::store::{SharedStore, Store};
use crate::walker::{scan_root, scan_subtree, WalkOptions};

pub struct Engine {
    pub store: SharedStore,
    pub index: SharedIndex,
    pub config: Config,
    monitor: Option<Arc<Monitor>>,
    dirty: AtomicBool,
    need_full: AtomicBool,
    changed: RwLock<HashSet<SlotId>>,
    rescan_paths: RwLock<Vec<PathBuf>>,
    last_change: RwLock<std::time::Instant>,
    last_full_scan: RwLock<std::time::Instant>,
    last_save: RwLock<std::time::Instant>,
    generation: AtomicU64,
    full_rebuilds: AtomicU64,
    incremental_updates: AtomicU64,
}

impl Engine {
    pub fn open(mut config: Config, load_snapshot: bool) -> Result<Self> {
        let index: SharedIndex = Arc::new(arc_swap::ArcSwap::from(Arc::new(IndexSnapshot::empty())));
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
        let snapshot = build_snapshot(&store, initial_gen);
        index.store(Arc::new(snapshot));
        let now = std::time::Instant::now();
        Ok(Self {
            store: RwLock::new(store),
            index,
            config,
            monitor: None,
            dirty: AtomicBool::new(false),
            need_full: AtomicBool::new(false),
            changed: RwLock::new(HashSet::new()),
            rescan_paths: RwLock::new(Vec::new()),
            last_change: RwLock::new(now),
            last_full_scan: RwLock::new(now),
            last_save: RwLock::new(now),
            generation: AtomicU64::new(initial_gen),
            full_rebuilds: AtomicU64::new(0),
            incremental_updates: AtomicU64::new(0),
        })
    }

    pub fn start_monitor(&mut self) -> Result<()> {
        if !self.config.watch_enabled {
            return Ok(());
        }
        let m = Arc::new(Monitor::new()?);
        {
            let s = self.store.read();
            let dirs: Vec<PathBuf> = (0..s.len())
                .filter(|&i| s.is_live(i as u32) && s.meta(i as u32).unwrap().kind == EntryType::Dir)
                .filter_map(|i| slot_path(&s, i as u32).map(PathBuf::from))
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

    pub fn scan_configured_roots(&mut self) -> Result<()> {
        let roots = self.config.roots.clone();
        for root in roots {
            self.add_root(&root)?;
        }
        self.rebuild();
        let _ = self.save();
        Ok(())
    }

    pub fn add_root(&mut self, path: &Path) -> Result<()> {
        let abs = std::fs::canonicalize(path)
            .map_err(|e| Error::Other(anyhow::anyhow!("cannot access {}: {e}", path.display())))?;
        if self.store.read().roots.iter().any(|r| r.path == abs) {
            return Ok(());
        }
        let opts = self.walk_options();
        let monitor = self.monitor.clone();
        let failed = Arc::new(RwLock::new(Vec::new()));
        let failed_t = failed.clone();
        let on_dir = move |p: &Path| {
            if let Some(m) = &monitor {
                if let Err(_) = m.add_watch(p) {
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
        Ok(())
    }

    pub fn remove_root(&mut self, path: &Path) -> Result<()> {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let slot = {
            let s = self.store.read();
            s.roots.iter().find(|r| r.path == abs).map(|r| r.slot)
        };
        if let Some(slot) = slot {
            let removed = self.remove_subtree(slot);
            self.mark_changed_many(&removed);
            self.store.write().roots.retain(|r| r.slot != slot);
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
        self.changed.write().clear();
        self.rescan_paths.write().clear();
        for root in &roots {
            self.add_root(root)?;
        }
        self.rebuild();
        self.save()?;
        *self.last_full_scan.write() = std::time::Instant::now();
        Ok(())
    }

    /// Full rebuild: copy all names and re-sort every index array.
    pub fn rebuild(&self) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let snap = build_snapshot(&self.store.read(), generation);
        persist::publish(&self.index, snap);
        self.changed.write().clear();
        self.dirty.store(false, Ordering::SeqCst);
        self.need_full.store(false, Ordering::SeqCst);
        *self.last_change.write() = std::time::Instant::now();
        self.full_rebuilds.fetch_add(1, Ordering::SeqCst);
        debug!("full index rebuild -> generation {generation}");
    }

    /// Incremental rebuild: only touch slots reported as changed.
    pub fn incremental_rebuild(&self) {
        let changed: Vec<SlotId> = self.changed.read().iter().copied().collect();
        if changed.is_empty() {
            self.dirty.store(false, Ordering::SeqCst);
            return;
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let old = self.index.load_full();
        let snap = old.apply_changes(&self.store.read(), &changed);
        let snap = IndexSnapshot {
            generation,
            ..snap
        };
        persist::publish(&self.index, snap);
        self.changed.write().clear();
        self.dirty.store(false, Ordering::SeqCst);
        *self.last_change.write() = std::time::Instant::now();
        self.incremental_updates.fetch_add(1, Ordering::SeqCst);
        debug!("incremental index update ({} slots) -> generation {generation}", changed.len());
    }

    pub fn save(&self) -> Result<()> {
        let generation = self.generation.load(Ordering::SeqCst);
        persist::save(&self.store.read(), &self.config.snapshot_path, generation)?;
        *self.last_save.write() = std::time::Instant::now();
        Ok(())
    }

    fn maybe_save(&self) {
        if self.config.snapshot_interval_secs == 0 {
            return;
        }
        if self.last_save.read().elapsed().as_secs() >= self.config.snapshot_interval_secs {
            let _ = self.save();
        }
    }

    pub fn query(&self, q: &str, opts: QueryOptions) -> Result<QueryResult> {
        let snap = self.index.load_full();
        crate::query::run(&snap, q, opts)
    }

    pub fn snapshot(&self) -> Arc<IndexSnapshot> {
        self.index.load_full()
    }

    pub fn stats(&self) -> (u64, u64, u64) {
        let s = self.snapshot();
        (s.file_count, s.dir_count, s.generation)
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
            index_hidden: self.config.index_hidden,
            follow_symlinks: self.config.follow_symlinks,
            threads: self.config.threads,
        }
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        *self.last_change.write() = std::time::Instant::now();
    }

    fn mark_changed(&self, slot: SlotId) {
        self.changed.write().insert(slot);
        self.mark_dirty();
    }

    fn mark_changed_many(&self, slots: &[SlotId]) {
        let mut c = self.changed.write();
        for &s in slots {
            c.insert(s);
        }
        drop(c);
        self.mark_dirty();
    }

    /// Called by the daemon every ~25ms.
    pub fn tick(&mut self) -> Result<()> {
        let Some(monitor) = self.monitor.clone() else {
            return Ok(());
        };
        let mut overflow = false;
        while let Some(ev) = monitor.try_recv() {
            match ev {
                FsEvent::Overflow => overflow = true,
                FsEvent::Create(path) => self.handle_create(&path, &monitor, false),
                FsEvent::CreateDir(path) => self.handle_create(&path, &monitor, true),
                FsEvent::Modify(path) => self.handle_modify(&path),
                FsEvent::Delete(path) => self.handle_delete(&path),
                FsEvent::Rename { from, to } => self.handle_rename(&from, &to, &monitor),
                FsEvent::Rescan(path) => {
                    if path.exists() {
                        let _ = monitor.add_watch_recursive(&path);
                    }
                    self.rescan_paths.write().push(path);
                }
                FsEvent::WatchFailed(path) => self.rescan_paths.write().push(path),
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
            Ok(Some(slot)) => {
                self.mark_changed(slot);
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
                    scan_subtree(path, slot, &self.store, &opts, Some(&on_dir));
                    for p in failed.read().iter() {
                        self.rescan_paths.write().push(p.clone());
                    }
                    let added = self.descendants(slot);
                    self.mark_changed_many(&added);
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
            let meta = crate::model::EntryMeta {
                ino: crate::walker::ino_of(&md),
                dev: crate::walker::dev_of(&md),
                kind,
                size: md.len(),
                mtime: crate::walker::mtime_of(&md),
                created: crate::walker::times_of(path, &md).0,
                accessed: crate::walker::times_of(path, &md).1,
                flags: self
                    .store
                    .read()
                    .meta(slot)
                    .map(|m| m.flags)
                    .unwrap_or(0),
            };
            self.store.write().set_meta(slot, meta);
            self.mark_changed(slot);
        }
    }

    fn handle_delete(&self, path: &Path) {
        let slot = match slot_for_path(&self.store.read(), path) {
            Some(s) => s,
            None => return,
        };
        let removed = self.remove_subtree(slot);
        self.mark_changed_many(&removed);
    }

    fn handle_rename(&self, from: &Path, to: &Path, monitor: &Monitor) {
        let from_slot = slot_for_path(&self.store.read(), from);
        let removed = match from_slot {
            Some(s) => self.remove_subtree(s),
            None => Vec::new(),
        };
        self.mark_changed_many(&removed);
        self.handle_create(to, monitor, false);
    }

    /// Re-scan a single directory subtree and reconcile it with the store.
    fn reconcile_path(&self, path: &Path, monitor: &Monitor) {
        if !path.is_dir() {
            return;
        }
        let existing = slot_for_path(&self.store.read(), path);
        if let Some(slot) = existing {
            let removed = self.remove_subtree(slot);
            self.mark_changed_many(&removed);
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
                let mut added = self.descendants(new_slot);
                added.push(new_slot);
                self.mark_changed_many(&added);
            }
        }
    }

    fn descendants(&self, root: SlotId) -> Vec<SlotId> {
        let s = self.store.read();
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(slot) = stack.pop() {
            if let Some(children) = s.children.get(&slot) {
                for &c in children {
                    stack.push(c);
                    out.push(c);
                }
            }
        }
        out
    }

    fn remove_subtree(&self, root_slot: SlotId) -> Vec<SlotId> {
        let mut to_remove = vec![root_slot];
        let mut i = 0;
        {
            let s = self.store.read();
            while i < to_remove.len() {
                if let Some(children) = s.children.get(&to_remove[i]) {
                    for &c in children {
                        to_remove.push(c);
                    }
                }
                i += 1;
            }
        }
        let mut s = self.store.write();
        for slot in &to_remove {
            s.remove_entry(*slot);
        }
        to_remove
    }
}

fn slot_path(store: &Store, slot: u32) -> Option<String> {
    let mut cur = slot;
    let mut parts: Vec<Vec<u8>> = Vec::new();
    loop {
        let _e = store.meta(cur)?;
        if store.parent(cur)? == crate::model::NULL_SLOT {
            break;
        }
        parts.push(store.name(cur).to_vec());
        cur = store.parent(cur)?;
    }
    let root = store.roots.iter().find(|r| r.slot == cur)?;
    let mut out = root.path.as_os_str().as_encoded_bytes().to_vec();
    for p in parts.iter().rev() {
        out.push(b'/');
        out.extend_from_slice(p);
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}
