use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;

use crate::model::{EntryType, RootInfo, SlotId};
use crate::store::Store;

#[derive(Clone, Copy, Debug)]
pub struct SnapEntry {
    pub name_off: u32,
    pub name_len: u16,
    pub fold_off: u32,
    pub parent: SlotId,
    pub kind: u8,
    pub root: u16,
    pub size: u64,
    pub mtime: i64,
    pub created: i64,
    pub accessed: i64,
}

/// Immutable index published to query threads. The name pools and base entries
/// are `Arc`-shared, so a small update only clones a few pointers plus the
/// changed entries instead of copying the whole index.
#[derive(Clone)]
pub struct IndexSnapshot {
    pub names: Arc<Vec<u8>>,
    pub folded: Arc<Vec<u8>>,
    pub entries: Arc<Vec<SnapEntry>>,
    pub entry_overrides: HashMap<SlotId, SnapEntry>,
    pub name_appends: Vec<u8>,
    pub folded_appends: Vec<u8>,
    pub by_name: Vec<u32>,
    pub by_name_cs: Vec<u32>,
    pub by_parent: HashMap<SlotId, Vec<u32>>,
    pub roots: Vec<RootInfo>,
    pub file_count: u64,
    pub dir_count: u64,
    pub generation: u64,
}

impl IndexSnapshot {
    pub fn empty() -> Self {
        Self {
            names: Arc::new(Vec::new()),
            folded: Arc::new(Vec::new()),
            entries: Arc::new(Vec::new()),
            entry_overrides: HashMap::new(),
            name_appends: Vec::new(),
            folded_appends: Vec::new(),
            by_name: Vec::new(),
            by_name_cs: Vec::new(),
            by_parent: HashMap::new(),
            roots: Vec::new(),
            file_count: 0,
            dir_count: 0,
            generation: 0,
        }
    }

    pub fn entry(&self, slot: SlotId) -> &SnapEntry {
        self.entry_overrides
            .get(&slot)
            .unwrap_or_else(|| &self.entries[slot as usize])
    }

    pub fn name(&self, slot: SlotId) -> &[u8] {
        let e = self.entry(slot);
        let off = e.name_off as usize;
        let base = self.names.len();
        let pool = if off < base {
            &self.names[off..off + e.name_len as usize]
        } else {
            let start = off - base;
            &self.name_appends[start..start + e.name_len as usize]
        };
        pool
    }

    pub fn folded(&self, slot: SlotId) -> &[u8] {
        let e = self.entry(slot);
        let off = e.fold_off as usize;
        let base = self.folded.len();
        if off < base {
            &self.folded[off..off + e.name_len as usize]
        } else {
            let start = off - base;
            &self.folded_appends[start..start + e.name_len as usize]
        }
    }

    pub fn kind(&self, slot: SlotId) -> EntryType {
        EntryType::from_u8(self.entry(slot).kind)
    }

    pub fn path(&self, slot: SlotId, buf: &mut Vec<u8>) {
        buf.clear();
        let mut cur = slot;
        let mut parts: Vec<&[u8]> = Vec::new();
        loop {
            let e = self.entry(cur);
            if e.parent == crate::model::NULL_SLOT {
                break;
            }
            parts.push(self.name(cur));
            cur = e.parent;
        }
        if let Some(root) = self.roots.iter().find(|r| r.slot == cur) {
            buf.extend_from_slice(root.path.as_os_str().as_encoded_bytes());
        }
        for part in parts.iter().rev() {
            if !buf.is_empty() && buf.last() != Some(&b'/') {
                buf.push(b'/');
            }
            buf.extend_from_slice(part);
        }
    }
}

pub fn fold_name(name: &[u8]) -> Vec<u8> {
    if let Ok(s) = std::str::from_utf8(name) {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        }
        out.into_bytes()
    } else {
        name.iter().map(|b| b.to_ascii_lowercase()).collect()
    }
}

fn sort_key_by_name(snap: &IndexSnapshot, slot: SlotId) -> &[u8] {
    let e = snap.entry(slot);
    let off = e.name_off as usize;
    let base = snap.names.len();
    if off < base {
        &snap.names[off..off + e.name_len as usize]
    } else {
        let s = off - base;
        &snap.name_appends[s..s + e.name_len as usize]
    }
}

fn sort_key_folded(snap: &IndexSnapshot, slot: SlotId) -> &[u8] {
    let e = snap.entry(slot);
    let off = e.fold_off as usize;
    let base = snap.folded.len();
    if off < base {
        &snap.folded[off..off + e.name_len as usize]
    } else {
        let s = off - base;
        &snap.folded_appends[s..s + e.name_len as usize]
    }
}

fn sorted_pos<F: Fn(SlotId) -> std::cmp::Ordering>(arr: &[u32], f: F) -> Result<usize, usize> {
    arr.binary_search_by(|&slot| f(slot))
}

impl IndexSnapshot {
    /// Apply a small set of changed slots without copying the full name pools
    /// or re-sorting the entire index. Only the affected entries and sorted
    /// positions are touched.
    pub fn apply_changes(&self, store: &Store, changed: &[SlotId]) -> IndexSnapshot {
        let mut next = self.clone();
        next.generation = self.generation + 1;

        for &slot in changed {
            let old = self.entry_overrides.get(&slot).copied().or_else(|| {
                (slot as usize).lt(&self.entries.len()).then(|| self.entries[slot as usize])
            });

            if let Some(old) = old {
                if let Ok(p) = sorted_pos(&next.by_name, |s| {
                    sort_key_folded(self, s)
                        .cmp(sort_key_folded(self, slot))
                        .then_with(|| s.cmp(&slot))
                }) {
                    next.by_name.remove(p);
                }
                if let Ok(p) = sorted_pos(&next.by_name_cs, |s| {
                    sort_key_by_name(self, s)
                        .cmp(sort_key_by_name(self, slot))
                        .then_with(|| s.cmp(&slot))
                }) {
                    next.by_name_cs.remove(p);
                }
                if let Some(v) = next.by_parent.get_mut(&old.parent) {
                    v.retain(|&s| s != slot);
                }
                if old.kind == EntryType::Dir as u8 {
                    next.dir_count = next.dir_count.saturating_sub(1);
                } else if old.kind == EntryType::File as u8 {
                    next.file_count = next.file_count.saturating_sub(1);
                }
                if !store.is_live(slot) {
                    let mut dead = old;
                    dead.kind = EntryType::Other as u8;
                    dead.name_len = 0;
                    next.entry_overrides.insert(slot, dead);
                    continue;
                }
            }

            if !store.is_live(slot) {
                continue;
            }

            let name = store.name(slot);
            let name_off = (next.names.len() + next.name_appends.len()) as u32;
            next.name_appends.extend_from_slice(name);
            next.name_appends.push(0);
            let folded_bytes = fold_name(name);
            let fold_off = (next.folded.len() + next.folded_appends.len()) as u32;
            next.folded_appends.extend_from_slice(&folded_bytes);
            next.folded_appends.push(0);

            let meta = store.meta(slot).copied().unwrap_or_default();
            let parent = store.parent(slot).unwrap_or(crate::model::NULL_SLOT);
            let root = store.root_index_for(slot).unwrap_or(usize::MAX) as u16;
            let entry = SnapEntry {
                name_off,
                name_len: name.len() as u16,
                fold_off,
                parent,
                kind: meta.kind as u8,
                root,
                size: meta.size,
                mtime: meta.mtime,
                created: meta.created,
                accessed: meta.accessed,
            };
            next.entry_overrides.insert(slot, entry);

            let pos = sorted_pos(&next.by_name, |s| {
                sort_key_folded(&next, s)
                    .cmp(sort_key_folded(&next, slot))
                    .then_with(|| s.cmp(&slot))
            })
            .unwrap_or_else(|p| p);
            next.by_name.insert(pos, slot);

            let pos = sorted_pos(&next.by_name_cs, |s| {
                sort_key_by_name(&next, s)
                    .cmp(sort_key_by_name(&next, slot))
                    .then_with(|| s.cmp(&slot))
            })
            .unwrap_or_else(|p| p);
            next.by_name_cs.insert(pos, slot);

            next.by_parent.entry(parent).or_default().push(slot);
            if meta.kind == EntryType::Dir {
                next.dir_count += 1;
            } else if meta.kind == EntryType::File {
                next.file_count += 1;
            }
        }
        next
    }
}

pub fn build_snapshot(store: &Store, generation: u64) -> IndexSnapshot {
    let mut names = Vec::with_capacity(store.name_pool.len());
    let mut folded = Vec::with_capacity(store.name_pool.len());
    let mut entries = Vec::with_capacity(store.len());
    let mut by_parent: HashMap<SlotId, Vec<u32>> = HashMap::new();
    let mut file_count = 0u64;
    let mut dir_count = 0u64;

    for slot in 0..store.len() {
        let slot = slot as SlotId;
        if !store.is_live(slot) {
            continue;
        }
        let name = store.name(slot);
        let name_off = names.len() as u32;
        names.extend_from_slice(name);
        names.push(0);
        let folded_bytes = fold_name(name);
        let fold_off = folded.len() as u32;
        folded.extend_from_slice(&folded_bytes);
        folded.push(0);

        let meta = store.meta(slot).copied().unwrap_or_default();
        let root = store.root_index_for(slot).unwrap_or(usize::MAX) as u16;
        entries.push(SnapEntry {
            name_off,
            name_len: name.len() as u16,
            fold_off,
            parent: store.parent(slot).unwrap_or(crate::model::NULL_SLOT),
            kind: meta.kind as u8,
            root,
            size: meta.size,
            mtime: meta.mtime,
            created: meta.created,
            accessed: meta.accessed,
        });
        if meta.kind == EntryType::Dir {
            dir_count += 1;
        } else if meta.kind == EntryType::File {
            file_count += 1;
        }
        if let Some(p) = store.parent(slot) {
            by_parent.entry(p).or_default().push(slot);
        }
    }

    let n = entries.len();
    let mut by_name: Vec<u32> = (0..n as u32).collect();
    let mut by_name_cs: Vec<u32> = (0..n as u32).collect();
    let folded_ref = &folded;
    let names_ref = &names;
    let entries_ref = &entries;
    by_name.par_sort_unstable_by(|&a, &b| {
        let ea = entries_ref[a as usize];
        let eb = entries_ref[b as usize];
        let sa = &folded_ref[ea.fold_off as usize..ea.fold_off as usize + ea.name_len as usize];
        let sb = &folded_ref[eb.fold_off as usize..eb.fold_off as usize + eb.name_len as usize];
        sa.cmp(sb).then_with(|| a.cmp(&b))
    });
    by_name_cs.par_sort_unstable_by(|&a, &b| {
        let ea = entries_ref[a as usize];
        let eb = entries_ref[b as usize];
        let sa = &names_ref[ea.name_off as usize..ea.name_off as usize + ea.name_len as usize];
        let sb = &names_ref[eb.name_off as usize..eb.name_off as usize + eb.name_len as usize];
        sa.cmp(sb).then_with(|| a.cmp(&b))
    });

    IndexSnapshot {
        names: Arc::new(names),
        folded: Arc::new(folded),
        entries: Arc::new(entries),
        entry_overrides: HashMap::new(),
        name_appends: Vec::new(),
        folded_appends: Vec::new(),
        by_name,
        by_name_cs,
        by_parent,
        roots: store.roots.clone(),
        file_count,
        dir_count,
        generation,
    }
}
