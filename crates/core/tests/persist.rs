use std::path::PathBuf;

use everything_core::model::{EntryMeta, EntryType, F_HIDDEN, NULL_SLOT};
use everything_core::persist;
use everything_core::store::Store;

fn temp_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let unique = format!(
        "everything-test-{}-{}-{}.eldb",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    p.push(unique);
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
            mtime: 100,
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
fn v3_roundtrip_preserves_entries_order_and_metadata() {
    let store = sample_store();
    let path = temp_path("roundtrip");
    persist::save(&store, &path, 7).unwrap();
    let (loaded, generation) = persist::load(&path).unwrap().unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(generation, 7);
    assert_eq!(loaded.file_count(), 2);
    assert_eq!(loaded.dir_count(), 2);
    assert_eq!(loaded.roots[0].path, PathBuf::from("/test/root"));

    let order: Vec<String> = loaded
        .name_order()
        .iter()
        .map(|&s| String::from_utf8_lossy(loaded.name(s)).into_owned())
        .collect();
    assert_eq!(order, vec!["Alpha.txt", "main.rs", "root", "src"]);

    let alpha = loaded
        .find_child(loaded.roots[0].slot, b"Alpha.txt")
        .unwrap();
    assert_eq!(loaded.folded(alpha), b"alpha.txt");
    assert_eq!(loaded.entry(alpha).unwrap().size, 123);
    assert_eq!(loaded.entry(alpha).unwrap().mtime, 456);
    assert_eq!(loaded.entry(alpha).unwrap().created, 789);
    assert_eq!(loaded.entry(alpha).unwrap().accessed, 1011);

    let src = loaded.find_child(loaded.roots[0].slot, b"src").unwrap();
    let main = loaded.find_child(src, b"main.rs").unwrap();
    assert_eq!(
        loaded.path_of(main).unwrap(),
        PathBuf::from("/test/root/src/main.rs")
    );
}

#[test]
fn legacy_v2_snapshot_still_loads() {
    let mut pool = Vec::new();
    pool.extend_from_slice(b"root\0old.txt\0");

    let mut raw = Vec::new();
    raw.extend_from_slice(b"ELDB");
    raw.extend_from_slice(&2u32.to_le_bytes());
    raw.extend_from_slice(&3u64.to_le_bytes());
    raw.extend_from_slice(&1u32.to_le_bytes());
    raw.extend_from_slice(&(10u16).to_le_bytes());
    raw.extend_from_slice(b"/test/root");
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&(pool.len() as u32).to_le_bytes());
    raw.extend_from_slice(&pool);
    raw.extend_from_slice(&2u32.to_le_bytes());

    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&u32::MAX.to_le_bytes());
    raw.push(EntryType::Dir as u8);
    raw.push(F_HIDDEN);
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());

    raw.extend_from_slice(&5u32.to_le_bytes());
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.push(EntryType::File as u8);
    raw.push(0);
    raw.extend_from_slice(&11u64.to_le_bytes());
    raw.extend_from_slice(&22u64.to_le_bytes());
    raw.extend_from_slice(&123u64.to_le_bytes());
    raw.extend_from_slice(&456u64.to_le_bytes());
    raw.extend_from_slice(&789u64.to_le_bytes());
    raw.extend_from_slice(&1011u64.to_le_bytes());

    let compressed = zstd::encode_all(raw.as_slice(), 1).unwrap();
    let path = temp_path("legacy");
    std::fs::write(&path, compressed).unwrap();
    let (loaded, generation) = persist::load(&path).unwrap().unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(generation, 3);
    assert_eq!(loaded.file_count(), 1);
    assert_eq!(loaded.dir_count(), 1);
    let file = loaded.find_child(loaded.roots[0].slot, b"old.txt").unwrap();
    assert_eq!(loaded.name(file), b"old.txt");
    assert_eq!(loaded.entry(file).unwrap().size, 123);
    assert_eq!(loaded.entry(file).unwrap().mtime, 456);
    assert_eq!(loaded.entry(file).unwrap().created, 789);
    assert_eq!(loaded.entry(file).unwrap().accessed, 1011);
    assert_eq!(
        loaded.path_of(file).unwrap(),
        PathBuf::from("/test/root/old.txt")
    );
}

#[test]
fn corrupt_v3_offsets_are_rejected() {
    let mut raw = Vec::new();
    raw.extend_from_slice(b"ELDB");
    raw.extend_from_slice(&3u32.to_le_bytes());
    raw.extend_from_slice(&1u64.to_le_bytes());
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&1u32.to_le_bytes());
    raw.extend_from_slice(&[0u8]);
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&1u32.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.push(1);
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u64.to_le_bytes());
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&0u32.to_le_bytes());
    raw.extend_from_slice(&5u32.to_le_bytes());
    raw.extend_from_slice(&u32::MAX.to_le_bytes());
    raw.extend_from_slice(&u32::MAX.to_le_bytes());
    raw.extend_from_slice(&1u16.to_le_bytes());
    raw.push(EntryType::File as u8);
    raw.push(0);

    let compressed = zstd::encode_all(raw.as_slice(), 1).unwrap();
    let path = temp_path("corrupt");
    std::fs::write(&path, compressed).unwrap();
    let result = persist::load(&path);
    let _ = std::fs::remove_file(&path);
    assert!(result.is_err());
}
