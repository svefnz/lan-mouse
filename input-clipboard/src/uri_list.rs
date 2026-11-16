//! `text/uri-list` conversion for file lists.
//!
//! The clipboard carries file lists as URIs, but the platform APIs used
//! to read and write them deal in filesystem paths. Paths containing
//! spaces or non-ASCII characters must be percent-encoded to survive the
//! round trip.

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Characters left unescaped in a URI path.
fn is_unescaped(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':')
}

/// Escape a path for use inside a `file://` URI.
fn escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if is_unescaped(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xf) as usize] as char);
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build a `text/uri-list` body from filesystem paths.
pub fn from_paths(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| {
            let normalised = p.replace('\\', "/");
            let with_root = if normalised.starts_with('/') {
                normalised
            } else {
                format!("/{normalised}")
            };
            format!("file://{}", escape(&with_root))
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// Filesystem paths from a `text/uri-list` body.
///
/// Lines that are blank or comments (`#…`) are skipped, as are URIs that
/// are not `file:` URIs.
pub fn to_paths(uri_list: &str) -> Vec<String> {
    uri_list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        .map(|p| {
            let unescaped = unescape(p);
            // strip the leading slash of a Windows drive path: /C:/x -> C:/x
            let bytes = unescaped.as_bytes();
            if bytes.len() > 2 && bytes[0] == b'/' && bytes[2] == b':' {
                unescaped[1..].to_owned()
            } else {
                unescaped
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_round_trip() {
        let paths = vec!["/Users/me/a.pdf".to_owned()];
        assert_eq!(from_paths(&paths), "file:///Users/me/a.pdf");
        assert_eq!(to_paths(&from_paths(&paths)), paths);
    }

    #[test]
    fn spaces_and_non_ascii_round_trip() {
        let paths = vec![
            "/Users/me/My File.pdf".to_owned(),
            "/Users/me/文档/报告.txt".to_owned(),
        ];
        let uri = from_paths(&paths);
        assert!(uri.contains("My%20File.pdf"));
        assert_eq!(to_paths(&uri), paths);
    }

    #[test]
    fn several_paths_are_separated_by_crlf() {
        let paths = vec!["/a".to_owned(), "/b".to_owned()];
        assert_eq!(from_paths(&paths), "file:///a\r\nfile:///b");
        assert_eq!(to_paths(&from_paths(&paths)), paths);
    }

    #[test]
    fn windows_paths_round_trip() {
        let paths = vec!["C:\\Users\\me\\a.pdf".to_owned()];
        let uri = from_paths(&paths);
        assert_eq!(uri, "file:///C:/Users/me/a.pdf");
        assert_eq!(to_paths(&uri), vec!["C:/Users/me/a.pdf".to_owned()]);
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let uri = "# comment\r\n\r\nfile:///a\r\n";
        assert_eq!(to_paths(uri), vec!["/a".to_owned()]);
    }

    #[test]
    fn non_file_uris_are_skipped() {
        assert!(to_paths("https://example.com/x").is_empty());
    }
}
