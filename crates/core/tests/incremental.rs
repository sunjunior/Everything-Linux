use std::path::PathBuf;

use everything_core::model::{EntryMeta, EntryType, F_HIDDEN, NULL_SLOT, SlotId};
use everything_core::store::Store;

fn store_with(n: usize) -> (Store, SlotId) {
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
    for i in 0..n {
        let name = format!("file_{i:04}.txt");
        s.add_entry(
            root,
            name.as_bytes(),
            EntryMeta {
                kind: EntryType::File,
                size: i as u64,
                ..Default::default()
            },
        );
    }
    (s, root)
}

fn names_in_order(s: &Store) -> Vec<String> {
    s.name_order()
        .iter()
        .map(|&slot| String::from_utf8_lossy(s.name(slot)).into_owned())
        .collect()
}

#[test]
fn insert_updates_order_and_counts() {
    let (mut s, root) = store_with(20);
    let before = names_in_order(&s);
    s.add_entry(
        root,
        b"aaa_new.txt",
        EntryMeta {
            kind: EntryType::File,
            ..Default::default()
        },
    );
    let after = names_in_order(&s);
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after[0], "aaa_new.txt");
    assert_eq!(s.file_count(), 21);
    assert_eq!(s.dir_count(), 1);
}

#[test]
fn delete_removes_from_order_and_counts() {
    let (mut s, _root) = store_with(20);
    let victim: SlotId = 5;
    assert!(s.remove_entry(victim));
    assert!(!s.is_live(victim));
    assert_eq!(s.file_count(), 19);
    assert!(!names_in_order(&s).contains(&"file_0004.txt".to_string()));
}

#[test]
fn rename_is_remove_plus_insert() {
    let (mut s, root) = store_with(20);
    let slot: SlotId = 7;
    let old_name = String::from_utf8_lossy(s.name(slot)).into_owned();
    s.remove_entry(slot);
    let new_slot = s.add_entry(
        root,
        b"zzz_renamed.bin",
        EntryMeta {
            kind: EntryType::File,
            size: 999,
            mtime: 1234,
            ..Default::default()
        },
    );
    assert_eq!(s.entry(new_slot).unwrap().size, 999);
    assert_eq!(s.entry(new_slot).unwrap().mtime, 1234);
    let order = names_in_order(&s);
    assert!(!order.contains(&old_name));
    assert_eq!(order.last().unwrap(), "zzz_renamed.bin");
    assert_eq!(s.file_count(), 20);
}

#[test]
fn duplicate_names_are_sorted_by_slot() {
    let (mut s, root) = store_with(5);
    let mut slots = Vec::new();
    for _ in 0..5 {
        slots.push(s.add_entry(
            root,
            b"file_0001.txt",
            EntryMeta {
                kind: EntryType::File,
                ..Default::default()
            },
        ));
    }
    let positions: Vec<usize> = slots
        .iter()
        .map(|&slot| s.name_order().iter().position(|&x| x == slot).unwrap())
        .collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn many_inserts_still_sort() {
    let (mut s, root) = store_with(0);
    for i in (0..50).rev() {
        s.add_entry(
            root,
            format!("bulk_{i:03}").as_bytes(),
            EntryMeta {
                kind: EntryType::File,
                ..Default::default()
            },
        );
    }
    let order = names_in_order(&s);
    assert_eq!(order.len(), 51);
    assert!(order.windows(2).all(|w| w[0] <= w[1]));
}
