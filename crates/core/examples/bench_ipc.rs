use std::time::Instant;

use everything_core::config::Config;
use everything_core::ipc::{Request, WireOptions, client_call};

fn main() -> anyhow::Result<()> {
    let config = Config::load()?;
    let socket = config.socket_path.clone();
    let cases = [
        ("*.rs", ""),
        ("main.rs", ""),
        ("zzzzz-nomatch", ""),
        ("path:everything-linux", ""),
        ("*.txt", "-size"),
    ];
    for (q, sort) in cases {
        let make = || Request {
            cmd: "query".into(),
            text: q.to_string(),
            options: WireOptions {
                limit: 5000,
                sort: sort.to_string(),
                ..Default::default()
            },
        };
        let _ = client_call(&socket, &make())?;
        let mut times = Vec::new();
        let (mut count, mut total, mut engine) = (0, 0, 0.0);
        for _ in 0..10 {
            let t = Instant::now();
            let (h, lines) = client_call(&socket, &make())?;
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            count = lines.len();
            total = h.total;
            engine = h.elapsed_ms;
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "query={q}\tcount={count}\ttotal={total}\tengine_ms={engine:.2}\twall_ms={:.2}",
            times[times.len() / 2]
        );
    }
    Ok(())
}
