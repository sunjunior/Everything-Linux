use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WireOptions {
    #[serde(default)]
    pub case: bool,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub whole_word: bool,
    #[serde(default)]
    pub path: bool,
    #[serde(default)]
    pub diacritics: bool,
    #[serde(default)]
    pub file_only: bool,
    #[serde(default)]
    pub folder_only: bool,
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub cmd: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub options: WireOptions,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Header {
    pub ok: bool,
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub elapsed_ms: f64,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub message: String,
}

impl Header {
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            count: 0,
            total: 0,
            elapsed_ms: 0.0,
            generation: 0,
            truncated: false,
            message: msg.into(),
        }
    }
}

pub fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    UnixListener::bind(path).map_err(|e| Error::Ipc(format!("bind {}: {e}", path.display())))
}

pub fn read_request(stream: &mut UnixStream) -> Result<Option<Request>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    let req: Request = serde_json::from_str(line.trim())
        .map_err(|e| Error::Ipc(format!("bad request: {e}")))?;
    Ok(Some(req))
}

pub fn write_response(stream: &mut UnixStream, header: &Header, lines: &[String]) -> Result<()> {
    let mut w = BufWriter::new(stream);
    let h = serde_json::to_string(header)
        .map_err(|e| Error::Ipc(format!("serialize header: {e}")))?;
    writeln!(w, "{h}")?;
    for line in lines {
        writeln!(w, "{line}")?;
    }
    w.flush()?;
    Ok(())
}

pub fn client_call(path: &Path, req: &Request) -> Result<(Header, Vec<String>)> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| Error::Ipc(format!("connect {}: {e}. is everythingd running?", path.display())))?;
    let payload = serde_json::to_string(req)
        .map_err(|e| Error::Ipc(format!("serialize request: {e}")))?;
    stream.write_all(payload.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(Error::Ipc("no response from daemon".into()));
    }
    let header: Header = serde_json::from_str(line.trim())
        .map_err(|e| Error::Ipc(format!("bad response header: {e}")))?;
    let mut lines = Vec::with_capacity(header.count);
    for _ in 0..header.count {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        lines.push(line.trim_end_matches('\n').to_string());
    }
    Ok((header, lines))
}

/// Ensure the daemon is running; spawn a sibling `everythingd` if needed.
fn find_daemon(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let name = "everythingd";
    let mut candidates = vec![exe.parent().map(|d| d.join(name)).unwrap_or_default()];
    if let Ok(dir) = std::env::current_dir() {
        candidates.push(dir.join(name));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(name));
        candidates.push(home.join(".local/bin").join(name));
    }
    candidates.push(std::path::PathBuf::from("/usr/local/bin").join(name));
    candidates.push(std::path::PathBuf::from("/usr/bin").join(name));
    candidates.into_iter().find(|p| p.is_file())
}

/// Ensure the daemon is running. `preferred_root` (typically the directory of
/// the calling GUI/CLI) is passed as --root when starting a fresh daemon.
fn extract_embedded_daemon(bytes: &[u8]) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if bytes.is_empty() {
        return Err(Error::Ipc("embedded daemon is empty".into()));
    }
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let dir = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("everything-linux");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("everythingd-{h:016x}"));
    if !path.exists() {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

pub fn ensure_daemon(
    config: &crate::config::Config,
    preferred_root: Option<&std::path::Path>,
    embedded_daemon: Option<&[u8]>,
) -> Result<()> {
    if config.socket_path.exists() {
        if client_call(&config.socket_path, &Request {
            cmd: "status".into(),
            text: String::new(),
            options: WireOptions::default(),
        }).is_ok() {
            return Ok(());
        }
        let _ = std::fs::remove_file(&config.socket_path);
    }
    let exe = std::env::current_exe().map_err(|e| Error::Ipc(e.to_string()))?;
    let daemon = find_daemon(&exe).or_else(|| {
        embedded_daemon
            .and_then(|bytes| extract_embedded_daemon(bytes).ok())
    }).ok_or_else(|| {
        Error::Ipc("找不到 everythingd，且当前可执行文件未内嵌守护进程".into())
    })?;
    let mut cmd = std::process::Command::new(daemon);
    cmd.arg("--quiet");
    if let Some(root) = preferred_root {
        cmd.arg("--root").arg(root);
    }
    for r in &config.roots {
        cmd.arg("--root").arg(r);
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map_err(|e| Error::Ipc(e.to_string()))?;
    for _ in 0..300 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if config.socket_path.exists() {
            return Ok(());
        }
    }
    Err(Error::Ipc("everythingd 启动超时（30 秒），请检查其是否能访问索引目录".into()))
}

