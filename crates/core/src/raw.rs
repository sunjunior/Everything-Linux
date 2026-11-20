use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{EntryType, NULL_SLOT, RootInfo, SlotId};
use std::os::unix::ffi::OsStringExt;

use crate::store::{Entry, SAME_FOLD, Store};

const MAGIC: &[u8; 4] = b"ELDR";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 256;
const ENTRY_LEN: usize = 40;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawEntry {
    pub size: u64,
    pub mtime: i64,
    pub created: u32,
    pub accessed: u32,
    pub name_off: u32,
    pub fold_off: u32,
    pub parent: u32,
    pub name_len: u16,
    pub kind: u8,
    pub flags: u8,
}

const _: () = assert!(std::mem::size_of::<RawEntry>() == ENTRY_LEN);

struct Mapping {
    ptr: *mut libc::c_void,
    len: usize,
}

impl Mapping {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len() as usize;
        if len < HEADER_LEN {
            return Err(Error::Snapshot("raw snapshot too small".into()));
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(Self { ptr, len })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}

unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

pub struct MappedIndex {
    map: Mapping,
    slot_count: usize,
    generation: u64,
    file_count: u64,
    dir_count: u64,
    entries_off: usize,
    live_off: usize,
    live_len: usize,
    names_off: usize,
    names_len: usize,
    folds_off: usize,
    folds_len: usize,
    roots: Vec<RootInfo>,
}

impl MappedIndex {
    pub fn open(path: &Path) -> Result<Self> {
        let map = Mapping::open(path)?;
        let base = unsafe { std::slice::from_raw_parts(map.ptr as *const u8, map.len) };
        if &base[0..4] != MAGIC {
            return Err(Error::Snapshot("bad raw magic".into()));
        }
        let version = u32_at(base, 4);
        if version != VERSION {
            return Err(Error::Snapshot(format!(
                "unsupported raw version {version}"
            )));
        }
        let header_len = u32_at(base, 8) as usize;
        let entry_len = u32_at(base, 12) as usize;
        if header_len < HEADER_LEN || header_len > base.len() || entry_len != ENTRY_LEN {
            return Err(Error::Snapshot("bad raw header".into()));
        }
        let generation = u64_at(base, 16);
        let slot_count = u32_at(base, 24) as usize;
        let root_count = u32_at(base, 28) as usize;
        let file_count = u64_at(base, 32);
        let dir_count = u64_at(base, 40);
        let entries_off = u64_at(base, 48) as usize;
        let live_off = u64_at(base, 56) as usize;
        let names_off = u64_at(base, 64) as usize;
        let folds_off = u64_at(base, 72) as usize;
        let names_len = u64_at(base, 80) as usize;
        let folds_len = u64_at(base, 88) as usize;
        let live_len = u64_at(base, 96) as usize;
        let roots_off = u64_at(base, 104) as usize;
        let roots_len = u64_at(base, 112) as usize;

        let entries_end = entries_off
            .checked_add(slot_count.saturating_mul(ENTRY_LEN))
            .ok_or_else(|| Error::Snapshot("entry range overflow".into()))?;
        let live_end = live_off
            .checked_add(live_len)
            .ok_or_else(|| Error::Snapshot("live range overflow".into()))?;
        let names_end = names_off
            .checked_add(names_len)
            .ok_or_else(|| Error::Snapshot("name range overflow".into()))?;
        let folds_end = folds_off
            .checked_add(folds_len)
            .ok_or_else(|| Error::Snapshot("fold range overflow".into()))?;
        let roots_end = roots_off
            .checked_add(roots_len)
            .ok_or_else(|| Error::Snapshot("root range overflow".into()))?;
        for end in [entries_end, live_end, names_end, folds_end, roots_end] {
            if end > base.len() {
                return Err(Error::Snapshot("raw section out of range".into()));
            }
        }
        if entries_off % 8 != 0 || live_off % 8 != 0 {
            return Err(Error::Snapshot("raw section misaligned".into()));
        }

        let mut roots = Vec::with_capacity(root_count);
        let mut pos = roots_off;
        for _ in 0..root_count {
            if pos + 2 > roots_end {
                return Err(Error::Snapshot("bad raw roots".into()));
            }
            let len = u16::from_le_bytes([base[pos], base[pos + 1]]) as usize;
            pos += 2;
            if pos + len + 4 > roots_end {
                return Err(Error::Snapshot("bad raw root path".into()));
            }
            let path = PathBuf::from(std::ffi::OsString::from_vec(base[pos..pos + len].to_vec()));
            pos += len;
            let slot = u32::from_le_bytes(base[pos..pos + 4].try_into().unwrap());
            pos += 4;
            if slot as usize >= slot_count {
                return Err(Error::Snapshot("raw root slot out of range".into()));
            }
            roots.push(RootInfo { path, slot });
        }

        Ok(Self {
            map,
            slot_count,
            generation,
            file_count,
            dir_count,
            entries_off,
            live_off,
            live_len,
            names_off,
            names_len,
            folds_off,
            folds_len,
            roots,
        })
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.map.ptr as *const u8, self.map.len) }
    }

    pub fn len(&self) -> usize {
        self.slot_count
    }

    pub fn is_empty(&self) -> bool {
        self.slot_count == 0
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn file_count(&self) -> u64 {
        self.file_count
    }

    pub fn dir_count(&self) -> u64 {
        self.dir_count
    }

    pub fn roots(&self) -> &[RootInfo] {
        &self.roots
    }

    pub fn entry(&self, slot: SlotId) -> Option<&RawEntry> {
        if slot as usize >= self.slot_count {
            return None;
        }
        let base = self.bytes();
        let off = self.entries_off + slot as usize * ENTRY_LEN;
        let ptr = unsafe { base.as_ptr().add(off) as *const RawEntry };
        Some(unsafe { &*ptr })
    }

    pub fn is_live(&self, slot: SlotId) -> bool {
        if slot as usize >= self.slot_count || self.live_len < 8 {
            return false;
        }
        let word = slot as usize / 64;
        let off = self.live_off + word * 8;
        if off + 8 > self.live_off + self.live_len {
            return false;
        }
        let base = self.bytes();
        let w = u64::from_le_bytes(base[off..off + 8].try_into().unwrap());
        (w >> (slot as usize % 64)) & 1 == 1
    }

    pub fn name(&self, slot: SlotId) -> &[u8] {
        let Some(e) = self.entry(slot) else {
            return &[];
        };
        let start = self.names_off + e.name_off as usize;
        let end = start + e.name_len as usize;
        if end > self.names_off + self.names_len {
            return &[];
        }
        &self.bytes()[start..end]
    }

    pub fn folded(&self, slot: SlotId) -> &[u8] {
        let Some(e) = self.entry(slot) else {
            return &[];
        };
        if e.fold_off == SAME_FOLD {
            return self.name(slot);
        }
        let start = self.folds_off + e.fold_off as usize;
        if start >= self.folds_off + self.folds_len {
            return &[];
        }
        let rest = &self.bytes()[start..self.folds_off + self.folds_len];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        &rest[..end]
    }

    pub fn kind(&self, slot: SlotId) -> EntryType {
        self.entry(slot)
            .map(|e| EntryType::from_u8(e.kind))
            .unwrap_or(EntryType::Other)
    }

    pub fn path(&self, slot: SlotId, buf: &mut Vec<u8>) {
        buf.clear();
        let mut cur = slot;
        let mut parts: smallvec::SmallVec<[SlotId; 64]> = smallvec::SmallVec::new();
        let mut depth = 0usize;
        loop {
            let Some(e) = self.entry(cur) else {
                return;
            };
            if e.parent == NULL_SLOT {
                break;
            }
            parts.push(cur);
            cur = e.parent;
            depth += 1;
            if depth > 4096 {
                return;
            }
        }
        if let Some(root) = self.roots.iter().find(|r| r.slot == cur) {
            buf.extend_from_slice(root.path.as_os_str().as_encoded_bytes());
        }
        for &s in parts.iter().rev() {
            if !buf.is_empty() && !buf.ends_with(b"/") {
                buf.push(b'/');
            }
            buf.extend_from_slice(self.name(s));
        }
    }
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn align8(v: usize) -> usize {
    (v + 7) & !7
}

pub fn write_raw(store: &Store, path: &Path, generation: u64) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("rawtmp");

    let mut roots = Vec::new();
    for root in &store.roots {
        let bytes = root.path.as_os_str().as_encoded_bytes();
        roots.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        roots.extend_from_slice(bytes);
        roots.extend_from_slice(&root.slot.to_le_bytes());
    }

    let roots_off = HEADER_LEN;
    let roots_len = roots.len();
    let names_off = align8(roots_off + roots_len);
    let names_len = store.name_pool.len();
    let folds_off = align8(names_off + names_len);
    let folds_len = store.fold_pool.len();
    let entries_off = align8(folds_off + folds_len);
    let slot_count = store.entries.len();
    let entries_len = slot_count * ENTRY_LEN;
    let live_off = align8(entries_off + entries_len);
    let live_words = slot_count.div_ceil(64);
    let live_len = live_words * 8;

    let mut header = vec![0u8; HEADER_LEN];
    header[0..4].copy_from_slice(MAGIC);
    put_u32(&mut header, 4, VERSION);
    put_u32(&mut header, 8, HEADER_LEN as u32);
    put_u32(&mut header, 12, ENTRY_LEN as u32);
    put_u64(&mut header, 16, generation);
    put_u32(&mut header, 24, slot_count as u32);
    put_u32(&mut header, 28, store.roots.len() as u32);
    put_u64(&mut header, 32, store.file_count());
    put_u64(&mut header, 40, store.dir_count());
    put_u64(&mut header, 48, entries_off as u64);
    put_u64(&mut header, 56, live_off as u64);
    put_u64(&mut header, 64, names_off as u64);
    put_u64(&mut header, 72, folds_off as u64);
    put_u64(&mut header, 80, names_len as u64);
    put_u64(&mut header, 88, folds_len as u64);
    put_u64(&mut header, 96, live_len as u64);
    put_u64(&mut header, 104, roots_off as u64);
    put_u64(&mut header, 112, roots_len as u64);

    let file = File::create(&tmp)?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    w.write_all(&header)?;
    w.write_all(&roots)?;
    write_pad(&mut w, names_off - (roots_off + roots_len))?;
    w.write_all(&store.name_pool)?;
    write_pad(&mut w, folds_off - (names_off + names_len))?;
    w.write_all(&store.fold_pool)?;
    write_pad(&mut w, entries_off - (folds_off + folds_len))?;

    let mut chunk = Vec::with_capacity(8192 * ENTRY_LEN);
    for slot in 0..slot_count {
        let e: Entry = store.entries[slot];
        let raw = RawEntry {
            size: e.size,
            mtime: e.mtime,
            created: e.created,
            accessed: e.accessed,
            name_off: e.name_off,
            fold_off: e.fold_off,
            parent: e.parent,
            name_len: e.name_len,
            kind: e.kind,
            flags: e.flags,
        };
        chunk.extend_from_slice(unsafe {
            std::slice::from_raw_parts(&raw as *const RawEntry as *const u8, ENTRY_LEN)
        });
        if chunk.len() >= 8192 * ENTRY_LEN {
            w.write_all(&chunk)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        w.write_all(&chunk)?;
    }
    write_pad(&mut w, live_off - (entries_off + entries_len))?;

    let live = &store.live;
    let mut live_bytes = Vec::with_capacity(live_len);
    for word in live {
        live_bytes.extend_from_slice(&word.to_le_bytes());
    }
    live_bytes.resize(live_len, 0);
    w.write_all(&live_bytes)?;
    w.flush()?;
    drop(w);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn write_pad<W: Write>(w: &mut W, mut n: usize) -> Result<()> {
    static ZEROS: [u8; 8] = [0; 8];
    while n > 0 {
        let take = n.min(8);
        w.write_all(&ZEROS[..take])?;
        n -= take;
    }
    Ok(())
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
