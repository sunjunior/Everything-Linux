use crate::model::{EntryType, SlotId};
use crate::raw::MappedIndex;
use crate::store::Store;

#[derive(Clone, Copy)]
pub enum IndexView<'a> {
    Store(&'a Store),
    Raw(&'a MappedIndex),
}

impl<'a> IndexView<'a> {
    pub fn len(&self) -> usize {
        match self {
            Self::Store(s) => s.len(),
            Self::Raw(r) => r.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_live(&self, slot: SlotId) -> bool {
        match self {
            Self::Store(s) => s.is_live(slot),
            Self::Raw(r) => r.is_live(slot),
        }
    }

    pub fn name(&self, slot: SlotId) -> &[u8] {
        match self {
            Self::Store(s) => s.name(slot),
            Self::Raw(r) => r.name(slot),
        }
    }

    pub fn folded(&self, slot: SlotId) -> &[u8] {
        match self {
            Self::Store(s) => s.folded(slot),
            Self::Raw(r) => r.folded(slot),
        }
    }

    pub fn kind(&self, slot: SlotId) -> EntryType {
        match self {
            Self::Store(s) => s.kind(slot),
            Self::Raw(r) => r.kind(slot),
        }
    }

    pub fn size(&self, slot: SlotId) -> u64 {
        match self {
            Self::Store(s) => s.entry(slot).map(|e| e.size).unwrap_or(0),
            Self::Raw(r) => r.entry(slot).map(|e| e.size).unwrap_or(0),
        }
    }

    pub fn mtime(&self, slot: SlotId) -> i64 {
        match self {
            Self::Store(s) => s.entry(slot).map(|e| e.mtime).unwrap_or(0),
            Self::Raw(r) => r.entry(slot).map(|e| e.mtime).unwrap_or(0),
        }
    }

    pub fn created(&self, slot: SlotId) -> i64 {
        match self {
            Self::Store(s) => s.entry(slot).map(|e| e.created as i64).unwrap_or(0),
            Self::Raw(r) => r.entry(slot).map(|e| e.created as i64).unwrap_or(0),
        }
    }

    pub fn accessed(&self, slot: SlotId) -> i64 {
        match self {
            Self::Store(s) => s.entry(slot).map(|e| e.accessed as i64).unwrap_or(0),
            Self::Raw(r) => r.entry(slot).map(|e| e.accessed as i64).unwrap_or(0),
        }
    }

    pub fn recent(&self, slot: SlotId) -> i64 {
        self.mtime(slot).max(self.accessed(slot))
    }

    pub fn path(&self, slot: SlotId, buf: &mut Vec<u8>) {
        match self {
            Self::Store(s) => s.path(slot, buf),
            Self::Raw(r) => r.path(slot, buf),
        }
    }
}
