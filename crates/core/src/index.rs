/// Case-fold a filename into `out` for case-insensitive matching. ASCII stays
/// byte-wise; non-UTF-8 names fall back to ASCII lowercasing.
pub fn fold_into(name: &[u8], out: &mut Vec<u8>) {
    out.clear();
    if name.is_ascii() {
        out.extend(name.iter().map(|b| b.to_ascii_lowercase()));
        return;
    }
    if let Ok(s) = std::str::from_utf8(name) {
        let mut buf = String::with_capacity(s.len());
        for c in s.chars() {
            for lc in c.to_lowercase() {
                buf.push(lc);
            }
        }
        out.extend_from_slice(buf.as_bytes());
    } else {
        out.extend(name.iter().map(|b| b.to_ascii_lowercase()));
    }
}

pub fn fold_name(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    fold_into(name, &mut out);
    out
}
