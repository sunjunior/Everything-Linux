use std::path::PathBuf;

use everything_core::index::{build_snapshot, IndexSnapshot};
use everything_core::model::{EntryMeta, EntryType, F_HIDDEN, NULL_SLOT, SlotId};

fn store_with(n: usize) -> everything_core::store::Store {
    let mut s = everything_core::store::Store::new();
    let root_name = s.intern_name(b"root");
    let root = s.add_entry(
        NULL_SLOT,
        root_name,
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
    for i in 0..n {
        let name = format!("file_{i:04}.txt");
        let off = s.intern_name(name.as_bytes());
        s.add_entry(
            root,
            off,
            EntryMeta {
                kind: EntryType::File,
                size: i as u64,
                ..Default::default()
            },
        );
    }
    s
}

fn names_in_order(snap: &IndexSnapshot) -> Vec<String> {
    snap.by_name
        .iter()
        .map(|&slot| String::from_utf8_lossy(snap.name(slot)).into_owned())
        .collect()
}

#[test]
fn incremental_insert_matches_full_rebuild() {
    let mut s = store_with(20);
    let before = build_snapshot(&s, 1);

    let off = s.intern_name(b"aaa_new.txt");
    let slot = s.add_entry(s.roots[0].slot, off, EntryMeta {
        kind: EntryType::File,
        ..Default::default()
    });

    let incremental = before.apply_changes(&s, &[slot]);
    let full = build_snapshot(&s, 2);
    assert_eq!(names_in_order(&incremental), names_in_order(&full));
    assert_eq!(incremental.file_count, full.file_count);
}

#[test]
fn incremental_delete_matches_full_rebuild() {
    let mut s = store_with(20);
    let before = build_snapshot(&s, 1);
    let victim: SlotId = s.roots[0].slot + 5;
    s.remove_entry(victim);

    let incremental = before.apply_changes(&s, &[victim]);
    let full = build_snapshot(&s, 2);
    assert_eq!(names_in_order(&incremental), names_in_order(&full));
    assert_eq!(incremental.file_count, full.file_count);
}

#[test]
fn incremental_modify_and_rename_match_full_rebuild() {
    let mut s = store_with(20);
    let before = build_snapshot(&s, 1);
    let slot: SlotId = s.roots[0].slot + 7;

    let old_parent = s.parent(slot).unwrap();
    let old_name_off = s.name_offs[slot as usize];
    s.by_key.remove(&everything_core::model::EntryKey {
        parent: old_parent,
        name_off: old_name_off,
    });
    let new_off = s.intern_name(b"zzz_renamed.bin");
    s.name_offs[slot as usize] = new_off;
    s.by_key.insert(
        everything_core::model::EntryKey {
            parent: old_parent,
            name_off: new_off,
        },
        slot,
    );
    s.set_meta(slot, EntryMeta {
        kind: EntryType::File,
        size: 999,
        mtime: 1234,
        ..Default::default()
    });

    let incremental = before.apply_changes(&s, &[slot]);
    let full = build_snapshot(&s, 2);
    assert_eq!(names_in_order(&incremental), names_in_order(&full));
    assert_eq!(incremental.entry(slot).size, 999);
    assert_eq!(incremental.file_count, full.file_count);
}

#[test]
fn duplicate_names_are_still_sorted_correctly() {
    let mut s = store_with(5);
    let root = s.roots[0].slot;
    for _ in 0..5 {
        let off = s.intern_name(b"file_0001.txt");
        s.add_entry(root, off, EntryMeta {
            kind: EntryType::File,
            ..Default::default()
        });
    }
    let before = build_snapshot(&s, 1);
    let off = s.intern_name(b"file_0001.txt");
    let slot = s.add_entry(root, off, EntryMeta {
        kind: EntryType::File,
        ..Default::default()
    });
    let incremental = before.apply_changes(&s, &[slot]);
    let full = build_snapshot(&s, 2);
    assert_eq!(names_in_order(&incremental), names_in_order(&full));
}
