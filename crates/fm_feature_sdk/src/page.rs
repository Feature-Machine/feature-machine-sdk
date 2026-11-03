// The page SDK files (feature surfaces plan, section 6): page.js, kit.css and kit.js. The app
// serves them at <page base>_fm/<VERSION>/<file> for each page grant, so a page links them
// relative to its own folder and works the same in the app and on the dashboard. They live in
// this crate, so the SDK repository export carries them with the Rust SDK. A new version that
// breaks a page gets a new number; the app keeps serving the old ones.

/// The SDK version of the files below (the folder name under `_fm/`).
pub const VERSION: u32 = 1;

/// The files of version `VERSION`: name, bytes and content type.
pub const FILES: [(&str, &[u8], &str); 3] = [
    ("page.js", include_bytes!("../page/page.js"), "text/javascript; charset=utf-8"),
    ("kit.css", include_bytes!("../page/kit.css"), "text/css; charset=utf-8"),
    ("kit.js", include_bytes!("../page/kit.js"), "text/javascript; charset=utf-8"),
];

/// The file at `path` under `_fm/` ("1/page.js"): its bytes and content type. None for another
/// version or name.
pub fn file(path: &str) -> Option<(&'static [u8], &'static str)> {
    let (version, name) = path.split_once('/')?;
    if version != VERSION.to_string() {
        return None;
    }
    FILES.iter().find(|(n, _, _)| *n == name).map(|(_, b, t)| (*b, *t))
}

#[cfg(test)]
mod tests {
    #[test]
    fn files_by_version_and_name() {
        assert!(super::file("1/page.js").is_some_and(|(b, t)| b.starts_with(b"// Feature Machine page SDK") && t.starts_with("text/javascript")));
        assert!(super::file("1/kit.css").is_some());
        for bad in ["2/page.js", "1/other.js", "page.js", "1/page.js/x", "1/", ""] {
            assert!(super::file(bad).is_none(), "{bad}");
        }
    }
}
