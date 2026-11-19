use std::path::PathBuf;

use everything_core::index::build_snapshot;
use everything_core::model::{EntryMeta, EntryType, F_HIDDEN, NULL_SLOT, QueryOptions};
use everything_core::query;
use everything_core::store::Store;

fn fixture() -> everything_core::index::IndexSnapshot {
    let mut s = Store::new();
    let root_path = PathBuf::from("/test/root");
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
        path: root_path,
        slot: root,
    });

    for (name, kind) in [
        ("alpha.txt", EntryType::File),
        ("alphabet.md", EntryType::File),
        ("beta.rs", EntryType::File),
        ("README.md", EntryType::File),
        ("src", EntryType::Dir),
    ] {
        let off = s.intern_name(name.as_bytes());
        s.add_entry(
            root,
            off,
            EntryMeta {
                kind,
                size: name.len() as u64,
                ..Default::default()
            },
        );
    }
    let src = s.lookup_name(root, b"src").unwrap();
    for name in ["nested_one.rs", "nested_two.rs", "deep.txt"] {
        let off = s.intern_name(name.as_bytes());
        s.add_entry(
            src,
            off,
            EntryMeta {
                kind: EntryType::File,
                size: name.len() as u64,
                ..Default::default()
            },
        );
    }
    build_snapshot(&s, 1)
}

fn names(snap: &everything_core::index::IndexSnapshot, q: &str, opts: QueryOptions) -> Vec<String> {
    let res = query::run(snap, q, opts).unwrap();
    res.slots
        .iter()
        .map(|&slot| String::from_utf8_lossy(snap.name(slot)).into_owned())
        .collect()
}

#[test]
fn substring_is_case_insensitive_by_default() {
    let snap = fixture();
    let got = names(&snap, "ALPHA", QueryOptions::default());
    assert_eq!(got, vec!["alpha.txt", "alphabet.md"]);
}

#[test]
fn case_sensitive_query() {
    let snap = fixture();
    let opts = QueryOptions {
        case_sensitive: true,
        ..Default::default()
    };
    assert_eq!(names(&snap, "ALPHA", opts), Vec::<String>::new());
}

#[test]
fn wildcard_and_regex() {
    let snap = fixture();
    assert_eq!(names(&snap, "*.rs", QueryOptions::default()).len(), 3);
    let opts = QueryOptions {
        regex: true,
        ..Default::default()
    };
    assert_eq!(names(&snap, "nested_[a-z]+", opts).len(), 2);
}

#[test]
fn boolean_or_and_not() {
    let snap = fixture();
    assert_eq!(names(&snap, "alpha.txt | beta.rs", QueryOptions::default()).len(), 2);
    let got = names(&snap, "*.md !README", QueryOptions::default());
    assert_eq!(got, vec!["alphabet.md"]);
}

#[test]
fn path_prefix_and_folder_filter() {
    let snap = fixture();
    assert_eq!(names(&snap, "path:deep", QueryOptions::default()), vec!["deep.txt"]);
    let got = names(&snap, "folder:*.rs", QueryOptions::default());
    assert!(got.is_empty());
}

#[test]
fn whole_word_and_startswith() {
    let snap = fixture();
    let opts = QueryOptions {
        whole_word: true,
        ..Default::default()
    };
    assert_eq!(names(&snap, "alpha", opts), vec!["alpha.txt"]);
    assert_eq!(
        names(&snap, "startwith:alpha", QueryOptions::default()),
        vec!["alpha.txt", "alphabet.md"]
    );
}

#[test]
fn snapshot_paths_are_reconstructed() {
    let snap = fixture();
    let slot = snap.by_name[0];
    let mut buf = Vec::new();
    snap.path(slot, &mut buf);
    let path = String::from_utf8_lossy(&buf).into_owned();
    assert!(path.starts_with("/test/root/"));
}

#[test]
fn size_filter() {
    let snap = fixture();
    let got = names(&snap, "size:>12", QueryOptions::default());
    assert_eq!(got, vec!["nested_one.rs", "nested_two.rs"]);
    let got = names(&snap, "size:8..12", QueryOptions::default());
    assert!(got.contains(&"alpha.txt".to_string()));
}

#[test]
fn extension_filter() {
    let snap = fixture();
    let got = names(&snap, "ext:rs", QueryOptions::default());
    assert_eq!(got, vec!["beta.rs", "nested_one.rs", "nested_two.rs"]);
    let got = names(&snap, "ext:md;txt", QueryOptions::default());
    assert!(got.contains(&"README.md".to_string()));
    assert!(got.contains(&"alpha.txt".to_string()));
}

#[test]
fn date_filter() {
    let snap = fixture();
    let got = names(&snap, "dm:<2020-01-01", QueryOptions::default());
    assert_eq!(got.len(), 9);
    let got = names(&snap, "dc:>2020-01-01", QueryOptions::default());
    assert!(got.is_empty());
}
