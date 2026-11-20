use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;

use crate::index::fold_name;
use crate::model::{EntryMeta, EntryType, NULL_SLOT, RootInfo, SlotId};

/// Marker for `Entry::fold_off` when the folded name is identical to the name.
pub const SAME_FOLD: u32 = u32::MAX;

/// Compact, cache-friendly index entry. Kept at 48 bytes so a million entries
/// cost ~48 MB instead of the old duplicated `EntryMeta` + `SnapEntry` pair.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Entry {
    pub size: u64,
    pub mtime: i64,
    pub created: u32,
    pub accessed: u32,
    pub name_off: u32,
    pub fold_off: u32,
    pub parent: SlotId,
    pub first_child: SlotId,
    pub next_sibling: SlotId,
    pub name_len: u16,
    pub kind: u8,
    pub flags: u8,
}

impl Entry {
    pub fn kind(&self) -> EntryType {
        EntryType::from_u8(self.kind)
    }
}

#[derive(Default)]
pub struct Store {
    pub name_pool: Vec<u8>,
    pub fold_pool: Vec<u8>,
    pub entries: Vec<Entry>,
    pub(crate) live: Vec<u64>,
    pub free: Vec<SlotId>,
    pub roots: Vec<RootInfo>,
    pub(crate) file_count: u64,
    pub(crate) dir_count: u64,
}

pub type SharedStore = RwLock<Store>;

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn file_count(&self) -> u64 {
        self.file_count
    }

    pub fn dir_count(&self) -> u64 {
        self.dir_count
    }

    pub fn is_live(&self, slot: SlotId) -> bool {
        let idx = slot as usize;
        let word = idx / 64;
        let bit = idx % 64;
        self.live
            .get(word)
            .map(|w| (w >> bit) & 1 == 1)
            .unwrap_or(false)
    }

    fn set_live(&mut self, slot: SlotId, value: bool) {
        let idx = slot as usize;
        let word = idx / 64;
        if word >= self.live.len() {
            self.live.resize(word + 1, 0);
        }
        let bit = idx % 64;
        if value {
            self.live[word] |= 1 << bit;
        } else {
            self.live[word] &= !(1 << bit);
        }
    }

    pub fn entry(&self, slot: SlotId) -> Option<&Entry> {
        self.entries.get(slot as usize)
    }

    pub fn kind(&self, slot: SlotId) -> EntryType {
        self.entries
            .get(slot as usize)
            .map(Entry::kind)
            .unwrap_or(EntryType::Other)
    }

    pub fn parent(&self, slot: SlotId) -> Option<SlotId> {
        self.entries.get(slot as usize).map(|e| e.parent)
    }

    fn intern_name(&mut self, name: &[u8]) -> (u32, u32, u16) {
        let off = self.name_pool.len() as u32;
        self.name_pool.extend_from_slice(name);
        self.name_pool.push(0);
        let folded = fold_name(name);
        let fold_off = if folded == name {
            SAME_FOLD
        } else {
            let o = self.fold_pool.len() as u32;
            self.fold_pool.extend_from_slice(&folded);
            self.fold_pool.push(0);
            o
        };
        (off, fold_off, name.len() as u16)
    }

    pub fn name(&self, slot: SlotId) -> &[u8] {
        match self.entries.get(slot as usize) {
            Some(e) => {
                let off = e.name_off as usize;
                &self.name_pool[off..off + e.name_len as usize]
            }
            None => &[],
        }
    }

    pub fn folded(&self, slot: SlotId) -> &[u8] {
        let Some(e) = self.entries.get(slot as usize) else {
            return &[];
        };
        if e.fold_off == SAME_FOLD {
            return self.name(slot);
        }
        let start = e.fold_off as usize;
        let rest = &self.fold_pool[start..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        &rest[..end]
    }

    pub fn add_entry(&mut self, parent: SlotId, name: &[u8], meta: EntryMeta) -> SlotId {
        let (name_off, fold_off, name_len) = self.intern_name(name);
        let entry = Entry {
            size: meta.size,
            mtime: meta.mtime,
            created: to_u32_time(meta.created),
            accessed: to_u32_time(meta.accessed),
            name_off,
            fold_off,
            parent,
            first_child: NULL_SLOT,
            next_sibling: NULL_SLOT,
            name_len,
            kind: meta.kind as u8,
            flags: meta.flags,
        };

        let slot = if let Some(s) = self.free.pop() {
            self.entries[s as usize] = entry;
            s
        } else {
            let s = self.entries.len() as SlotId;
            self.entries.push(entry);
            s
        };
        self.set_live(slot, true);
        self.link_child(parent, slot);
        self.bump_kind(meta.kind, 1);
        slot
    }

    fn link_child(&mut self, parent: SlotId, slot: SlotId) {
        let p = parent as usize;
        if let Some(pe) = self.entries.get_mut(p) {
            let first = pe.first_child;
            pe.first_child = slot;
            self.entries[slot as usize].next_sibling = first;
        }
    }

    fn unlink_child(&mut self, parent: SlotId, slot: SlotId) {
        let p = parent as usize;
        if p >= self.entries.len() {
            return;
        }
        let first = self.entries[p].first_child;
        if first == slot {
            self.entries[p].first_child = self.entries[slot as usize].next_sibling;
            return;
        }
        let mut cur = first;
        while cur != NULL_SLOT {
            let next = self.entries[cur as usize].next_sibling;
            if next == slot {
                let sib = self.entries[slot as usize].next_sibling;
                self.entries[cur as usize].next_sibling = sib;
                return;
            }
            cur = next;
        }
    }

    pub fn children(&self, slot: SlotId) -> impl Iterator<Item = SlotId> + '_ {
        let mut cur = self
            .entries
            .get(slot as usize)
            .map(|e| e.first_child)
            .unwrap_or(NULL_SLOT);
        std::iter::from_fn(move || {
            if cur == NULL_SLOT {
                return None;
            }
            let next = self.entries[cur as usize].next_sibling;
            let out = cur;
            cur = next;
            Some(out)
        })
    }

    pub fn find_child(&self, parent: SlotId, name: &[u8]) -> Option<SlotId> {
        for child in self.children(parent) {
            if self.name(child) == name {
                return Some(child);
            }
        }
        None
    }

    pub fn lookup(&self, parent: SlotId, name: &[u8]) -> Option<SlotId> {
        self.find_child(parent, name)
    }

    pub fn set_meta(&mut self, slot: SlotId, meta: EntryMeta) {
        let Some(e) = self.entries.get_mut(slot as usize) else {
            return;
        };
        let old_kind = EntryType::from_u8(e.kind);
        e.size = meta.size;
        e.mtime = meta.mtime;
        e.created = to_u32_time(meta.created);
        e.accessed = to_u32_time(meta.accessed);
        e.kind = meta.kind as u8;
        e.flags = meta.flags;
        if old_kind != meta.kind {
            self.bump_kind(old_kind, -1);
            self.bump_kind(meta.kind, 1);
        }
    }

    fn bump_kind(&mut self, kind: EntryType, delta: i64) {
        match kind {
            EntryType::File => self.file_count = add_signed(self.file_count, delta),
            EntryType::Dir => self.dir_count = add_signed(self.dir_count, delta),
            _ => {}
        }
    }

    pub fn remove_entry(&mut self, slot: SlotId) -> bool {
        if slot as usize >= self.entries.len() || !self.is_live(slot) {
            return false;
        }
        let e = self.entries[slot as usize];
        self.unlink_child(e.parent, slot);
        self.bump_kind(e.kind(), -1);
        self.set_live(slot, false);
        self.entries[slot as usize] = Entry::default();
        if !self.free.contains(&slot) {
            self.free.push(slot);
        }
        self.roots.retain(|r| r.slot != slot);
        true
    }

    pub fn remove_subtree(&mut self, root: SlotId) -> Vec<SlotId> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(slot) = stack.pop() {
            if slot as usize >= self.entries.len() || !self.is_live(slot) {
                continue;
            }
            out.push(slot);
            stack.extend(self.children(slot));
        }
        for &slot in &out {
            self.remove_entry(slot);
        }
        out
    }

    /// Compute the case-insensitive name order on demand. Queries only sort
    /// the entries that matched, so the index no longer keeps a 4 MB array and
    /// does not pay a full sort on every load.
    pub fn name_order(&self) -> Vec<SlotId> {
        let mut order: Vec<SlotId> = (0..self.entries.len() as SlotId)
            .filter(|&s| self.is_live(s))
            .collect();
        order.sort_unstable_by(|&a, &b| self.folded(a).cmp(self.folded(b)).then_with(|| a.cmp(&b)));
        order
    }

    /// Rebuild parent/child links after loading a snapshot.
    pub(crate) fn rebuild_after_load(&mut self) {
        for slot in 0..self.entries.len() {
            self.entries[slot].first_child = NULL_SLOT;
            self.entries[slot].next_sibling = NULL_SLOT;
        }
        for slot in 0..self.entries.len() {
            if !self.is_live(slot as SlotId) {
                continue;
            }
            let parent = self.entries[slot].parent;
            if parent != NULL_SLOT && (parent as usize) < self.entries.len() && self.is_live(parent)
            {
                let first = self.entries[parent as usize].first_child;
                self.entries[slot].next_sibling = first;
                self.entries[parent as usize].first_child = slot as SlotId;
            }
        }
    }

    pub fn path(&self, slot: SlotId, buf: &mut Vec<u8>) {
        buf.clear();
        let mut cur = slot;
        let mut chain: smallvec::SmallVec<[SlotId; 64]> = smallvec::SmallVec::new();
        loop {
            let Some(e) = self.entries.get(cur as usize) else {
                return;
            };
            if e.parent == NULL_SLOT {
                break;
            }
            chain.push(cur);
            cur = e.parent;
        }
        if let Some(root) = self.roots.iter().find(|r| r.slot == cur) {
            buf.extend_from_slice(root.path.as_os_str().as_encoded_bytes());
        }
        for &s in chain.iter().rev() {
            if !buf.is_empty() && !buf.ends_with(b"/") {
                buf.push(b'/');
            }
            buf.extend_from_slice(self.name(s));
        }
    }

    pub fn path_of(&self, slot: SlotId) -> Option<PathBuf> {
        let mut buf = Vec::new();
        self.path(slot, &mut buf);
        if buf.is_empty() {
            None
        } else {
            Some(PathBuf::from(std::ffi::OsString::from_vec(std::mem::take(
                &mut buf,
            ))))
        }
    }

    pub fn root_index_for(&self, slot: SlotId) -> Option<usize> {
        let mut cur = slot;
        loop {
            let e = self.entries.get(cur as usize)?;
            if e.parent == NULL_SLOT {
                return self.roots.iter().position(|r| r.slot == cur);
            }
            cur = e.parent;
        }
    }

    pub fn root_path(&self, slot: SlotId) -> Option<&Path> {
        self.roots
            .iter()
            .find(|r| r.slot == slot)
            .map(|r| r.path.as_path())
    }
}

fn to_u32_time(v: i64) -> u32 {
    if v <= 0 {
        0
    } else if v > u32::MAX as i64 {
        u32::MAX
    } else {
        v as u32
    }
}

fn add_signed(v: u64, delta: i64) -> u64 {
    if delta >= 0 {
        v.saturating_add(delta as u64)
    } else {
        v.saturating_sub((-delta) as u64)
    }
}
