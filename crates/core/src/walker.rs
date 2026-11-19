use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use rayon::prelude::*;
use tracing::warn;

use crate::model::{EntryMeta, EntryType, F_HIDDEN, F_NOACCESS, NULL_SLOT, RootInfo};
use crate::store::SharedStore;

#[derive(Debug, Default)]
pub struct WalkStats {
    pub files: u64,
    pub dirs: u64,
    pub errors: u64,
}

pub struct WalkOptions<'a> {
    pub exclude: &'a [String],
    pub index_hidden: bool,
    pub follow_symlinks: bool,
    pub threads: usize,
}

fn is_hidden(name: &OsStr) -> bool {
    name.as_bytes().first() == Some(&b'.')
}

fn excluded(path: &Path, exclude: &[String]) -> bool {
    let s = path.to_string_lossy();
    exclude.iter().any(|pat| {
        if let Some(suffix) = pat.strip_prefix('*') {
            s.ends_with(suffix)
        } else if let Some(prefix) = pat.strip_suffix('*') {
            s.starts_with(prefix)
        } else {
            s.contains(pat)
        }
    })
}

fn meta_of(path: &Path, follow: bool) -> std::io::Result<std::fs::Metadata> {
    if follow {
        std::fs::metadata(path)
    } else {
        std::fs::symlink_metadata(path)
    }
}

unsafe extern "C" {
    fn statx(
        dirfd: libc::c_int,
        pathname: *const libc::c_char,
        flags: libc::c_int,
        mask: libc::c_uint,
        buf: *mut libc::statx,
    ) -> libc::c_int;
}

const STATX_ATIME: libc::c_uint = 0x20;
const STATX_MTIME: libc::c_uint = 0x40;
const STATX_CTIME: libc::c_uint = 0x80;
const STATX_BTIME: libc::c_uint = 0x800;

/// (created, accessed, modified) in unix seconds.
pub fn times_of(path: &Path, md: &std::fs::Metadata) -> (i64, i64, i64) {
    let atime = md.atime();
    let mtime = md.mtime();
    let ctime = md.ctime();
    if let Ok(cstr) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
        unsafe {
            let mut sx: libc::statx = std::mem::zeroed();
            let rc = statx(
                libc::AT_FDCWD,
                cstr.as_ptr(),
                0,
                STATX_ATIME | STATX_MTIME | STATX_CTIME | STATX_BTIME,
                &mut sx,
            );
            if rc == 0 {
                let created = if sx.stx_mask & STATX_BTIME != 0 {
                    sx.stx_btime.tv_sec
                } else {
                    sx.stx_ctime.tv_sec
                };
                return (created, sx.stx_atime.tv_sec, sx.stx_mtime.tv_sec);
            }
        }
    }
    (ctime, atime, mtime)
}

fn entry_meta(md: &std::fs::Metadata, path: &Path) -> EntryMeta {
    let ft = md.file_type();
    let kind = if ft.is_dir() {
        EntryType::Dir
    } else if ft.is_symlink() {
        EntryType::Symlink
    } else if ft.is_file() {
        EntryType::File
    } else {
        EntryType::Other
    };
    let mut flags = 0;
    if is_hidden(path.file_name().unwrap_or_default()) {
        flags |= F_HIDDEN;
    }
    let (created, accessed, modified) = times_of(path, md);
    EntryMeta {
        ino: md.ino(),
        dev: md.dev(),
        kind,
        size: md.size(),
        mtime: modified,
        created,
        accessed,
        flags,
    }
}

fn insert_dir_watch<F: Fn(&Path) + Sync>(path: &Path, cb: Option<&F>) {
    if let Some(cb) = cb {
        cb(path);
    }
}

pub fn scan_root<F>(
    root: &Path,
    store: &SharedStore,
    opts: &WalkOptions<'_>,
    on_dir: Option<&F>,
) -> anyhow::Result<WalkStats>
where
    F: Fn(&Path) + Sync,
{
    let root = std::fs::canonicalize(root).with_context(|| format!("canonicalize {}", root.display()))?;
    let stats = AtomicU64::new(0);
    let errors = AtomicU64::new(0);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.max(1))
        .build()
        .context("build rayon pool")?;

    pool.install(|| {
        let root_name = root
            .file_name()
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_else(|| b"/".to_vec());

        let md = meta_of(&root, false)?;
        let mut meta = entry_meta(&md, &root);
        meta.flags |= F_HIDDEN;
        let (root_slot, root_idx) = {
            let mut s = store.write();
            let name_off = s.intern_name(&root_name);
            let slot = s.add_entry(NULL_SLOT, name_off, meta);
            let idx = s.roots.len();
            s.roots.push(RootInfo {
                path: root.clone(),
                slot,
            });
            (slot, idx)
        };
        insert_dir_watch(&root, on_dir);
        stats.fetch_add(1, Ordering::Relaxed);

        walk_dir(
            &root,
            root_slot,
            root_idx,
            store,
            opts,
            on_dir,
            &stats,
            &errors,
        );
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(WalkStats {
        files: stats.load(Ordering::Relaxed),
        dirs: 0,
        errors: errors.load(Ordering::Relaxed),
    })
}

#[allow(clippy::too_many_arguments)]
fn walk_dir<F>(
    dir: &Path,
    parent_slot: u32,
    root_idx: usize,
    store: &SharedStore,
    opts: &WalkOptions<'_>,
    on_dir: Option<&F>,
    stats: &AtomicU64,
    errors: &AtomicU64,
) where
    F: Fn(&Path) + Sync,
{
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            warn!("read_dir {} failed: {e}", dir.display());
            errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let mut subdirs: Vec<(PathBuf, u32)> = Vec::new();

    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("read_dir entry failed: {e}");
                errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let path = entry.path();
        let name = entry.file_name();
        if !opts.index_hidden && is_hidden(&name) {
            continue;
        }
        if excluded(&path, opts.exclude) {
            continue;
        }
        let md = match meta_of(&path, opts.follow_symlinks) {
            Ok(md) => md,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!("stat {} failed: {e}", path.display());
                }
                errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let mut meta = entry_meta(&md, &path);
        if meta.flags & F_HIDDEN != 0 && !opts.index_hidden {
            continue;
        }
        if md.permissions().readonly() {
            meta.flags |= F_NOACCESS;
        }
        let is_dir = meta.kind == EntryType::Dir;

        let slot = {
            let mut s = store.write();
            let name_off = s.intern_name(name.as_bytes());
            let slot = s.add_entry(parent_slot, name_off, meta);
            let _ = root_idx;
            slot
        };
        stats.fetch_add(1, Ordering::Relaxed);

        if is_dir {
            insert_dir_watch(&path, on_dir);
            subdirs.push((path, slot));
        }
    }

    subdirs.into_par_iter().for_each(|(p, slot)| {
        walk_dir(&p, slot, root_idx, store, opts, on_dir, stats, errors);
    });
}

pub fn scan_subtree<F>(
    dir: &Path,
    parent_slot: u32,
    store: &SharedStore,
    opts: &WalkOptions<'_>,
    on_dir: Option<&F>,
) where
    F: Fn(&Path) + Sync,
{
    let stats = AtomicU64::new(0);
    let errors = AtomicU64::new(0);
    walk_dir(
        dir,
        parent_slot,
        usize::MAX,
        store,
        opts,
        on_dir,
        &stats,
        &errors,
    );
}

pub fn ino_of(md: &std::fs::Metadata) -> u64 {
    md.ino()
}

pub fn dev_of(md: &std::fs::Metadata) -> u64 {
    md.dev()
}

pub fn mtime_of(md: &std::fs::Metadata) -> i64 {
    md.mtime()
}

pub fn add_single(
    path: &Path,
    parent: u32,
    store: &SharedStore,
) -> anyhow::Result<Option<u32>> {
    let md = meta_of(path, false)?;
    let meta = entry_meta(&md, path);
    let name = path.file_name().unwrap_or_default();
    let mut s = store.write();
    let name_off = s.intern_name(name.as_bytes());
    let existing = s.lookup(parent, name_off);
    if let Some(slot) = existing {
        s.set_meta(slot, meta);
        return Ok(Some(slot));
    }
    Ok(Some(s.add_entry(parent, name_off, meta)))
}
