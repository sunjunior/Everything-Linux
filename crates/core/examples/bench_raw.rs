use std::path::Path;
use std::time::Instant;

use everything_core::model::QueryOptions;
use everything_core::query;
use everything_core::raw::MappedIndex;
use everything_core::view::IndexView;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: bench_raw <raw.eldr>"))?;
    let mapped = MappedIndex::open(Path::new(&path))?;
    let view = IndexView::Raw(&mapped);
    for q in ["*.rs", "main.rs", "zzzzz-nomatch", "path:everything-linux"] {
        let opts = QueryOptions {
            limit: 5000,
            ..Default::default()
        };
        let _ = query::run_view(&view, mapped.generation(), q, opts)?;
        let mut ts = Vec::new();
        for _ in 0..10 {
            let t = Instant::now();
            let _ = query::run_view(&view, mapped.generation(), q, opts)?;
            ts.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("{q}\tmedian {:.2} ms", ts[5]);
    }
    Ok(())
}
