# Tasks

## 1. Workspace scaffolding

- [x] 1.1 Add an `input-clipboard` crate to the workspace with a stub `lib.rs` declaring the `Snapshot` type and the `Clipboard` trait; verify `cargo build -p input-clipboard` succeeds
- [x] 1.2 Add `clipboard-rs` and `tokio-rustls` as dependencies in the crates that use them; verify `cargo build --workspace` succeeds and `cargo clippy --workspace --all-targets --all-features` reports no new warnings

## 2. Clipboard backend and representation model

- [x] 2.1 Implement the macOS backend over `clipboard-rs`: enumerate all offered representations, read each as raw bytes, write a representation set; verify a unit test writes a value and reads the same representations back
- [x] 2.2 Add concealed/transient representation detection at snapshot time; verify a unit test with a fabricated representation list that includes a concealed marker yields no snapshot
- [x] 2.3 Add the canonical representation-name mapping table (both directions) with raw passthrough for unknown names; verify unit tests cover a known mapping each way, an unknown name surviving unchanged, and an empty name being rejected
- [x] 2.4 Add `CF_HTML` offset rewriting for transfers to and from Windows; verify a unit test with a fixed `CF_HTML` fixture round-trips byte offsets correctly and a malformed header is passed through untouched rather than corrupted
- [x] 2.5 Add the dummy backend used on platforms without a backend, logging that clipboard sharing is unavailable; verify the workspace builds on a non-macOS target and the log line is emitted once at startup
- [x] 2.6 Document the platform support matrix in the crate-level docs; verify `cargo doc -p input-clipboard` builds without warnings

## 3. Frame codec

- [x] 3.1 Implement the length-prefixed frame encoder and decoder (`u32` length, `u64` origin sequence, item count, `(name, bytes)` items); verify a round-trip test covering zero items, one item, and several items with mixed name lengths
- [x] 3.2 Reject a declared frame length above the configured cap before allocating the body; verify a test with an oversized declared length returns an error without a large allocation
- [x] 3.3 Ensure a truncated frame yields no snapshot; verify a test feeding a partial body returns "incomplete" and the caller never observes a partial item list

## 4. Side channel

- [x] 4.1 Add a TCP listener on the configured port using TLS with the existing certificate, accepting only peers whose fingerprint is in `authorized_keys`; verify a test that an unknown peer's connection is refused and no bytes are read
- [x] 4.2 Add the lazy connector with a per-peer connection cache, invalidated on failure; verify an in-process test transferring a frame between two endpoints and reusing the cached connection for a second transfer
- [ ] 4.3 Wire the channel's inbound arm into the service `select!` loop; verify two instances on the same host exchange a frame end to end
- [x] 4.4 Document the second channel in `DOC.md` (why TCP alongside UDP, and that the input protocol is unchanged); verify the described port behaviour matches `config.toml`

## 5. Sync state and service hooks

- [x] 5.1 Implement `local_seq` and per-peer `last_pushed` gating; verify unit tests cover the four cases from design D2: change-then-enter transfers, enter-without-change skips, repeated crossings skip, and re-copying identical content transfers again
- [ ] 5.2 Hook `ICaptureEvent::ClientEntered` to take the cached snapshot and send it; verify a manual two-machine test where text copied on one machine is pasted on the other
- [x] 5.3 Apply received snapshots and suppress the resulting local change event; verify a test that applying a snapshot does not increment the outbound sequence used for the sender, so no ping-pong occurs
- [x] 5.4 Never propagate an empty clipboard, and degrade over-cap content to the representations that fit; verify unit tests for both, and that a degradation emits a status event naming the dropped representation and its size
- [ ] 5.5 Run the clipboard watcher on its own thread and communicate with the service through a channel; verify that entering a client while a large transfer is running does not delay pointer events

## 6. Configuration and IPC

- [x] 6.1 Add the `[clipboard]` section with `enabled` (default `true`) and `max_size_mb` (default `100`, clamped on load); verify config tests for a missing section, an explicit disable, and an out-of-range value being clamped
- [x] 6.2 Add the `FrontendEvent::ClipboardStatus` variant and include it in the initial state sync; verify a frontend that connects after a degradation still receives the current status
- [x] 6.3 Document the new configuration section in the `README.md` configuration reference; verify the documented example parses with the config loader's test helper

## 7. GTK frontend

The two GTK frontend tasks that were planned here (the clipboard sharing
switch and the clipboard status line) moved to the `add-mygo-frontend`
change (its tasks 7.1 and 7.2), which builds them in the new Go frontend
instead of in GTK.

## 8. Remaining platform backends

- [ ] 8.1 Validate the Windows backend end to end, including HTML pasting into a rich text application; verify by manual paste of text, HTML and an image between Windows and macOS
- [ ] 8.2 Validate the X11 backend, including content large enough to require the X11 INCR path; verify by manual paste of an image larger than 1 MB between X11 and macOS
- [ ] 8.3 Validate the Wayland backend and record whether `clipboard-rs` reports the complete representation set, resolving the design's open question; verify by manual paste of text, HTML and an image between Wayland and macOS, and update the crate docs with the outcome

## 9. Integration verification

- [ ] 9.1 Run the cross-platform matrix (macOS, Windows, X11, Wayland pairs) for plain text, HTML, an image and a file list; verify each cell either works or is recorded as a documented limitation
- [ ] 9.2 Confirm input latency is unaffected during a transfer at the size cap; verify by comparing pointer smoothness with and without a large transfer running
- [x] 9.3 Update the `README.md` roadmap entry and the platform support tables; verify the rendered tables list the new capability per platform
- [x] 9.4 Run `cargo fmt`, `cargo clippy --workspace --all-targets --all-features` and `cargo test --workspace`; verify all are clean

## Workflow follow-up

- Archive the change after review requirements are satisfied.
