use std::path::PathBuf;

use everything_core::model::{EntryMeta, EntryType, F_HIDDEN, NULL_SLOT, QueryOptions};
use everything_core::query;
use everything_core::raw::{MappedIndex, write_raw};
use everything_core::store::Store;
use everything_core::view::IndexView;

fn temp_path() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "everything-raw-{}-{}.eldr",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

fn sample_store() -> Store {
    let mut s = Store::new();
    let root = s.add_entry(
        NULL_SLOT,
        b"root",
        EntryMeta {
            kind: EntryType::Dir,
            flags: F_HIDDEN,
            ..Default::default()
        },
    );
    s.roots.push(everything_core::model::RootInfo {
        path: PathBuf::from("/test/root"),
        slot: root,
    });
    s.add_entry(
        root,
        b"Alpha.txt",
        EntryMeta {
            kind: EntryType::File,
            size: 123,
            mtime: 456,
            created: 789,
            accessed: 1011,
            ..Default::default()
        },
    );
    let dir = s.add_entry(
        root,
        b"src",
        EntryMeta {
            kind: EntryType::Dir,
            ..Default::default()
        },
    );
    s.add_entry(
        dir,
        b"main.rs",
        EntryMeta {
            kind: EntryType::File,
            size: 42,
            mtime: 2,
            ..Default::default()
        },
    );
    s
}

#[test]
fn raw_snapshot_roundtrip_and_query() {
    let store = sample_store();
    let path = temp_path();
    write_raw(&store, &path, 7).unwrap();
    let mapped = MappedIndex::open(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(mapped.generation(), 7);
    assert_eq!(mapped.len(), store.len());
    assert_eq!(mapped.file_count(), 2);
    assert_eq!(mapped.dir_count(), 2);

    let alpha = (0..store.len() as u32)
        .find(|&s| store.name(s) == b"Alpha.txt")
        .unwrap();
    assert!(mapped.is_live(alpha));
    assert_eq!(mapped.name(alpha), b"Alpha.txt");
    assert_eq!(mapped.folded(alpha), b"alpha.txt");
    assert_eq!(mapped.entry(alpha).unwrap().size, 123);
    assert_eq!(mapped.entry(alpha).unwrap().mtime, 456);
    assert_eq!(mapped.entry(alpha).unwrap().created, 789);
    assert_eq!(mapped.entry(alpha).unwrap().accessed, 1011);

    let mut buf = Vec::new();
    mapped.path(alpha, &mut buf);
    assert_eq!(String::from_utf8_lossy(&buf), "/test/root/Alpha.txt");

    let view = IndexView::Raw(&mapped);
    let raw = query::run_view(&view, 7, "*.rs", QueryOptions::default()).unwrap();
    let full = query::run(&store, 7, "*.rs", QueryOptions::default()).unwrap();
    let names =
        |slots: &[u32], f: &dyn Fn(u32) -> Vec<u8>| slots.iter().map(|&s| f(s)).collect::<Vec<_>>();
    assert_eq!(
        names(&raw.slots, &|s| mapped.name(s).to_vec()),
        names(&full.slots, &|s| store.name(s).to_vec())
    );
}
