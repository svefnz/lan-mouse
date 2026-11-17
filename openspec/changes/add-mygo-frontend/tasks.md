# Tasks

## 1. Frontend protocol contract

- [x] 1.1 Add `IPC_PROTOCOL_VERSION` and a `Hello` request/event pair to `lan-mouse-ipc`, treating a client whose first message is not `Hello` as a legacy client (version 0, no capabilities); verify unit tests for a v1 client negotiating, a legacy client still receiving state, and a version the service does not support being rejected with an explicit error
- [x] 1.2 Add `FrontendRequest::SetClipboardEnabled(bool)`, applied to the config and saved by the service, mirroring `ChangePort`; verify a test that toggling writes `[clipboard] enabled` to the config file
- [x] 1.3 Document the frontend protocol in `DOC.md`: socket paths per platform, newline-delimited JSON framing, the handshake, and every request and event; verify by implementing the Go client from that section alone in task 3.2 and correcting any discrepancy found
- [x] 1.4 Remove the two GTK frontend tasks (7.1, 7.2) from `openspec/changes/add-clipboard-sync/tasks.md` and note that they moved to this change; verify `openspec validate add-clipboard-sync` still passes

## 2. Go module and framework spike

- [x] 2.1 Create the `frontend-mygo` Go module with a pinned mygo version and no cgo; verify `go build ./...` and `go vet ./...` succeed on macOS
- [x] 2.2 Spike the native UI: one window containing a list with rows, a text entry, a toggle, a button and a menu; verify by running it on macOS and recording in `frontend-mygo/README.md` whether a macOS menu bar status item equivalent to `lan-mouse-gtk/src/macos_status_item.rs` is reachable (this answers the design's open question)

## 3. Go IPC client

- [x] 3.1 Implement the transport: unix socket on macOS and Linux using the same path resolution as `lan-mouse-ipc`, TCP on `127.0.0.1:5252` on Windows, newline-delimited JSON; verify a Go test against a fake server that echoes a request and returns an event
- [x] 3.2 Implement the request and event types plus the handshake, transcribed from the `DOC.md` section written in 1.3; verify a round-trip test using JSON fixtures captured from the running Rust service, and fix `DOC.md` where it disagrees with reality
- [x] 3.3 Verify an incompatible protocol version produces a clear error in the Go client rather than a decode failure, by pointing it at a fake server that answers with a future version

## 4. Frontend shell

- [x] 4.1 Implement the state struct, the goroutine that reads events and the redraw loop, matching design D8; verify a test that replays a recorded event stream and produces the expected view state
- [x] 4.2 Implement connect-or-spawn: connect when a service is already running, otherwise start `lan-mouse daemon` and stop it with SIGINT on exit; verify manually that launching with no service shows the window, and closing it leaves no daemon process behind
- [x] 4.3 Handle a service that exits while the frontend is open; verify manually that the frontend reports it and reconnects when the service returns

## 5. Client management

- [x] 5.1 Render the client list with each client's hostname, addresses, port, position and active state; verify against a service configured with three clients that all three rows match `config.toml`
- [x] 5.2 Implement add and delete; verify that adding writes a new entry and deleting removes it from `config.toml`
- [x] 5.3 Implement editing hostname, port, position and fixed addresses, and the active toggle; verify that changing a position moves the capture zone and that toggling active starts and stops capture

## 6. Settings and pairing

- [x] 6.1 Show and change the listen port, and show capture and emulation status; verify that changing the port updates `config.toml` and the service reports the new port
- [x] 6.2 Display this machine's public key fingerprint and manage authorized peer fingerprints; verify that adding then removing a fingerprint is reflected in `authorized_fingerprints` in `config.toml`
- [x] 6.3 Implement the incoming-device approval dialog; verify that an unauthorized device produces the prompt and that approving lets it connect while rejecting leaves it refused

## 7. Clipboard frontend

- [x] 7.1 Implement the clipboard sharing switch bound to `SetClipboardEnabled`; verify that toggling writes `[clipboard] enabled` and that the service stops transferring clipboard content when it is off
- [x] 7.2 Implement the clipboard status area driven by `FrontendEvent::ClipboardStatus`; verify that a forced over-limit copy shows the degradation message and that the message clears on the next successful change

## 8. Build and packaging

- [x] 8.1 Make the root `build.rs` run `go build` into the cargo target directory when the `mygo` feature is enabled, honouring a `GO` override and failing with an actionable message when Go is missing; verify a clean `cargo build` produces both `lan-mouse` and `lan-mouse-ui`
- [x] 8.2 Move `mygo` into the default features and take `gtk` out of it; verify `cargo build` yields the Go frontend, `cargo build --no-default-features --features gtk` yields the GTK frontend, and `src/main.rs` falls back to running the daemon when `lan-mouse-ui` is absent
- [ ] 8.3 Place both binaries in `Contents/MacOS/` of the bundle and verify that the accessibility grant attaches to the bundle rather than to the daemon alone, including after rebuilding the bundle; record the result in the README

## 9. Documentation

- [x] 9.1 Rewrite the macOS, Linux and Windows build sections of `README.md` for the new frontend, keeping the `--features gtk` path documented as the fallback; verify each documented command runs as written on the platform it names
- [x] 9.2 Add a frontend section to the README's OS support tables stating that the default build needs no GUI toolkit at build time and that Linux needs the GTK 3 runtime at run time; verify the rendered tables match the behaviour observed in 8.2 and 8.3

## 10. CI

- [x] 10.1 Add a Go job to `.github/workflows/rust.yml` running `go build ./...`, `go vet ./...` and `go test ./...` in `frontend-mygo`; verify the job passes on ubuntu, macOS and Windows (run 44014159410, all three green)
- [x] 10.2 Confirm the Rust jobs no longer install GTK on macOS unless the `gtk` feature is being built; verify the macOS job runs without `brew install gtk4 libadwaita` and still passes (run 44014159410)

## Workflow follow-up

- Archive the change after review requirements are satisfied.
- Removing `lan-mouse-gtk` is a separate follow-up change, once the Go frontend has been used in practice.
