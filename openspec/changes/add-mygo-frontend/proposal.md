# Proposal

## Why

The GTK4/libadwaita frontend is the only part of lan-mouse that needs a system
toolkit to build. Everything else — capture, emulation, the peer protocol, the
service loop — is plain Rust that `cargo build` handles with no system
packages. Because of GTK, building the GUI requires `pkg-config` and the GTK
development headers on every platform, and shipping it on macOS means bundling
GTK's own dylibs into the `.app`. That single dependency is the main barrier to
building and distributing lan-mouse on macOS, and it is what keeps the project
from producing a small self-contained binary.

The service already talks to its frontends over a documented socket protocol,
and two frontends (GTK and CLI) already exist against it. Replacing the
graphical frontend is therefore a frontend problem, not a rewrite: the input
capture and emulation layers — the part that carries all the platform risk —
stay untouched.

## What Changes

- Add a Go frontend built on [mygo](https://github.com/egoist/mygo), using its
  native UI path: the interface is written in Go and drawn by the framework
  itself, with no webview and no HTML. On macOS and Windows this needs nothing
  from the system at build or run time; on Linux it needs the GTK 3 runtime
  libraries, which desktop distributions already have, and no development
  headers at any point.
- Make the Go frontend the default. Keep `lan-mouse-gtk` buildable behind the
  existing `gtk` feature so a working frontend remains available for
  comparison and rollback.
- Add a version and capability handshake to the frontend IPC. A client written
  in another language needs an explicit contract; today a new `FrontendEvent`
  variant silently breaks any frontend that does not know it.
- Take over the clipboard frontend work from `add-clipboard-sync`: the
  per-machine enable switch and the status line are built in the Go frontend
  instead of in GTK, so no effort goes into UI that is being replaced.
- Update the build, CI and documentation so both frontends are covered.

Out of scope, deliberately:

- Removing `lan-mouse-gtk`. It stays until the Go frontend has been used in
  practice; deleting it is a follow-up change.
- Rewriting the daemon, capture or emulation in Go. Those are the platform FFI
  layers and carry no toolkit dependency.
- Changing the peer wire protocol, the clipboard behavior, or the configuration
  format.

## Capabilities

### New Capabilities

- `desktop-frontend`: the graphical frontend a user runs to manage clients,
  settings and status, and the contract it uses to talk to the service —
  including how a frontend written in a different language negotiates with the
  service.

### Modified Capabilities

None. `clipboard-sync` is not yet archived, so its delta is adjusted directly
rather than through a second delta.

## Impact

- **New module** `frontend-mygo/` (Go module, no cgo), with its own build,
  packaging and tests.
- **`Cargo.toml`**: a new `mygo` feature that becomes part of `default`; the
  existing `gtk` feature stops being in `default`.
- **`src/main.rs`**: frontend selection gains a Go branch; the GTK branch is
  unchanged and feature-gated.
- **`lan-mouse-ipc`**: a handshake request/event pair and a protocol version
  constant. This is the frontend socket, not the peer wire protocol, so no
  peer version bump.
- **`openspec/changes/add-clipboard-sync/tasks.md`**: its two GTK frontend
  tasks are removed and recorded as moved here.
- **CI**: the Rust matrix gains a Go build/test job; the macOS runner no longer
  needs `brew install gtk4 libadwaita` unless the `gtk` feature is built.
- **Docs**: `README.md` build instructions per platform, `DOC.md` note on the
  frontend contract.
