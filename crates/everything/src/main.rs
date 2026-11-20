use std::path::{Path, PathBuf};

use everything_core::config::Config;
use everything_core::ipc::{self, Request, WireOptions};
use everything_core::model::{QueryOptions, SortBy};
use everything_core::query;
use everything_core::raw::MappedIndex;
use everything_core::sdk::LocalEngine;
use everything_core::view::IndexView;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    let config = Config::load()?;
    let mut opts = WireOptions::default();
    opts.limit = config.max_results;
    let mut text_parts: Vec<String> = Vec::new();
    let mut cmd: Option<String> = None;
    let mut cmd_arg: Option<String> = None;
    let mut json = false;
    let mut local = false;
    let mut raw_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-c" | "--case" => opts.case = true,
            "-i" | "--ignore-case" => opts.case = false,
            "-e" | "--regex" => opts.regex = true,
            "-w" | "--whole-word" => opts.whole_word = true,
            "--path" => opts.path = true,
            "--json" => json = true,
            "--local" => local = true,
            "--raw" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--raw requires a path");
                }
                raw_path = Some(PathBuf::from(&args[i]));
            }
            "--file" => opts.file_only = true,
            "--folder" => opts.folder_only = true,
            "-s" | "--sort" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--sort requires a value");
                }
                opts.sort = args[i].clone();
            }
            "-l" | "--limit" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--limit requires a number");
                }
                opts.limit = args[i].parse()?;
            }
            "status" | "reindex" | "save" | "shutdown" => {
                cmd = Some(a.clone());
            }
            "index" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("index requires add|remove|list");
                }
                match args[i].as_str() {
                    "add" => cmd = Some("add".into()),
                    "remove" => cmd = Some("remove".into()),
                    "list" => cmd = Some("status".into()),
                    other => anyhow::bail!("unknown index subcommand: {other}"),
                }
            }
            _ if cmd_arg.is_none() && matches!(cmd.as_deref(), Some("add") | Some("remove")) => {
                cmd_arg = Some(a.clone());
            }
            _ => text_parts.push(a.clone()),
        }
        i += 1;
    }

    let cmd = cmd.unwrap_or_else(|| "query".into());
    let text = cmd_arg.unwrap_or_else(|| text_parts.join(" "));
    let req = Request {
        cmd: cmd.clone(),
        text,
        options: opts,
    };

    if local {
        return run_local(config, &req, json, raw_path.as_deref());
    }

    let (header, lines) = match ipc::client_call(&config.socket_path, &req) {
        Ok(v) => v,
        Err(e) => {
            if std::path::Path::new(&config.socket_path).exists() {
                return Err(e.into());
            }
            ipc::ensure_daemon(&config, None, None)?;
            ipc::client_call(&config.socket_path, &req)?
        }
    };

    if !header.ok {
        anyhow::bail!("daemon error: {}", header.message);
    }

    if json {
        let obj = serde_json::json!({
            "count": header.count,
            "total": header.total,
            "elapsed_ms": header.elapsed_ms,
            "generation": header.generation,
            "truncated": header.truncated,
            "results": lines,
        });
        println!("{obj}");
    } else {
        for line in &lines {
            println!("{line}");
        }
    }

    if cmd == "query" || cmd == "status" {
        eprintln!(
            "# {} results ({} total), {:.2} ms, generation {}",
            header.count, header.total, header.elapsed_ms, header.generation
        );
        if header.truncated {
            eprintln!("# truncated: use --limit 0 for all results");
        }
    }
    Ok(())
}

fn query_options(req: &Request, max_results: usize) -> QueryOptions {
    QueryOptions {
        case_sensitive: req.options.case,
        regex: req.options.regex,
        whole_word: req.options.whole_word,
        path: req.options.path,
        diacritics: req.options.diacritics,
        file_only: req.options.file_only,
        folder_only: req.options.folder_only,
        sort: SortBy::parse(&req.options.sort).unwrap_or_default(),
        limit: if req.options.limit == 0 {
            max_results
        } else {
            req.options.limit
        },
    }
}

fn run_local(
    mut config: Config,
    req: &Request,
    json: bool,
    raw_path: Option<&Path>,
) -> anyhow::Result<()> {
    let max_results = config.max_results;
    if let Some(path) = raw_path {
        return run_raw(req, json, max_results, path);
    }
    config.watch_enabled = false;
    let roots = config.roots.clone();
    let engine = LocalEngine::open(config, true)?;
    match req.cmd.as_str() {
        "query" => {
            let options = query_options(req, max_results);
            let hits = engine.search_paths(&req.text, options)?;
            let lines: Vec<String> = hits
                .paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if json {
                let obj = serde_json::json!({
                    "count": lines.len(),
                    "total": hits.result.total_matched,
                    "elapsed_ms": hits.result.elapsed_ms,
                    "generation": hits.result.generation,
                    "truncated": hits.result.truncated,
                    "results": lines,
                });
                println!("{obj}");
            } else {
                for line in &lines {
                    println!("{line}");
                }
            }
            eprintln!(
                "# {} results ({} total), {:.2} ms, generation {}",
                lines.len(),
                hits.result.total_matched,
                hits.result.elapsed_ms,
                hits.result.generation
            );
            if hits.result.truncated {
                eprintln!("# truncated: use --limit 0 for all results");
            }
        }
        "status" => {
            let (files, dirs, generation) = engine.status();
            let (full, incr) = engine.update_counters();
            let mut lines = vec![
                format!("files={files}"),
                format!("dirs={dirs}"),
                format!("generation={generation}"),
                format!("watches={}", engine.watch_count()),
                format!("full_rebuilds={full}"),
                format!("incremental_updates={incr}"),
            ];
            for root in &roots {
                lines.push(format!("root={}", root.display()));
            }
            for line in &lines {
                println!("{line}");
            }
            eprintln!(
                "# {} results (0 total), 0.00 ms, generation {}",
                lines.len(),
                generation
            );
        }
        "save" => engine.save()?,
        "reindex" => engine.reindex()?,
        "add" => engine.add_root(std::path::Path::new(&req.text))?,
        "remove" => engine.remove_root(std::path::Path::new(&req.text))?,
        "shutdown" => {}
        other => anyhow::bail!("local mode does not support command: {other}"),
    }
    Ok(())
}

fn run_raw(req: &Request, json: bool, max_results: usize, path: &Path) -> anyhow::Result<()> {
    let mapped = MappedIndex::open(path)?;
    let view = IndexView::Raw(&mapped);
    match req.cmd.as_str() {
        "query" => {
            let options = query_options(req, max_results);
            let result = query::run_view(&view, mapped.generation(), &req.text, options)?;
            let mut lines = Vec::with_capacity(result.slots.len());
            let mut buf = Vec::new();
            for &slot in &result.slots {
                mapped.path(slot, &mut buf);
                lines.push(String::from_utf8_lossy(&buf).into_owned());
            }
            if json {
                let obj = serde_json::json!({
                    "count": lines.len(),
                    "total": result.total_matched,
                    "elapsed_ms": result.elapsed_ms,
                    "generation": mapped.generation(),
                    "truncated": result.truncated,
                    "results": lines,
                });
                println!("{obj}");
            } else {
                for line in &lines {
                    println!("{line}");
                }
            }
            eprintln!(
                "# {} results ({} total), {:.2} ms, generation {}",
                lines.len(),
                result.total_matched,
                result.elapsed_ms,
                mapped.generation()
            );
            if result.truncated {
                eprintln!("# truncated: use --limit 0 for all results");
            }
        }
        "status" => {
            let mut lines = vec![
                format!("files={}", mapped.file_count()),
                format!("dirs={}", mapped.dir_count()),
                format!("generation={}", mapped.generation()),
                "watches=0".to_string(),
                "full_rebuilds=0".to_string(),
                "incremental_updates=0".to_string(),
            ];
            for root in mapped.roots() {
                lines.push(format!("root={}", root.path.display()));
            }
            for line in &lines {
                println!("{line}");
            }
            eprintln!(
                "# {} results (0 total), 0.00 ms, generation {}",
                lines.len(),
                mapped.generation()
            );
        }
        other => anyhow::bail!("raw snapshot mode does not support command: {other}"),
    }
    Ok(())
}

fn print_help() {
    println!("everything - Everything-like file search client for Linux\n");
    println!("Usage: everything [OPTIONS] [QUERY...]\n");
    println!("Commands:");
    println!("  status                 show index status");
    println!("  reindex                rebuild the index from configured roots");
    println!("  save                   save snapshot to disk");
    println!("  shutdown               stop the daemon");
    println!("  index add|list <path>  manage indexed directories\n");
    println!("Options:");
    println!("  -c, --case             case sensitive matching");
    println!("  -i, --ignore-case      case insensitive (default)");
    println!("  -e, --regex            treat query as regular expression");
    println!("  -w, --whole-word       match whole words");
    println!("      --path             match full paths");
    println!("      --file / --folder  restrict to files / folders");
    println!("  -s, --sort <FIELD>     name|path|size|mtime (prefix - for descending)");
    println!("  -l, --limit <N>        max results (0 = all)");
    println!("      --json             JSON output");
    println!("      --local            run the index in this process (no daemon/IPC)");
    println!("      --raw <PATH>       with --local: query a raw mmap snapshot");
    println!("  -h, --help             show this help\n");
    println!("Query syntax:");
    println!("  space = AND, | = OR, ! = NOT, parentheses supported");
    println!("  prefixes: startwith: endwith: exact: regex: path: file: folder: case: wholeword:");
    println!("  wildcards: * and ?");
}
