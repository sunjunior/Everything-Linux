use std::path::PathBuf;

use everything_core::model::{EntryMeta, EntryType, F_HIDDEN, NULL_SLOT, QueryOptions};
use everything_core::query;
use everything_core::store::Store;

fn fixture() -> Store {
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

    for (name, kind) in [
        ("alpha.txt", EntryType::File),
        ("alphabet.md", EntryType::File),
        ("beta.rs", EntryType::File),
        ("README.md", EntryType::File),
        ("src", EntryType::Dir),
    ] {
        s.add_entry(
            root,
            name.as_bytes(),
            EntryMeta {
                kind,
                size: name.len() as u64,
                ..Default::default()
            },
        );
    }
    let src = s.find_child(root, b"src").unwrap();
    for name in ["nested_one.rs", "nested_two.rs", "deep.txt"] {
        s.add_entry(
            src,
            name.as_bytes(),
            EntryMeta {
                kind: EntryType::File,
                size: name.len() as u64,
                ..Default::default()
            },
        );
    }
    s
}

fn names(s: &Store, q: &str, opts: QueryOptions) -> Vec<String> {
    let res = query::run(s, 1, q, opts).unwrap();
    res.slots
        .iter()
        .map(|&slot| String::from_utf8_lossy(s.name(slot)).into_owned())
        .collect()
}

#[test]
fn substring_is_case_insensitive_by_default() {
    let s = fixture();
    let got = names(&s, "ALPHA", QueryOptions::default());
    assert_eq!(got, vec!["alpha.txt", "alphabet.md"]);
}

#[test]
fn case_sensitive_query() {
    let s = fixture();
    let opts = QueryOptions {
        case_sensitive: true,
        ..Default::default()
    };
    assert_eq!(names(&s, "ALPHA", opts), Vec::<String>::new());
}

#[test]
fn wildcard_and_regex() {
    let s = fixture();
    assert_eq!(names(&s, "*.rs", QueryOptions::default()).len(), 3);
    let opts = QueryOptions {
        regex: true,
        ..Default::default()
    };
    assert_eq!(names(&s, "nested_[a-z]+", opts).len(), 2);
}

#[test]
fn boolean_or_and_not() {
    let s = fixture();
    assert_eq!(
        names(&s, "alpha.txt | beta.rs", QueryOptions::default()).len(),
        2
    );
    let got = names(&s, "*.md !README", QueryOptions::default());
    assert_eq!(got, vec!["alphabet.md"]);
}

#[test]
fn path_prefix_and_folder_filter() {
    let s = fixture();
    assert_eq!(
        names(&s, "path:deep", QueryOptions::default()),
        vec!["deep.txt"]
    );
    let got = names(&s, "folder:*.rs", QueryOptions::default());
    assert!(got.is_empty());
}

#[test]
fn whole_word_and_startswith() {
    let s = fixture();
    let opts = QueryOptions {
        whole_word: true,
        ..Default::default()
    };
    assert_eq!(names(&s, "alpha", opts), vec!["alpha.txt"]);
    assert_eq!(
        names(&s, "startwith:alpha", QueryOptions::default()),
        vec!["alpha.txt", "alphabet.md"]
    );
}

#[test]
fn paths_are_reconstructed() {
    let s = fixture();
    let slot = s.name_order()[0];
    assert_eq!(
        s.path_of(slot).unwrap(),
        PathBuf::from("/test/root/alpha.txt")
    );
}

#[test]
fn size_filter() {
    let s = fixture();
    let got = names(&s, "size:>12", QueryOptions::default());
    assert_eq!(got, vec!["nested_one.rs", "nested_two.rs"]);
    let got = names(&s, "size:8..12", QueryOptions::default());
    assert!(got.contains(&"alpha.txt".to_string()));
}

#[test]
fn extension_filter() {
    let s = fixture();
    let got = names(&s, "ext:rs", QueryOptions::default());
    assert_eq!(got, vec!["beta.rs", "nested_one.rs", "nested_two.rs"]);
    let got = names(&s, "ext:md;txt", QueryOptions::default());
    assert!(got.contains(&"README.md".to_string()));
    assert!(got.contains(&"alpha.txt".to_string()));
}

#[test]
fn date_filter() {
    let s = fixture();
    let got = names(&s, "dm:<2020-01-01", QueryOptions::default());
    assert_eq!(got.len(), 9);
    let got = names(&s, "dc:>2020-01-01", QueryOptions::default());
    assert!(got.is_empty());
}

#[test]
fn limit_keeps_top_n_before_truncation() {
    let s = fixture();
    let opts = QueryOptions {
        limit: 2,
        ..Default::default()
    };
    let res = query::run(&s, 1, "*.rs", opts).unwrap();
    let got: Vec<String> = res
        .slots
        .iter()
        .map(|&slot| String::from_utf8_lossy(s.name(slot)).into_owned())
        .collect();
    assert_eq!(got, vec!["beta.rs", "nested_one.rs"]);
    assert_eq!(res.total_matched, 3);
    assert!(res.truncated);

    let opts = QueryOptions {
        limit: 2,
        sort: everything_core::model::SortBy::NameDesc,
        ..Default::default()
    };
    let res = query::run(&s, 1, "*.rs", opts).unwrap();
    let got: Vec<String> = res
        .slots
        .iter()
        .map(|&slot| String::from_utf8_lossy(s.name(slot)).into_owned())
        .collect();
    assert_eq!(got, vec!["nested_two.rs", "nested_one.rs"]);
}

#[test]
fn size_desc_limit_keeps_largest() {
    let s = fixture();
    let opts = QueryOptions {
        limit: 2,
        sort: everything_core::model::SortBy::SizeDesc,
        ..Default::default()
    };
    let res = query::run(&s, 1, "*.rs", opts).unwrap();
    let got: Vec<String> = res
        .slots
        .iter()
        .map(|&slot| String::from_utf8_lossy(s.name(slot)).into_owned())
        .collect();
    assert_eq!(got.len(), 2);
    assert!(got.contains(&"nested_one.rs".to_string()));
    assert!(got.contains(&"nested_two.rs".to_string()));
}
