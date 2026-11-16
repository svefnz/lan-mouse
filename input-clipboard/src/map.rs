//! Canonical representation names.
//!
//! Each platform names the same clipboard representation differently
//! (`public.utf8-plain-text` on macOS, `CF_UNICODETEXT` on Windows,
//! `text/plain;charset=utf-8` on Wayland). Transferring the raw name
//! would make content invisible on the other side, so names are
//! normalized to a canonical form on the way out and mapped back to the
//! destination platform's name on the way in.
//!
//! Names outside the table are passed through unchanged, so an
//! application-private format survives a transfer between two machines
//! that use the same convention.

use std::fmt::Display;

/// Platforms whose representation names differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    X11,
    Wayland,
}

impl Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Platform::MacOs => "macOS",
            Platform::Windows => "windows",
            Platform::X11 => "X11",
            Platform::Wayland => "wayland",
        };
        write!(f, "{name}")
    }
}

/// The platform this build runs on.
pub const fn current_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform::MacOs
    }
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // X11 and Wayland share the same canonical MIME names, so the
        // distinction only matters when choosing a local name back.
        Platform::X11
    }
}

/// Local representation name -> canonical name.
const CANONICAL: &[(&str, &str)] = &[
    // macOS
    ("public.utf8-plain-text", "text/plain"),
    ("NSStringPboardType", "text/plain"),
    ("public.html", "text/html"),
    ("public.rtf", "text/rtf"),
    ("public.png", "image/png"),
    ("public.tiff", "image/tiff"),
    ("public.file-url", "text/uri-list"),
    ("NSFilenamesPboardType", "text/uri-list"),
    // Windows
    ("CF_UNICODETEXT", "text/plain"),
    ("CF_HTML", "text/html"),
    ("Rich Text Format", "text/rtf"),
    ("PNG", "image/png"),
    ("CF_DIBV5", "image/bmp"),
    ("CF_DIB", "image/bmp"),
    ("CF_HDROP", "text/uri-list"),
    // X11 / Wayland
    ("UTF8_STRING", "text/plain"),
    ("STRING", "text/plain"),
    ("text/plain;charset=utf-8", "text/plain"),
    ("text/plain", "text/plain"),
    ("text/html", "text/html"),
    ("text/rtf", "text/rtf"),
    ("image/png", "image/png"),
    ("image/bmp", "image/bmp"),
    ("text/uri-list", "text/uri-list"),
];

/// Canonical name -> preferred local representation name, per platform.
fn local_table(platform: Platform) -> &'static [(&'static str, &'static str)] {
    match platform {
        Platform::MacOs => &[
            ("text/plain", "public.utf8-plain-text"),
            ("text/html", "public.html"),
            ("text/rtf", "public.rtf"),
            ("image/png", "public.png"),
            ("image/tiff", "public.tiff"),
            ("image/bmp", "public.tiff"),
            ("text/uri-list", "public.file-url"),
        ],
        Platform::Windows => &[
            ("text/plain", "CF_UNICODETEXT"),
            ("text/html", "CF_HTML"),
            ("text/rtf", "Rich Text Format"),
            ("image/png", "PNG"),
            ("image/tiff", "PNG"),
            ("image/bmp", "CF_DIBV5"),
            ("text/uri-list", "CF_HDROP"),
        ],
        Platform::X11 | Platform::Wayland => &[
            ("text/plain", "text/plain;charset=utf-8"),
            ("text/html", "text/html"),
            ("text/rtf", "text/rtf"),
            ("image/png", "image/png"),
            ("image/tiff", "image/png"),
            ("image/bmp", "image/png"),
            ("text/uri-list", "text/uri-list"),
        ],
    }
}

/// Canonical name for a local representation name.
///
/// Returns `None` for an empty name, which is never a valid
/// representation. Unknown names are returned unchanged.
pub fn to_canonical(name: &str) -> Option<&str> {
    if name.is_empty() {
        return None;
    }
    Some(
        CANONICAL
            .iter()
            .find(|(local, _)| *local == name)
            .map(|(_, canonical)| *canonical)
            .unwrap_or(name),
    )
}

/// Preferred local representation name for a canonical name.
///
/// Returns `None` when the destination platform has no mapping, in which
/// case the canonical name should be written as-is.
pub fn to_local(platform: Platform, canonical: &str) -> Option<&'static str> {
    local_table(platform)
        .iter()
        .find(|(c, _)| *c == canonical)
        .map(|(_, local)| *local)
}

/// Whether a canonical name is plain text.
pub fn is_text(canonical: &str) -> bool {
    canonical == "text/plain"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_map_to_canonical() {
        assert_eq!(to_canonical("public.utf8-plain-text"), Some("text/plain"));
        assert_eq!(to_canonical("CF_UNICODETEXT"), Some("text/plain"));
        assert_eq!(to_canonical("UTF8_STRING"), Some("text/plain"));
        assert_eq!(to_canonical("public.html"), Some("text/html"));
        assert_eq!(to_canonical("CF_HTML"), Some("text/html"));
        assert_eq!(to_canonical("public.file-url"), Some("text/uri-list"));
    }

    #[test]
    fn canonical_names_map_back_to_local() {
        assert_eq!(
            to_local(Platform::MacOs, "text/plain"),
            Some("public.utf8-plain-text")
        );
        assert_eq!(
            to_local(Platform::Windows, "text/plain"),
            Some("CF_UNICODETEXT")
        );
        assert_eq!(
            to_local(Platform::X11, "text/plain"),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(to_local(Platform::MacOs, "text/html"), Some("public.html"));
    }

    #[test]
    fn unknown_names_pass_through_unchanged() {
        assert_eq!(
            to_canonical("com.example.private"),
            Some("com.example.private")
        );
        assert_eq!(
            to_canonical("application/x-krita"),
            Some("application/x-krita")
        );
        // no mapping back, caller writes the canonical name as-is
        assert_eq!(to_local(Platform::MacOs, "com.example.private"), None);
    }

    #[test]
    fn empty_name_is_rejected() {
        assert_eq!(to_canonical(""), None);
    }

    #[test]
    fn every_canonical_has_a_local_mapping_on_every_platform() {
        let canonical: Vec<&str> = CANONICAL.iter().map(|(_, c)| *c).collect();
        for platform in [
            Platform::MacOs,
            Platform::Windows,
            Platform::X11,
            Platform::Wayland,
        ] {
            for name in &canonical {
                assert!(
                    to_local(platform, name).is_some(),
                    "{name} has no {platform} mapping"
                );
            }
        }
    }
}
