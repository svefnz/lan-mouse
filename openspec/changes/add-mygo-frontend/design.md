# Design

## Context

See `proposal.md` for motivation and `specs/desktop-frontend/spec.md` for the
behavior contract. What shapes the approach:

- `src/main.rs` decides what to run: a subcommand (`daemon`, `cli`, test
  helpers) or, with no subcommand, the graphical frontend. Today the frontend
  branch spawns `current_exe() daemon` as a child and then runs GTK in the same
  process.
- The frontend is already a separate consumer of state, not an owner of it:
  `lan-mouse-gtk/Cargo.toml` depends only on `lan-mouse-ipc` plus GTK, and reads
  no config file and no certificate.
- The IPC is newline-delimited JSON: `FrontendRequest` in, `FrontendEvent` out,
  over a unix socket (`$XDG_RUNTIME_DIR/lan-mouse-socket.sock`, or
  `$HOME/Library/Caches/lan-mouse-socket.sock` on macOS) and over
  `127.0.0.1:5252` on Windows. It carries no version field.
- The clipboard change in flight added `FrontendEvent::ClipboardStatus` but no
  request to change the setting, so the frontend currently has no way to toggle
  clipboard sharing.
- mygo builds with `CGO_ENABLED=0` and reaches native APIs through `purego`
  (`dlopen`) on macOS and Linux and through `syscall` on Windows. Its native UI
  path draws with Metal on macOS, Direct3D 11 on Windows, and OpenGL or CPU on
  Linux. On Linux it `dlopen`s `libgtk-3.so.0`, `libgdk-3.so.0`, `libpango`,
  `libglib` and `libepoxy` at run time — a runtime dependency, never a build
  dependency.
- Go 1.27 is the toolchain mygo requires.

## Goals / Non-Goals

**Goals:**

- A graphical frontend whose build needs no system GUI toolkit on macOS and
  Windows.
- Two frontends that can coexist during the transition, sharing one protocol.
- A protocol contract explicit enough that a third implementation is possible.

**Non-Goals:**

- Not a daemon rewrite. Capture, emulation and the peer protocol stay in Rust.
- Not a UI redesign. The Go frontend reproduces the existing GTK information
  architecture.
- Not a packaging overhaul. The bundle layout changes only as far as two
  binaries require.

## Decisions

### D1 — Add the Go frontend alongside GTK, and make it the default

`lan-mouse-gtk` stays in the workspace behind the existing `gtk` feature and
stops being part of `default`; a new `mygo` feature takes its place. Removing
GTK is a follow-up change once the Go frontend has been used in practice.

Deleting GTK now was rejected: the Go frontend would then have no working
reference to compare against and no rollback path if a mygo limitation blocks
something.

### D2 — Native UI, not a webview

The interface is written in Go with mygo's `ui` package and drawn by the
framework. The webview path was rejected because on Linux it requires
WebKitGTK 4.1, which is heavier and far less likely to be preinstalled than the
GTK 3 runtime that native UI needs. On macOS and Windows both paths are
dependency-free, so Linux decides it.

Consequence worth stating plainly: "no system toolkit" holds at **build** time
on all three platforms and at **run** time only on macOS and Windows. Linux
needs GTK 3 libraries present, which desktop distributions already ship.

### D3 — `lan-mouse` stays the single entry point

With no subcommand and the `mygo` feature enabled, the Rust binary locates the
UI executable (a sibling named `lan-mouse-ui`, overridable with
`LAN_MOUSE_UI`), starts it, and waits. If the executable is missing, it logs a
warning and falls back to running the daemon directly, which is today's
`not(gtk)` behavior. With the `gtk` feature also enabled, `mygo` wins.

Making the Go binary the entry point instead was rejected: the spec says
launching lan-mouse shows the frontend, and users already run `lan-mouse`.

### D4 — The UI executable owns the daemon lifecycle

`lan-mouse-ui` mirrors what the GTK frontend does today: if
`lan_mouse_ipc::is_service_running()` says a service exists it connects,
otherwise it spawns `lan-mouse daemon` as a child, and on exit sends it SIGINT
and waits. The alternative — the Rust entry point spawning the daemon and
passing the PID to the UI — adds a channel for a value the UI can determine
itself.

### D5 — `build.rs` runs `go build`

The `mygo` feature makes the root `build.rs` run `go build` into
`target/<profile>/lan-mouse-ui`, honouring a `GO` environment override and
failing with an actionable message when Go is absent. This keeps `cargo build`
the one command that produces a complete application, as it is today.

A separate `make`/script step was rejected as a second way to build that users
would have to discover. Cross-compiling the UI is not solved here: `GOOS` and
`GOARCH` are passed through when set, and the limitation is documented.

### D6 — Protocol handshake, backward compatible

```text
FrontendRequest::Hello { version: u32, capabilities: Vec<String> }
FrontendEvent::Hello   { version: u32, capabilities: Vec<String> }
pub const IPC_PROTOCOL_VERSION: u32 = 1;
```

The service treats a `Hello` received as the first message as the negotiated
version and answers with its own. A client whose first message is anything else
is treated as a legacy client: version 0, no capabilities. That keeps the GTK
and CLI frontends working unmodified while giving the Go frontend an explicit
contract, and lets future capabilities be advertised rather than assumed.

### D7 — The Go frontend gets its own IPC client

`frontend-mygo/ipc` reimplements the transport (newline-delimited JSON, same
socket paths, same Windows TCP port) and the message types, rather than
generating them from Rust. Generating them was rejected: it would add a codegen
step and a build dependency for two small enums, and the handshake from D6
already turns a mismatch into a clear error instead of silent breakage.

The message types are therefore duplicated by hand, which is a maintenance cost
paid knowingly; the spec's "documented protocol" requirement is what keeps the
duplication honest.

### D8 — UI as a view over service state

The Go frontend holds one state struct, fed by a goroutine that reads events
from the socket, and rebuilds the view from it on each frame
(`ui.View(func(c *ui.Context))`, matching mygo's own examples). Requests are
sent from the same goroutine-safe client. No widget owns state, so reconnecting
after a service restart cannot leave stale rows behind.

### D9 — Clipboard frontend work moves here

The two unimplemented GTK tasks from `add-clipboard-sync` (enable switch, status
line) are removed from that change and implemented in the Go frontend. That
requires a request the protocol does not have yet:

```text
FrontendRequest::SetClipboardEnabled(bool)
```

which the service applies to its config and saves, mirroring `ChangePort`.

### D10 — Both binaries live in the application bundle

The macOS bundle carries `lan-mouse` and `lan-mouse-ui` in `Contents/MacOS/`.
The daemon is a child of the bundled binary, so the accessibility grant attaches
to the bundle rather than to a loose executable. This needs verification, not
assumption: it is a task, because a mis-attributed grant would force the user to
approve the daemon separately and could be lost on upgrade.

## Risks / Trade-offs

- **mygo is a young, single-author project with a moving API** → pin the module
  version, keep the framework behind a thin adapter package inside
  `frontend-mygo/`, and never let it into the daemon. A broken UI build does not
  break the service.
- **Two frontends to keep in step with protocol changes** → the handshake (D6)
  turns a mismatch into a reported error; CI builds both.
- **Hand-duplicated protocol types** → documented in `DOC.md`; the handshake is
  the safety net rather than a shared codegen step.
- **The repository becomes polyglot** → Go is confined to `frontend-mygo/`, with
  its own `go.mod`, tests and CI job. Rust builds keep working with the `gtk`
  feature if Go is unavailable.
- **Linux needs the GTK 3 runtime** → stated in the README support section and
  in the crate docs; it is a runtime dependency only.
- **Cross-compiling the UI is not solved** → `GOOS`/`GOARCH` pass-through plus a
  documented limitation; native builds are the supported path.
- **A missing `lan-mouse-ui` degrades to a headless daemon** → logged at warn
  level so the user is not left wondering why no window appeared.

## Migration Plan

- The `mygo` feature joins `default`; `gtk` leaves it. Existing users who build
  from source get the new frontend on their next build and keep the old one
  available with `--features gtk`.
- The IPC change is additive and backward compatible (D6), so an older GTK or
  CLI frontend keeps working against a newer service.
- No configuration change. The clipboard switch reuses the existing
  `[clipboard] enabled` key.
- Rollback: build with `--features gtk` and no `mygo`, or revert the commit.

## Open Questions

- Whether mygo's native UI can reproduce the macOS menu bar entry that
  `lan-mouse-gtk/src/macos_status_item.rs` provides today. It is answered by the
  first spike task; if it cannot, the frontend ships without a status item and
  that is recorded as a limitation rather than blocking the change.
