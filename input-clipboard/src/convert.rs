//! Conversion between local and canonical representation sets.
//!
//! Sending normalizes names to their canonical form and strips the
//! Windows `CF_HTML` framing; receiving maps them back to the local
//! platform's names and adds that framing where the destination needs
//! it.

use crate::{
    Representation, Snapshot, cf_html,
    map::{self, Platform},
};

/// Convert a local snapshot into the form sent to a peer.
pub fn to_wire(platform: Platform, snapshot: &Snapshot) -> Snapshot {
    let items = snapshot
        .items
        .iter()
        .filter_map(|item| {
            let canonical = map::to_canonical(&item.name)?;
            let bytes = if canonical == "text/html" && platform == Platform::Windows {
                cf_html::unwrap_fragment(&item.bytes).to_vec()
            } else {
                item.bytes.clone()
            };
            Some(Representation::new(canonical, bytes))
        })
        .collect();

    Snapshot {
        items,
        dropped: snapshot.dropped.clone(),
    }
}

/// Convert a received snapshot into this platform's local form.
pub fn from_wire(platform: Platform, snapshot: &Snapshot) -> Snapshot {
    let items = snapshot
        .items
        .iter()
        .map(|item| {
            let local = map::to_local(platform, &item.name).unwrap_or(item.name.as_str());
            let bytes = if local == "CF_HTML" {
                cf_html::wrap(&item.bytes)
            } else {
                item.bytes.clone()
            };
            Representation::new(local, bytes)
        })
        .collect();

    Snapshot {
        items,
        dropped: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(name: &str, bytes: &[u8]) -> Representation {
        Representation::new(name, bytes.to_vec())
    }

    fn snapshot(items: Vec<Representation>) -> Snapshot {
        Snapshot {
            items,
            dropped: Vec::new(),
        }
    }

    #[test]
    fn macos_names_become_canonical() {
        let local = snapshot(vec![
            rep("public.utf8-plain-text", b"hello"),
            rep("public.html", b"<b>hello</b>"),
        ]);
        let wire = to_wire(Platform::MacOs, &local);
        let names: Vec<&str> = wire.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["text/plain", "text/html"]);
        assert_eq!(wire.items[0].bytes, b"hello");
    }

    #[test]
    fn canonical_names_become_linux_names() {
        let wire = snapshot(vec![rep("text/plain", b"hello")]);
        let local = from_wire(Platform::X11, &wire);
        assert_eq!(local.items[0].name, "text/plain;charset=utf-8");
        assert_eq!(local.items[0].bytes, b"hello");
    }

    #[test]
    fn windows_html_is_wrapped_on_receive_and_stripped_on_send() {
        let wire = snapshot(vec![rep("text/html", b"<b>hi</b>")]);

        let local = from_wire(Platform::Windows, &wire);
        assert_eq!(local.items[0].name, "CF_HTML");
        assert_ne!(local.items[0].bytes, b"<b>hi</b>");
        assert_eq!(
            cf_html::unwrap_fragment(&local.items[0].bytes),
            b"<b>hi</b>"
        );

        // and back again
        let round_trip = to_wire(Platform::Windows, &local);
        assert_eq!(round_trip.items[0].name, "text/html");
        assert_eq!(round_trip.items[0].bytes, b"<b>hi</b>");
    }

    #[test]
    fn unknown_types_keep_their_name_and_bytes() {
        let local = snapshot(vec![rep("com.example.private", b"\x01\x02")]);
        let wire = to_wire(Platform::MacOs, &local);
        assert_eq!(wire.items[0].name, "com.example.private");
        assert_eq!(wire.items[0].bytes, b"\x01\x02");

        let back = from_wire(Platform::Windows, &wire);
        assert_eq!(back.items[0].name, "com.example.private");
        assert_eq!(back.items[0].bytes, b"\x01\x02");
    }

    #[test]
    fn an_empty_representation_name_is_dropped() {
        let local = snapshot(vec![rep("", b"x"), rep("text/plain", b"y")]);
        let wire = to_wire(Platform::MacOs, &local);
        assert_eq!(wire.items.len(), 1);
    }
}
