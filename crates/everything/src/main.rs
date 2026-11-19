use std::path::{Path, PathBuf};

use everything_core::config::Config;
use everything_core::ipc::{self, Request, WireOptions};

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

    let (header, lines) = match ipc::client_call(&config.socket_path, &req) {
        Ok(v) => v,
        Err(e) => {
            if std::path::Path::new(&config.socket_path).exists() {
                return Err(e.into());
            }
            let exe = std::env::current_exe()?;
            let root = exe.parent().map(|p| p.to_path_buf());
            ipc::ensure_daemon(&config, root.as_deref(), None)?;
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
    println!("  -h, --help             show this help\n");
    println!("Query syntax:");
    println!("  space = AND, | = OR, ! = NOT, parentheses supported");
    println!("  prefixes: startwith: endwith: exact: regex: path: file: folder: case: wholeword:");
    println!("  wildcards: * and ?");
}
