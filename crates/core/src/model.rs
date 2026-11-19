use std::path::PathBuf;

pub type SlotId = u32;

pub const NULL_SLOT: SlotId = u32::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum EntryType {
    #[default]
    Other = 0,
    File = 1,
    Dir = 2,
    Symlink = 3,
}

impl EntryType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::File,
            2 => Self::Dir,
            3 => Self::Symlink,
            _ => Self::Other,
        }
    }
}

pub const F_HIDDEN: u8 = 1 << 0;
pub const F_NOACCESS: u8 = 1 << 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct EntryMeta {
    pub ino: u64,
    pub dev: u64,
    pub kind: EntryType,
    pub size: u64,
    pub mtime: i64,
    pub created: i64,
    pub accessed: i64,
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntryKey {
    pub parent: SlotId,
    pub name_off: u32,
}

#[derive(Clone, Debug)]
pub struct RootInfo {
    pub path: PathBuf,
    pub slot: SlotId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortBy {
    #[default]
    Name,
    NameDesc,
    Path,
    PathDesc,
    Size,
    SizeDesc,
    Mtime,
    MtimeDesc,
}

impl SortBy {
    pub fn parse(s: &str) -> Option<Self> {
        let (name, desc) = if let Some(rest) = s.strip_prefix('-') {
            (rest, true)
        } else {
            (s, false)
        };
        let base = match name.to_ascii_lowercase().as_str() {
            "name" | "n" => Self::Name,
            "path" | "p" => Self::Path,
            "size" | "s" => Self::Size,
            "mtime" | "date" | "t" => Self::Mtime,
            _ => return None,
        };
        Some(if desc { base.reversed() } else { base })
    }

    pub fn reversed(self) -> Self {
        match self {
            Self::Name => Self::NameDesc,
            Self::NameDesc => Self::Name,
            Self::Path => Self::PathDesc,
            Self::PathDesc => Self::Path,
            Self::Size => Self::SizeDesc,
            Self::SizeDesc => Self::Size,
            Self::Mtime => Self::MtimeDesc,
            Self::MtimeDesc => Self::Mtime,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct QueryOptions {
    pub case_sensitive: bool,
    pub regex: bool,
    pub whole_word: bool,
    pub path: bool,
    pub diacritics: bool,
    pub file_only: bool,
    pub folder_only: bool,
    pub sort: SortBy,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct QueryResult {
    pub slots: Vec<SlotId>,
    pub elapsed_ms: f64,
    pub total_matched: usize,
    pub generation: u64,
    pub truncated: bool,
}
