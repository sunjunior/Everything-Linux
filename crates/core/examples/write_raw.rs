use std::path::Path;

use everything_core::config::Config;
use everything_core::{persist, raw};

fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: write_raw <out.eldr>"))?;
    let config = Config::load()?;
    let (store, generation) = persist::load(&config.snapshot_path)?
        .ok_or_else(|| anyhow::anyhow!("no snapshot at {}", config.snapshot_path.display()))?;
    raw::write_raw(&store, Path::new(&out), generation)?;
    println!(
        "wrote {} slots, generation {}, to {}",
        store.len(),
        generation,
        out
    );
    Ok(())
}
