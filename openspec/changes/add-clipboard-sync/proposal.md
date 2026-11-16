# Proposal

## Why

lan-mouse shares a mouse and keyboard across machines but not the thing people use them for: copy and paste. Copying on one machine and pasting on another currently requires a third-party tool or a manual round trip over the network. Clipboard support has been an open item on the roadmap, and it is the last missing piece that makes a multi-machine setup feel like one desktop.

## What Changes

- Add a cross-platform clipboard backend crate that exposes the clipboard as a stream of snapshots. A snapshot is the full set of `(type, bytes)` representations the OS currently offers for one clipboard change, so rich content (plain text + HTML + RTF + image together) is transferred faithfully rather than collapsed to one format.
- Add a reliable side channel for clipboard payloads over TLS on the existing configured port, using length-prefixed frames. The existing DTLS/UDP input path and the `lan-mouse-proto` wire format are untouched, so input latency and protocol compatibility are unaffected.
- Sync on screen entry: when the cursor enters a client, push this machine's clipboard only if it has changed since the last successful push to that client. Merely moving the cursor across the boundary never overwrites the other machine's clipboard.
- Normalize clipboard type names through a canonical mapping table so content survives macOS, Windows, X11 and Wayland; types outside the table are passed through as raw bytes. Rewrite `CF_HTML` byte offsets when crossing to and from Windows.
- Never propagate clipboard content flagged as concealed or transient by the source application.
- Enforce a configurable size cap (default 100 MB). Content above the cap degrades to the small formats instead of failing the whole transfer.
- Add a per-machine on/off switch in the configuration, surfaced in the GTK settings page. Disabling it stops both sending and receiving.
- Report degradation and in-progress transfers in a silent GTK status line; connection failures only log.

Out of scope for this change, deliberately:

- Continuous mirroring of every clipboard change. Sync is triggered by screen entry only.
- Transferring file contents. File lists travel as `text/uri-list` paths, which are only useful when a shared mount exists.
- Clipboard history, lazy/promised data materialization, and X11 `PRIMARY` selection.

## Capabilities

### New Capabilities

- `clipboard-sync`: transferring the clipboard between paired lan-mouse peers, including when a transfer is triggered, which content may be transferred, how content is preserved across platforms, and how failures are surfaced.

### Modified Capabilities

None. This is the first capability recorded for this project.

## Impact

- **New crate** `input-clipboard` (workspace member), with a platform backend plus a dummy fallback.
- **New module** in the root crate for the side-channel listener/connector, frame codec, sync state and type mapping; wired into the existing service select loop.
- **`src/config.rs`**: new `[clipboard]` section with `enabled` and `max_size_mb`.
- **`lan-mouse-ipc`**: new `FrontendEvent` variant for clipboard status. This is the frontend/service socket, not the peer wire protocol, so no protocol version bump.
- **`lan-mouse-gtk`**: settings switch and status line.
- **`lan-mouse-proto`**: unchanged.
- **New dependencies**: `clipboard-rs` (platform clipboard access) and `tokio-rustls` (TLS for the side channel; `rustls` and `rcgen` are already in use for peer certificates).
- **Docs**: `README.md` roadmap entry and platform support tables, `DOC.md` architecture note for the second channel.
