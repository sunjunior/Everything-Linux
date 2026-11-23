use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::RwLock;
use tracing::{debug, warn};

use crate::error::{Error, Result};

const WATCH_MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_MODIFY
    | libc::IN_CLOSE_WRITE
    | libc::IN_ATTRIB
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_IGNORED
    | libc::IN_UNMOUNT
    | libc::IN_ISDIR
    | libc::IN_ONLYDIR;

pub fn max_user_watches() -> usize {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(524_288)
        .max(1)
}

#[derive(Debug, Clone)]
pub enum FsEvent {
    Create(PathBuf),
    CreateDir(PathBuf),
    Modify(PathBuf),
    Delete(PathBuf),
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// A watched directory disappeared or was moved; its watch must be re-established
    /// and its subtree reconciled.
    Rescan(PathBuf),
    /// inotify could not watch this directory (typically ENOSPC / max_user_watches).
    WatchFailed(PathBuf),
    /// Event queue overflowed: the event stream is no longer reliable.
    Overflow,
}

pub struct Monitor {
    fd: RawFd,
    rx: Receiver<FsEvent>,
    wd_paths: Arc<RwLock<HashMap<i32, PathBuf>>>,
    limit: usize,
    limit_warned: AtomicBool,
}

impl Monitor {
    pub fn new(limit: usize) -> Result<Self> {
        let limit = limit.max(1);
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        let (tx, rx): (Sender<FsEvent>, Receiver<FsEvent>) = crossbeam_channel::unbounded();
        let wd_paths = Arc::new(RwLock::new(HashMap::new()));
        let wd_paths_t = wd_paths.clone();

        std::thread::Builder::new()
            .name("inotify-reader".into())
            .spawn(move || reader_loop(fd, tx, wd_paths_t))
            .map_err(|e| Error::Other(e.into()))?;

        Ok(Self {
            fd,
            rx,
            wd_paths,
            limit,
            limit_warned: AtomicBool::new(false),
        })
    }

    pub fn add_watch(&self, path: &Path) -> Result<()> {
        if self.wd_paths.read().len() >= self.limit {
            if !self.limit_warned.swap(true, Ordering::SeqCst) {
                warn!(
                    "inotify watch limit reached ({}); new directories will not be watched",
                    self.limit
                );
            }
            return Ok(());
        }
        let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::Ipc("path contains NUL".into()))?;
        let wd = unsafe { libc::inotify_add_watch(self.fd, cpath.as_ptr(), WATCH_MASK) };
        if wd < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        self.wd_paths.write().insert(wd, path.to_path_buf());
        debug!("watch {wd} {}", path.display());
        Ok(())
    }

    /// Add watches for a directory and all of its subdirectories.
    pub fn add_watch_recursive(&self, root: &Path) -> usize {
        let mut count = 0;
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if self.add_watch(&dir).is_ok() {
                count += 1;
            }
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    if let Ok(md) = entry.file_type() {
                        if md.is_dir() {
                            stack.push(entry.path());
                        }
                    }
                }
            }
        }
        count
    }

    pub fn try_recv(&self) -> Option<FsEvent> {
        self.rx.try_recv().ok()
    }

    pub fn watch_count(&self) -> usize {
        self.wd_paths.read().len()
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn watched_paths(&self) -> Vec<PathBuf> {
        self.wd_paths.read().values().cloned().collect()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

fn reader_loop(fd: RawFd, tx: Sender<FsEvent>, wd_paths: Arc<RwLock<HashMap<i32, PathBuf>>>) {
    let mut buf = vec![0u8; 65536];
    let mut cookies: HashMap<u32, (PathBuf, bool)> = HashMap::new();

    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            warn!("inotify read failed: {err}");
            return;
        }
        let n = n as usize;
        let mut pos = 0;
        while pos + 16 <= n {
            let wd = i32::from_ne_bytes(buf[pos..pos + 4].try_into().unwrap());
            let mask = u32::from_ne_bytes(buf[pos + 4..pos + 8].try_into().unwrap());
            let cookie = u32::from_ne_bytes(buf[pos + 8..pos + 12].try_into().unwrap());
            let len = u32::from_ne_bytes(buf[pos + 12..pos + 16].try_into().unwrap()) as usize;
            pos += 16;
            if pos + len > n {
                break;
            }
            let name_bytes = &buf[pos..pos + len];
            let name_end = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());
            let name = OsString::from_vec(name_bytes[..name_end].to_vec());
            pos += len;
            let is_dir = mask & libc::IN_ISDIR != 0;

            if mask & libc::IN_Q_OVERFLOW != 0 {
                cookies.clear();
                let _ = tx.send(FsEvent::Overflow);
                continue;
            }
            if mask & libc::IN_UNMOUNT != 0 {
                let _ = tx.send(FsEvent::Overflow);
                continue;
            }

            let parent = match wd_paths.read().get(&wd) {
                Some(p) => p.clone(),
                None => continue,
            };
            let full = parent.join(&name);

            if mask & libc::IN_MOVED_FROM != 0 {
                cookies.insert(cookie, (full, is_dir));
                continue;
            }
            if mask & libc::IN_MOVED_TO != 0 {
                match cookies.remove(&cookie) {
                    Some((from, _)) => {
                        let _ = tx.send(FsEvent::Rename { from, to: full });
                    }
                    None => {
                        let ev = if is_dir {
                            FsEvent::CreateDir(full)
                        } else {
                            FsEvent::Create(full)
                        };
                        let _ = tx.send(ev);
                    }
                }
                continue;
            }
            if mask & (libc::IN_DELETE | libc::IN_DELETE_SELF) != 0 {
                let _ = tx.send(FsEvent::Delete(full));
                if mask & libc::IN_DELETE_SELF != 0 {
                    wd_paths.write().remove(&wd);
                }
                continue;
            }
            if mask & libc::IN_IGNORED != 0 {
                wd_paths.write().remove(&wd);
                let _ = tx.send(FsEvent::Rescan(parent.clone()));
                continue;
            }
            if mask & libc::IN_MOVE_SELF != 0 {
                let _ = tx.send(FsEvent::Rescan(parent.clone()));
                continue;
            }
            if mask & libc::IN_CREATE != 0 {
                let ev = if is_dir {
                    FsEvent::CreateDir(full)
                } else {
                    FsEvent::Create(full)
                };
                let _ = tx.send(ev);
                continue;
            }
            if mask & (libc::IN_MODIFY | libc::IN_CLOSE_WRITE | libc::IN_ATTRIB) != 0 {
                let _ = tx.send(FsEvent::Modify(full));
            }
        }
    }
}

pub fn slot_for_path(store: &crate::store::Store, path: &Path) -> Option<u32> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for root in &store.roots {
        if let Ok(rel) = path.strip_prefix(&root.path) {
            let mut slot = root.slot;
            for comp in rel.components() {
                let name = comp.as_os_str().as_encoded_bytes();
                slot = store.find_child(slot, name)?;
            }
            return Some(slot);
        }
    }
    None
}
