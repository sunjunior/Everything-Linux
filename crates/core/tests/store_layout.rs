use std::path::PathBuf;

use everything_core::model::{EntryMeta, EntryType, F_HIDDEN, NULL_SLOT, RootInfo};
use everything_core::store::{Entry, Store};

fn root_store() -> (Store, u32) {
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
    s.roots.push(RootInfo {
        path: PathBuf::from("/test/root"),
        slot: root,
    });
    (s, root)
}

fn add(s: &mut Store, parent: u32, name: &str, kind: EntryType, size: u64) -> u32 {
    s.add_entry(
        parent,
        name.as_bytes(),
        EntryMeta {
            kind,
            size,
            ..Default::default()
        },
    )
}

fn ordered_names(s: &Store) -> Vec<String> {
    s.name_order()
        .iter()
        .map(|&slot| String::from_utf8_lossy(s.name(slot)).into_owned())
        .collect()
}

#[test]
fn entry_is_compact() {
    assert_eq!(std::mem::size_of::<Entry>(), 48);
}

#[test]
fn names_are_ordered_case_insensitively_with_slot_tiebreak() {
    let (mut s, root) = root_store();
    add(&mut s, root, "beta.txt", EntryType::File, 1);
    add(&mut s, root, "Alpha.txt", EntryType::File, 2);
    add(&mut s, root, "alpha.txt", EntryType::File, 3);
    assert_eq!(
        ordered_names(&s),
        vec!["Alpha.txt", "alpha.txt", "beta.txt", "root"]
    );
}

#[test]
fn folded_name_is_shared_when_already_lowercase() {
    let (mut s, root) = root_store();
    let lower = add(&mut s, root, "lowercase.txt", EntryType::File, 1);
    let upper = add(&mut s, root, "UpperCase.txt", EntryType::File, 1);
    assert_eq!(s.folded(lower), b"lowercase.txt");
    assert_eq!(s.folded(upper), b"uppercase.txt");
    assert!(s.fold_pool.len() < s.name_pool.len());
}

#[test]
fn find_child_matches_exact_name_bytes() {
    let (mut s, root) = root_store();
    let a = add(&mut s, root, "same.txt", EntryType::File, 1);
    let b = add(&mut s, root, "same.txt.bak", EntryType::File, 1);
    assert_eq!(s.find_child(root, b"same.txt"), Some(a));
    assert_eq!(s.find_child(root, b"same.txt.bak"), Some(b));
    assert_eq!(s.find_child(root, b"missing"), None);
}

#[test]
fn remove_unlinks_subtree_and_updates_counts() {
    let (mut s, root) = root_store();
    let dir = add(&mut s, root, "dir", EntryType::Dir, 0);
    let child = add(&mut s, dir, "child.txt", EntryType::File, 10);
    assert_eq!(s.find_child(root, b"dir"), Some(dir));
    assert_eq!(s.file_count(), 1);
    assert_eq!(s.dir_count(), 2);

    let removed = s.remove_subtree(dir);
    assert_eq!(removed.len(), 2);
    assert!(!s.is_live(dir));
    assert!(!s.is_live(child));
    assert_eq!(s.find_child(root, b"dir"), None);
    assert_eq!(s.file_count(), 0);
    assert_eq!(s.dir_count(), 1);
}

#[test]
fn insert_order_is_computed_on_demand() {
    let (mut s, root) = root_store();
    for name in ["c", "a", "b"] {
        add(&mut s, root, name, EntryType::File, 1);
    }
    assert_eq!(ordered_names(&s), vec!["a", "b", "c", "root"]);
}

#[test]
fn paths_are_reconstructed_from_parent_links() {
    let (mut s, root) = root_store();
    let dir = add(&mut s, root, "src", EntryType::Dir, 0);
    let file = add(&mut s, dir, "main.rs", EntryType::File, 42);
    let mut buf = Vec::new();
    s.path(file, &mut buf);
    assert_eq!(String::from_utf8_lossy(&buf), "/test/root/src/main.rs");
    assert_eq!(s.path_of(root).unwrap(), PathBuf::from("/test/root"));
}

#[test]
fn metadata_update_does_not_change_order() {
    let (mut s, root) = root_store();
    let slot = add(&mut s, root, "keep.txt", EntryType::File, 1);
    s.set_meta(
        slot,
        EntryMeta {
            kind: EntryType::File,
            size: 999,
            mtime: 1234,
            ..Default::default()
        },
    );
    assert_eq!(s.entry(slot).unwrap().size, 999);
    assert_eq!(s.entry(slot).unwrap().mtime, 1234);
    assert_eq!(ordered_names(&s), vec!["keep.txt", "root"]);
}
