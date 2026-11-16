//! Windows `CF_HTML` framing.
//!
//! Windows does not store HTML as raw markup: it prefixes a header whose
//! `StartHTML`/`EndHTML`/`StartFragment`/`EndFragment` fields are byte
//! offsets into the whole payload. Copying those offsets between machines
//! corrupts the content, so the header is rebuilt with correct offsets on
//! the way in and stripped on the way out.
//!
//! A payload whose header cannot be parsed is returned unchanged rather
//! than rewritten into something broken.

/// Marker surrounding the fragment inside the HTML body.
const START_FRAGMENT: &[u8] = b"<!--StartFragment-->";
const END_FRAGMENT: &[u8] = b"<!--EndFragment-->";

/// Wrapper written around the fragment when building a payload.
const BODY_PREFIX: &[u8] = b"<html><body><!--StartFragment-->";
const BODY_SUFFIX: &[u8] = b"<!--EndFragment--></body></html>";

const KEYS: [&str; 4] = ["StartHTML", "EndHTML", "StartFragment", "EndFragment"];

#[derive(Debug, PartialEq, Eq)]
struct Header {
    version: String,
    /// byte offset where the body starts
    header_len: usize,
    offsets: [usize; 4],
}

/// Find the end of the line starting at `pos`, returning
/// `(line_end, next_line_start)`.
fn line_bounds(data: &[u8], pos: usize) -> Option<(usize, usize)> {
    if pos >= data.len() {
        return None;
    }
    let rest = &data[pos..];
    let end = rest.iter().position(|b| *b == b'\n')?;
    let line_end = if end > 0 && rest[end - 1] == b'\r' {
        end - 1
    } else {
        end
    };
    Some((pos + line_end, pos + end + 1))
}

fn parse_header(payload: &[u8]) -> Option<Header> {
    let mut pos = 0;
    let mut version = String::from("1.0");
    let mut offsets: [Option<usize>; 4] = [None; 4];

    while let Some((line_end, next)) = line_bounds(payload, pos) {
        let line = &payload[pos..line_end];
        let Some(colon) = line.iter().position(|b| *b == b':') else {
            // not a header line: the body started
            break;
        };
        let Ok(key) = std::str::from_utf8(&line[..colon]) else {
            break;
        };
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
            break;
        }
        let Ok(value) = std::str::from_utf8(&line[colon + 1..]) else {
            break;
        };
        let value = value.trim();
        if key == "Version" {
            version = value.to_owned();
        } else if let Some(idx) = KEYS.iter().position(|k| *k == key) {
            offsets[idx] = value.parse::<usize>().ok();
        }
        pos = next;
    }

    let [
        Some(start_html),
        Some(end_html),
        Some(start_fragment),
        Some(end_fragment),
    ] = offsets
    else {
        return None;
    };
    let header = Header {
        version,
        header_len: pos,
        offsets: [start_html, end_html, start_fragment, end_fragment],
    };
    if end_html > payload.len() {
        return None;
    }
    Some(header)
}

/// Byte range of the fragment inside a parsed payload.
///
/// The `<!--StartFragment-->` markers are preferred over the declared
/// offsets: offsets are what gets corrupted when a payload is copied
/// between machines, while the markers travel with the markup.
fn fragment_bounds(payload: &[u8], header: &Header) -> (usize, usize) {
    let start = find(payload, START_FRAGMENT, 0).map(|i| i + START_FRAGMENT.len());
    if let Some(start) = start {
        if let Some(end) = find(payload, END_FRAGMENT, start) {
            return (start, end);
        }
    }
    (header.offsets[2], header.offsets[3])
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn build(version: &str, fragment: &[u8]) -> Vec<u8> {
    let render = |start_html: usize,
                  end_html: usize,
                  start_fragment: usize,
                  end_fragment: usize| {
        format!(
            "Version:{version}\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
        )
        .into_bytes()
    };
    // offsets are fixed width, so the header length does not depend on them
    let header_len = render(0, 0, 0, 0).len();
    let body_len = BODY_PREFIX.len() + fragment.len() + BODY_SUFFIX.len();
    let start_fragment = header_len + BODY_PREFIX.len();
    let mut out = render(
        header_len,
        header_len + body_len,
        start_fragment,
        start_fragment + fragment.len(),
    );
    out.extend_from_slice(BODY_PREFIX);
    out.extend_from_slice(fragment);
    out.extend_from_slice(BODY_SUFFIX);
    out
}

/// Wrap a plain HTML fragment in `CF_HTML` framing.
pub fn wrap(fragment: &[u8]) -> Vec<u8> {
    build("1.0", fragment)
}

/// The HTML fragment of a `CF_HTML` payload, or the input when the
/// header does not parse.
pub fn unwrap_fragment(payload: &[u8]) -> &[u8] {
    match parse_header(payload) {
        Some(header) => {
            let (start, end) = fragment_bounds(payload, &header);
            payload.get(start..end).unwrap_or(payload)
        }
        None => payload,
    }
}

/// Rebuild a `CF_HTML` payload with correct offsets.
///
/// Returns the input unchanged when the header does not parse.
pub fn rewrite(payload: &[u8]) -> Vec<u8> {
    match parse_header(payload) {
        Some(header) => {
            let (start, end) = fragment_bounds(payload, &header);
            match payload.get(start..end) {
                Some(fragment) => build(&header.version, fragment),
                None => payload.to_vec(),
            }
        }
        None => payload.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_FRAGMENT: &[u8] = b"<b>hello</b>";

    /// A hand-written payload with deliberately wrong offsets, as a
    /// naive copy between machines produces.
    fn broken_fixture() -> Vec<u8> {
        let mut payload =
            b"Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n"
                .to_vec();
        payload.extend_from_slice(b"<html><body><!--StartFragment-->");
        payload.extend_from_slice(FIXTURE_FRAGMENT);
        payload.extend_from_slice(b"<!--EndFragment--></body></html>");
        payload
    }

    #[test]
    fn wrap_then_unwrap_round_trips_the_fragment() {
        let payload = wrap(FIXTURE_FRAGMENT);
        assert_eq!(unwrap_fragment(&payload), FIXTURE_FRAGMENT);
    }

    #[test]
    fn wrap_emits_offsets_that_point_at_the_real_bytes() {
        let payload = wrap(FIXTURE_FRAGMENT);
        let header = parse_header(&payload).expect("header parses");

        assert_eq!(
            &payload[header.offsets[0]..header.offsets[1]],
            &payload[header.header_len..]
        );
        assert_eq!(
            &payload[header.offsets[2]..header.offsets[3]],
            FIXTURE_FRAGMENT
        );
        assert_eq!(header.offsets[0], header.header_len);
        assert_eq!(header.offsets[1], payload.len());
    }

    #[test]
    fn rewrite_repairs_wrong_offsets_and_keeps_the_version() {
        let repaired = rewrite(&broken_fixture());
        assert_ne!(repaired, broken_fixture());

        let header = parse_header(&repaired).expect("header parses");
        assert_eq!(header.version, "0.9");
        assert_eq!(
            &repaired[header.offsets[2]..header.offsets[3]],
            FIXTURE_FRAGMENT
        );
        assert_eq!(header.offsets[1], repaired.len());
    }

    #[test]
    fn rewrite_is_idempotent() {
        let once = rewrite(&broken_fixture());
        let twice = rewrite(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn malformed_header_is_passed_through_untouched() {
        let garbage = b"this is not a cf_html payload".to_vec();
        assert_eq!(rewrite(&garbage), garbage);
        assert_eq!(unwrap_fragment(&garbage), garbage);

        // header present but an offset points outside the payload
        let out_of_range = b"Version:1.0\r\nStartHTML:0000000000\r\nEndHTML:9999999999\r\nStartFragment:0000000000\r\nEndFragment:9999999999\r\n<body>"
            .to_vec();
        assert_eq!(rewrite(&out_of_range), out_of_range);
    }

    #[test]
    fn empty_fragment_round_trips() {
        let payload = wrap(b"");
        assert_eq!(unwrap_fragment(&payload), b"");
    }
}
