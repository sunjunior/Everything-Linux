use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use crate::error::{Error, Result};
use crate::model::EntryMeta;
use crate::store::{Entry, SAME_FOLD, Store};

const MAGIC: &[u8; 4] = b"ELDB";
const VERSION: u32 = 3;
const RECORD_LEN: usize = 41;

pub fn save(store: &Store, path: &Path, generation: u64) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let file = File::create(&tmp)?;
        let mut enc = zstd::Encoder::new(BufWriter::new(file), 1)?;
        enc.write_all(MAGIC)?;
        enc.write_all(&VERSION.to_le_bytes())?;
        w_u64(&mut enc, generation)?;

        w_u32(&mut enc, store.roots.len() as u32)?;
        for root in &store.roots {
            let bytes = root.path.as_os_str().as_encoded_bytes();
            w_u16(&mut enc, bytes.len() as u16)?;
            enc.write_all(bytes)?;
            w_u32(&mut enc, root.slot)?;
        }

        w_u32(&mut enc, store.name_pool.len() as u32)?;
        enc.write_all(&store.name_pool)?;
        w_u32(&mut enc, store.fold_pool.len() as u32)?;
        enc.write_all(&store.fold_pool)?;

        w_u32(&mut enc, store.entries.len() as u32)?;
        w_u64(&mut enc, store.file_count())?;
        w_u64(&mut enc, store.dir_count())?;

        let mut buf = Vec::with_capacity(256 * 1024);
        for slot in 0..store.entries.len() {
            let e = store.entries[slot];
            buf.push(store.is_live(slot as u32) as u8);
            put_record(&mut buf, &e);
            if buf.len() >= 240 * 1024 {
                enc.write_all(&buf)?;
                buf.clear();
            }
        }
        if !buf.is_empty() {
            enc.write_all(&buf)?;
        }
        let mut w = enc.finish()?;
        w.flush()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn put_record(buf: &mut Vec<u8>, e: &Entry) {
    buf.extend_from_slice(&e.size.to_le_bytes());
    buf.extend_from_slice(&(e.mtime as u64).to_le_bytes());
    buf.extend_from_slice(&e.created.to_le_bytes());
    buf.extend_from_slice(&e.accessed.to_le_bytes());
    buf.extend_from_slice(&e.name_off.to_le_bytes());
    buf.extend_from_slice(&e.fold_off.to_le_bytes());
    buf.extend_from_slice(&e.parent.to_le_bytes());
    buf.extend_from_slice(&e.name_len.to_le_bytes());
    buf.push(e.kind);
    buf.push(e.flags);
}

fn parse_record(b: &[u8; RECORD_LEN - 1]) -> Entry {
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    Entry {
        size: u64_at(0),
        mtime: u64_at(8) as i64,
        created: u32_at(16),
        accessed: u32_at(20),
        name_off: u32_at(24),
        fold_off: u32_at(28),
        parent: u32_at(32),
        name_len: u16::from_le_bytes(b[36..38].try_into().unwrap()),
        kind: b[38],
        flags: b[39],
        first_child: crate::model::NULL_SLOT,
        next_sibling: crate::model::NULL_SLOT,
    }
}

fn w_u16<W: Write>(w: &mut W, v: u16) -> Result<()> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}
fn w_u32<W: Write>(w: &mut W, v: u32) -> Result<()> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}
fn w_u64<W: Write>(w: &mut W, v: u64) -> Result<()> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}

fn r_u8<R: Read>(r: &mut R) -> Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}
fn r_u16<R: Read>(r: &mut R) -> Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}
fn r_u32<R: Read>(r: &mut R) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn r_u64<R: Read>(r: &mut R) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

pub fn load(path: &Path) -> Result<Option<(Store, u64)>> {
    if !path.exists() {
        return Ok(None);
    }
    let file = File::open(path)?;
    let mut dec = zstd::Decoder::new(BufReader::new(file))?;
    let mut magic = [0u8; 4];
    dec.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(Error::Snapshot("bad magic".into()));
    }
    let version = r_u32(&mut dec)?;
    match version {
        1 | 2 => load_legacy(dec, version).map(Some),
        3 => load_v3(dec).map(Some),
        v => Err(Error::Snapshot(format!("unsupported version {v}"))),
    }
}

fn load_v3<R: Read>(mut r: R) -> Result<(Store, u64)> {
    let generation = r_u64(&mut r)?;
    let mut store = Store::new();

    let root_count = r_u32(&mut r)?;
    for _ in 0..root_count {
        let len = r_u16(&mut r)? as usize;
        let mut bytes = vec![0u8; len];
        r.read_exact(&mut bytes)?;
        let slot = r_u32(&mut r)?;
        store.roots.push(crate::model::RootInfo {
            path: std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes)),
            slot,
        });
    }

    let pool_len = r_u32(&mut r)? as usize;
    store.name_pool = vec![0u8; pool_len];
    r.read_exact(&mut store.name_pool)?;

    let fold_len = r_u32(&mut r)? as usize;
    store.fold_pool = vec![0u8; fold_len];
    r.read_exact(&mut store.fold_pool)?;

    let slot_count = r_u32(&mut r)? as usize;
    for root in &store.roots {
        if root.slot as usize >= slot_count {
            return Err(Error::Snapshot("root slot out of range".into()));
        }
    }
    let file_count = r_u64(&mut r)?;
    let dir_count = r_u64(&mut r)?;

    store.entries.reserve(slot_count);
    store.live.resize(slot_count.div_ceil(64), 0);
    const CHUNK: usize = 8192;
    let mut rec = vec![0u8; CHUNK * RECORD_LEN];
    let mut done = 0usize;
    while done < slot_count {
        let take = (slot_count - done).min(CHUNK);
        r.read_exact(&mut rec[..take * RECORD_LEN])?;
        for i in 0..take {
            let base = i * RECORD_LEN;
            let live = rec[base] != 0;
            let entry = parse_record(rec[base + 1..base + RECORD_LEN].try_into().unwrap());
            let slot = done + i;
            if live {
                if entry.parent != crate::model::NULL_SLOT && entry.parent as usize >= slot_count {
                    return Err(Error::Snapshot("parent slot out of range".into()));
                }
                if entry.name_off as usize + entry.name_len as usize > store.name_pool.len() {
                    return Err(Error::Snapshot("name offset out of range".into()));
                }
                if entry.fold_off != SAME_FOLD && entry.fold_off as usize >= store.fold_pool.len() {
                    return Err(Error::Snapshot("fold offset out of range".into()));
                }
            }
            if live {
                store.live[slot / 64] |= 1 << (slot % 64);
            } else {
                store.free.push(slot as u32);
            }
            store.entries.push(entry);
        }
        done += take;
    }
    store.file_count = file_count;
    store.dir_count = dir_count;
    store.rebuild_after_load();
    Ok((store, generation))
}

fn load_legacy<R: Read>(mut r: R, version: u32) -> Result<(Store, u64)> {
    let generation = r_u64(&mut r)?;
    let mut store = Store::new();

    let root_count = r_u32(&mut r)?;
    for _ in 0..root_count {
        let len = r_u16(&mut r)? as usize;
        let mut bytes = vec![0u8; len];
        r.read_exact(&mut bytes)?;
        let slot = r_u32(&mut r)?;
        store.roots.push(crate::model::RootInfo {
            path: std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes)),
            slot,
        });
    }

    let pool_len = r_u32(&mut r)? as usize;
    let mut pool = vec![0u8; pool_len];
    r.read_exact(&mut pool)?;

    let count = r_u32(&mut r)?;
    for _ in 0..count {
        let name_off = r_u32(&mut r)? as usize;
        let parent = r_u32(&mut r)?;
        let kind = r_u8(&mut r)?;
        let flags = r_u8(&mut r)?;
        let _ino = r_u64(&mut r)?;
        let _dev = r_u64(&mut r)?;
        let size = r_u64(&mut r)?;
        let mtime = r_u64(&mut r)? as i64;
        let (created, accessed) = if version >= 2 {
            (r_u64(&mut r)? as i64, r_u64(&mut r)? as i64)
        } else {
            (mtime, mtime)
        };
        if name_off >= pool.len() {
            return Err(Error::Snapshot("bad name offset".into()));
        }
        let rest = &pool[name_off..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        store.add_entry(
            parent,
            &rest[..end],
            EntryMeta {
                kind: crate::model::EntryType::from_u8(kind),
                size,
                mtime,
                created,
                accessed,
                flags,
            },
        );
    }
    Ok((store, generation))
}

/// Hint the allocator to return freed pages after large snapshot operations.
pub fn trim_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}
