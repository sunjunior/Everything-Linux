use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::error::{Error, Result};
use crate::index::IndexSnapshot;
use crate::store::Store;

const MAGIC: &[u8; 4] = b"ELDB";
const VERSION: u32 = 2;

fn put_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn put_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(Error::Snapshot("unexpected EOF".into()));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
}

pub fn save(store: &Store, path: &Path, generation: u64) -> Result<()> {
    let mut raw = Vec::with_capacity(store.name_pool.len() + store.len() * 40);
    raw.extend_from_slice(MAGIC);
    put_u32(&mut raw, VERSION);
    put_u64(&mut raw, generation);

    put_u32(&mut raw, store.roots.len() as u32);
    for root in &store.roots {
        let bytes = root.path.as_os_str().as_encoded_bytes();
        put_u16(&mut raw, bytes.len() as u16);
        raw.extend_from_slice(bytes);
        put_u32(&mut raw, root.slot);
    }

    put_u32(&mut raw, store.name_pool.len() as u32);
    raw.extend_from_slice(&store.name_pool);

    let live_count = store.live.iter().filter(|&&b| b).count();
    put_u32(&mut raw, live_count as u32);
    for slot in 0..store.len() {
        let slot = slot as u32;
        if !store.is_live(slot) {
            continue;
        }
        let m = store.meta(slot).unwrap();
        put_u32(&mut raw, store.name_offs[slot as usize]);
        put_u32(&mut raw, store.parents[slot as usize]);
        put_u8(&mut raw, m.kind as u8);
        put_u8(&mut raw, m.flags);
        put_u64(&mut raw, m.ino);
        put_u64(&mut raw, m.dev);
        put_u64(&mut raw, m.size);
        put_u64(&mut raw, m.mtime as u64);
        put_u64(&mut raw, m.created as u64);
        put_u64(&mut raw, m.accessed as u64);
    }

    let compressed = zstd::encode_all(raw.as_slice(), 1)
        .map_err(|e| Error::Snapshot(format!("compress: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, compressed)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn put_u8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}

pub fn load(path: &Path) -> Result<Option<(Store, u64)>> {
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read(path)?;
    let raw = zstd::decode_all(data.as_slice())
        .map_err(|e| Error::Snapshot(format!("decompress: {e}")))?;
    let mut r = Reader {
        buf: &raw,
        pos: 0,
    };
    if r.take(4)? != MAGIC {
        return Err(Error::Snapshot("bad magic".into()));
    }
    let version = r.u32()?;
    if version != 1 && version != VERSION {
        return Err(Error::Snapshot(format!("unsupported version {version}")));
    }
    let generation = r.u64()?;

    let mut store = Store::new();
    let root_count = r.u32()?;
    for _ in 0..root_count {
        let len = r.u16()? as usize;
        let bytes = r.take(len)?;
        let slot = r.u32()?;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()));
        store.roots.push(crate::model::RootInfo { path, slot });
    }

    let pool_len = r.u32()? as usize;
    store.name_pool = r.take(pool_len)?.to_vec();

    let count = r.u32()?;
    for _ in 0..count {
        let name_off = r.u32()?;
        let parent = r.u32()?;
        let kind = r.u8()?;
        let flags = r.u8()?;
        let ino = r.u64()?;
        let dev = r.u64()?;
        let size = r.u64()?;
        let mtime = r.u64()? as i64;
        let (created, accessed) = if version >= 2 {
            (r.u64()? as i64, r.u64()? as i64)
        } else {
            (mtime, mtime)
        };
        let meta = crate::model::EntryMeta {
            ino,
            dev,
            kind: crate::model::EntryType::from_u8(kind),
            size,
            mtime,
            created,
            accessed,
            flags,
        };
        let slot = store.add_entry(parent, name_off, meta);
        debug_assert_eq!(slot, store.len() as u32 - 1);
    }

    for slot in 0..store.len() {
        let slot = slot as u32;
        let name = store.name(slot).to_vec();
        store.name_dedup.insert(name, store.name_offs[slot as usize]);
    }

    Ok(Some((store, generation)))
}

pub type SharedIndex = Arc<ArcSwap<IndexSnapshot>>;

pub fn publish(index: &SharedIndex, snapshot: IndexSnapshot) {
    index.store(Arc::new(snapshot));
}
