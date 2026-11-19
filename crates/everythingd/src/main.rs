use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use everything_core::config::Config;
use everything_core::engine::Engine;
use everything_core::ipc::{self, Header, Request};

fn main() -> anyhow::Result<()> {
    let mut config = Config::load()?;
    let mut no_snapshot = false;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut quiet = false;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-snapshot" => no_snapshot = true,
            "--no-watch" => config.watch_enabled = false,
            "--quiet" | "-q" => quiet = true,
            "--root" | "-r" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--root requires a path");
                }
                roots.push(PathBuf::from(&args[i]));
            }
            "--threads" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--threads requires a number");
                }
                config.threads = args[i].parse()?;
            }
            "--socket" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--socket requires a path");
                }
                config.socket_path = PathBuf::from(&args[i]);
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => anyhow::bail!("unknown option: {other}"),
        }
        i += 1;
    }
    if !roots.is_empty() {
        for r in roots {
            if !config.roots.contains(&r) {
                config.roots.push(r);
            }
        }
        // Explicit --root wins over exe-location defaults.
        config.mark_manual_roots();
    }
    let roots_changed = config.apply_exe_root();
    let used_default = roots_changed;

    if !quiet {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_target(false)
            .init();
    }
    if used_default {
        tracing::info!(
            "default root resolved from executable location: {}",
            config.roots[0].display()
        );
    }

    // A moved executable invalidates auto-resolved roots and the old snapshot.
    let mut engine = Engine::open(config, !no_snapshot && !roots_changed)?;
    tracing::info!(
        "index loaded: {} files, {} dirs (generation {})",
        engine.stats().0,
        engine.stats().1,
        engine.stats().2
    );

    let socket_path = engine.config.socket_path.clone();
    let listener = ipc::bind(&socket_path)?;
    listener.set_nonblocking(true)?;
    tracing::info!("listening on {}", socket_path.display());

    let engine = Arc::new(Mutex::new(engine));
    let running = Arc::new(AtomicBool::new(true));
    let running_t = running.clone();

    // Bind IPC first; scan in the background so clients can connect immediately.
    let engine_for_init = engine.clone();
    std::thread::Builder::new()
        .name("initial-scan".into())
        .spawn(move || {
            let mut eng = match engine_for_init.lock() {
                Ok(g) => g,
                Err(e) => {
                    tracing::error!("init lock failed: {e}");
                    return;
                }
            };
            if let Err(e) = eng.scan_configured_roots() {
                tracing::error!("initial scan failed: {e}");
                return;
            }
            if let Err(e) = eng.start_monitor() {
                tracing::warn!("start monitor failed: {e}");
            }
            tracing::info!(
                "index ready: {} files, {} dirs (generation {}), watches {}",
                eng.stats().0, eng.stats().1, eng.stats().2, eng.watch_count()
            );
            let _ = eng.save();
            let _ = eng.config.save();
        })
        .expect("spawn initial scan");

    let engine_for_server = engine.clone();
    let server = std::thread::spawn(move || {
        for stream in listener.incoming() {
            if !running_t.load(Ordering::SeqCst) {
                break;
            }
            let stream = match stream {
                Ok(s) => s,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    continue;
                }
            };
            let engine = engine_for_server.clone();
            let running = running_t.clone();
            std::thread::spawn(move || handle_client(stream, engine, running));
        }
    });

    while running.load(Ordering::SeqCst) {
        if let Ok(mut eng) = engine.try_lock() {
            if let Err(e) = eng.tick() {
                tracing::warn!("tick failed: {e}");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = server.join();
    let _ = std::fs::remove_file(Config::load()?.socket_path);
    Ok(())
}

fn handle_client(
    mut stream: std::os::unix::net::UnixStream,
    engine: Arc<Mutex<Engine>>,
    running: Arc<AtomicBool>,
) {
    let req = match ipc::read_request(&mut stream) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            let _ = ipc::write_response(&mut stream, &Header::error(e.to_string()), &[]);
            return;
        }
    };

    let mut out_lines: Vec<String> = Vec::new();
    let header = {
        let mut eng = match engine.lock() {
            Ok(v) => v,
            Err(e) => {
                let _ = ipc::write_response(&mut stream, &Header::error(e.to_string()), &[]);
                return;
            }
        };
        dispatch(&mut eng, &req, &running, &mut out_lines)
    };
    if let Err(e) = ipc::write_response(&mut stream, &header, &out_lines) {
        tracing::warn!("write response failed: {e}");
    }
}

fn dispatch(
    engine: &mut Engine,
    req: &Request,
    running: &AtomicBool,
    out: &mut Vec<String>,
) -> Header {
    match req.cmd.as_str() {
        "query" => {
            let opts = crate_options(&req);
            match engine.query(&req.text, opts) {
                Ok(res) => {
                    let snap = engine.snapshot();
                    let mut buf = Vec::new();
                    for slot in res.slots {
                        snap.path(slot, &mut buf);
                        out.push(String::from_utf8_lossy(&buf).into_owned());
                    }
                    Header {
                        ok: true,
                        count: out.len(),
                        total: res.total_matched,
                        elapsed_ms: res.elapsed_ms,
                        generation: res.generation,
                        truncated: res.truncated,
                        message: String::new(),
                    }
                }
                Err(e) => Header::error(e.to_string()),
            }
        }
        "status" => {
            let (files, dirs, generation) = engine.stats();
            out.push(format!("files={files}"));
            out.push(format!("dirs={dirs}"));
            out.push(format!("generation={generation}"));
            out.push(format!("watches={}", engine.watch_count()));
            let (full, incr) = engine.update_counters();
            out.push(format!("full_rebuilds={full}"));
            out.push(format!("incremental_updates={incr}"));
            for root in &engine.config.roots {
                out.push(format!("root={}", root.display()));
            }
            Header {
                ok: true,
                count: out.len(),
                ..Header::default()
            }
        }
        "add" => match engine.add_root(std::path::Path::new(&req.text)) {
            Ok(()) => {
                engine.config.mark_manual_roots();
                engine.rebuild();
                let _ = engine.save();
                let _ = engine.config.save();
                Header { ok: true, ..Header::default() }
            }
            Err(e) => Header::error(e.to_string()),
        },
        "remove" => match engine.remove_root(std::path::Path::new(&req.text)) {
            Ok(()) => {
                engine.config.mark_manual_roots();
                engine.rebuild();
                let _ = engine.save();
                let _ = engine.config.save();
                Header { ok: true, ..Header::default() }
            }
            Err(e) => Header::error(e.to_string()),
        },
        "reindex" => match engine.reindex() {
            Ok(()) => {
                let _ = engine.config.save();
                Header { ok: true, ..Header::default() }
            }
            Err(e) => Header::error(e.to_string()),
        },
        "save" => match engine.save() {
            Ok(()) => Header { ok: true, ..Header::default() },
            Err(e) => Header::error(e.to_string()),
        },
        "shutdown" => {
            running.store(false, Ordering::SeqCst);
            Header { ok: true, ..Header::default() }
        }
        other => Header::error(format!("unknown command: {other}")),
    }
}

fn crate_options(req: &Request) -> everything_core::model::QueryOptions {
    let o = &req.options;
    everything_core::model::QueryOptions {
        case_sensitive: o.case,
        regex: o.regex,
        whole_word: o.whole_word,
        path: o.path,
        diacritics: o.diacritics,
        file_only: o.file_only,
        folder_only: o.folder_only,
        sort: everything_core::model::SortBy::parse(&o.sort).unwrap_or_default(),
        limit: if o.limit == 0 { 100_000 } else { o.limit },
    }
}


fn print_help() {
    println!("everythingd - Everything-like index daemon for Linux\n");
    println!("Usage: everythingd [OPTIONS]\n");
    println!("Options:");
    println!("  -r, --root <PATH>     directory to index (repeatable; default: working directory)");
    println!("      --no-snapshot     ignore saved snapshot, rebuild from scratch");
    println!("      --no-watch        disable inotify monitoring");
    println!("      --threads <N>     number of indexing threads");
    println!("      --socket <PATH>   IPC socket path");
    println!("  -q, --quiet           no log output");
    println!("  -h, --help            show this help");
}
