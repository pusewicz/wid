//! `file:` URIs and the paths they name.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use lsp_types::Uri;

/// The absolute path a `file:` URI names, cleaned (`a/./b/../c` is `a/c`);
/// `None` for another scheme, a malformed URI or another host.
pub(crate) fn to_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let scheme = text.get(..7)?;
    if !scheme.eq_ignore_ascii_case("file://") {
        return None;
    }
    let rest = &text[7..];
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = decode(path)?;
    native(host, &path)
}

#[cfg(not(windows))]
fn native(host: &str, path: &str) -> Option<PathBuf> {
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    Some(clean(Path::new(path)))
}

/// On Windows, `/C:/dir/file` is `C:\dir\file` and `//server/share/file`
/// a UNC path.
#[cfg(windows)]
fn native(host: &str, path: &str) -> Option<PathBuf> {
    let path = path.replace('/', "\\");
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return Some(clean(Path::new(&format!("\\\\{host}{path}"))));
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0] == b'\\' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    let path = if drive { &path[1..] } else { path.as_str() };
    Some(clean(Path::new(path)))
}

/// The `file:` URI of an absolute path; `None` for a relative one.
pub(crate) fn from_path(path: &Path) -> Option<Uri> {
    if !path.is_absolute() {
        return None;
    }
    let text = path.to_string_lossy();
    #[cfg(windows)]
    let text = {
        let slashed = text.replace('\\', "/");
        match slashed.strip_prefix("//") {
            // A UNC path's server is the URI's host.
            Some(unc) => unc.to_string(),
            None => format!("/{slashed}"),
        }
    };
    let mut out = String::from("file://");
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Uri::from_str(&out).ok()
}

/// Decodes `%XX` escapes; `None` when the result isn't UTF-8. A `%` not
/// followed by two hex digits stays as it is.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) =
                (bytes.get(i + 1).and_then(|&b| hex(b)), bytes.get(i + 2).and_then(|&b| hex(b)))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A path without `.` components, `dir/..` folded, as the loader spells
/// paths.
pub(crate) fn clean(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::str::FromStr;

    use lsp_types::Uri;

    use super::{from_path, to_path};

    fn uri(text: &str) -> Uri {
        Uri::from_str(text).expect("a valid URI")
    }

    #[cfg(not(windows))]
    #[test]
    fn file_uris_name_paths() {
        assert_eq!(to_path(&uri("file:///home/me/game/main.wid")), Some(PathBuf::from("/home/me/game/main.wid")));
        assert_eq!(to_path(&uri("file:///a%20b/caf%C3%A9.wid")), Some(PathBuf::from("/a b/café.wid")));
        assert_eq!(to_path(&uri("file://localhost/x/./y/../z.wid")), Some(PathBuf::from("/x/z.wid")));
        assert_eq!(to_path(&uri("untitled:Untitled-1")), None);
        assert_eq!(to_path(&uri("file://server/share/x.wid")), None);
        let path = PathBuf::from("/tmp/my game/café #1.wid");
        let made = from_path(&path).expect("an absolute path");
        assert_eq!(made.as_str(), "file:///tmp/my%20game/caf%C3%A9%20%231.wid");
        assert_eq!(to_path(&made), Some(path));
        assert!(from_path(&PathBuf::from("relative.wid")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn file_uris_name_paths() {
        assert_eq!(to_path(&uri("file:///C:/game/main.wid")), Some(PathBuf::from(r"C:\game\main.wid")));
        assert_eq!(to_path(&uri("file:///c%3A/game/main.wid")), Some(PathBuf::from(r"c:\game\main.wid")));
        let path = PathBuf::from(r"C:\my game\main.wid");
        let made = from_path(&path).expect("an absolute path");
        assert_eq!(made.as_str(), "file:///C:/my%20game/main.wid");
        assert_eq!(to_path(&made), Some(path));
    }
}
