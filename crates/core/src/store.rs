use std::collections::HashMap;

use dashmap::DashMap;
use parking_lot::RwLock;

use crate::model::{EntryKey, EntryMeta, RootInfo, SlotId, NULL_SLOT};

#[derive(Default)]
pub struct Store {
    pub name_pool: Vec<u8>,
    pub name_offs: Vec<u32>,
    pub parents: Vec<SlotId>,
    pub metas: Vec<EntryMeta>,
    pub live: Vec<bool>,
    pub free: Vec<SlotId>,
    pub by_key: DashMap<EntryKey, SlotId>,
    pub children: HashMap<SlotId, Vec<SlotId>>,
    pub name_dedup: HashMap<Vec<u8>, u32>,
    pub roots: Vec<RootInfo>,
}

pub type SharedStore = RwLock<Store>;

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.metas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_live(&self, slot: SlotId) -> bool {
        self.live.get(slot as usize).copied().unwrap_or(false)
    }

    pub fn is_free(&self, slot: SlotId) -> bool {
        !self.is_live(slot)
    }

    pub fn intern_name(&mut self, name: &[u8]) -> u32 {
        if let Some(&off) = self.name_dedup.get(name) {
            return off;
        }
        let off = self.name_pool.len() as u32;
        self.name_pool.extend_from_slice(name);
        self.name_pool.push(0);
        self.name_dedup.insert(name.to_vec(), off);
        off
    }

    pub fn name(&self, slot: SlotId) -> &[u8] {
        let idx = slot as usize;
        let off = self.name_offs[idx] as usize;
        let end = self.name_pool[off..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| off + p)
            .unwrap_or(self.name_pool.len());
        &self.name_pool[off..end]
    }

    pub fn add_entry(
        &mut self,
        parent: SlotId,
        name_off: u32,
        meta: EntryMeta,
    ) -> SlotId {
        let slot = if let Some(s) = self.free.pop() {
            let idx = s as usize;
            self.name_offs[idx] = name_off;
            self.parents[idx] = parent;
            self.metas[idx] = meta;
            self.live[idx] = true;
            s
        } else {
            let s = self.metas.len() as SlotId;
            self.name_offs.push(name_off);
            self.parents.push(parent);
            self.metas.push(meta);
            self.live.push(true);
            s
        };
        self.by_key.insert(EntryKey { parent, name_off }, slot);
        self.children.entry(parent).or_default().push(slot);
        slot
    }

    pub fn remove_entry(&mut self, slot: SlotId) -> bool {
        let idx = slot as usize;
        if idx >= self.metas.len() {
            return false;
        }
        let parent = self.parents[idx];
        let name_off = self.name_offs[idx];
        self.by_key.remove(&EntryKey { parent, name_off });
        if let Some(v) = self.children.get_mut(&parent) {
            v.retain(|&x| x != slot);
        }
        self.children.remove(&slot);
        self.metas[idx] = EntryMeta::default();
        self.parents[idx] = NULL_SLOT;
        self.name_offs[idx] = 0;
        self.live[idx] = false;
        self.free.push(slot);
        self.roots.retain(|r| r.slot != slot);
        true
    }

    pub fn lookup(&self, parent: SlotId, name_off: u32) -> Option<SlotId> {
        self.by_key
            .get(&EntryKey { parent, name_off })
            .map(|v| *v)
    }

    pub fn lookup_name(&self, parent: SlotId, name: &[u8]) -> Option<SlotId> {
        let off = *self.name_dedup.get(name)?;
        self.lookup(parent, off)
    }

    pub fn set_meta(&mut self, slot: SlotId, meta: EntryMeta) {
        if (slot as usize) < self.metas.len() {
            self.metas[slot as usize] = meta;
        }
    }

    pub fn meta(&self, slot: SlotId) -> Option<&EntryMeta> {
        self.metas.get(slot as usize)
    }

    pub fn parent(&self, slot: SlotId) -> Option<SlotId> {
        self.parents.get(slot as usize).copied()
    }

    pub fn root_index_for(&self, slot: SlotId) -> Option<usize> {
        self.roots.iter().position(|r| r.slot == slot)
    }
}
