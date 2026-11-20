use std::time::Instant;

use gtk::prelude::*;

fn bench_strings(n: usize) -> f64 {
    let store = gtk::ListStore::new(&[String::static_type()]);
    let t = Instant::now();
    for i in 0..n {
        let path = format!("/home/user/projects/some/deep/path/file_{i:05}.rs");
        store.set(&store.append(), &[(0, &path)]);
    }
    t.elapsed().as_secs_f64() * 1000.0
}

fn bench_slots(n: usize) -> f64 {
    let store = gtk::ListStore::new(&[u32::static_type()]);
    let t = Instant::now();
    for i in 0..n {
        let slot = i as u32;
        store.set(&store.append(), &[(0, &slot)]);
    }
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() -> anyhow::Result<()> {
    gtk::init()?;
    let n = 5000;
    for round in 0..3 {
        println!(
            "round {round}: strings={:.2} ms  slots={:.2} ms",
            bench_strings(n),
            bench_slots(n)
        );
    }
    Ok(())
}
