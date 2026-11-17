# frontend-mygo

The Go frontend for lan-mouse (`lan-mouse-ui`), built on
[mygo](https://github.com/egoist/mygo)'s native UI path: the interface is
written in Go and drawn by the framework itself, with no webview and no
HTML.

## Status

Part of the `add-mygo-frontend` change. Implemented so far:

- `ipc/` — the frontend protocol client: transport (unix socket on
  macOS/Linux, TCP on Windows), newline-delimited JSON, the version
  handshake and all request/event types, transcribed from the
  "Frontend Protocol (local IPC)" section of [DOC.md](../DOC.md). The
  wire forms are pinned by fixture tests captured from the Rust
  service's own serde output (`ipc/testdata/`).
- `app.go` — the state the window shows, folded purely from service
  events; the connection goroutine (connect-or-spawn, handshake, event
  pump, reconnect after a service exit) and the daemon lifecycle
  (`lan-mouse daemon` as a child process, SIGINT on exit).
- `view.go` — the interface: client list with per-client active
  switches, add/delete, a detail editor (hostname, port, position,
  fixed ips, dns resolve), listen port, capture/emulation status,
  fingerprint display, authorized-device management, the incoming-device
  approval banner, and the clipboard sharing switch plus status line.

## Building

Requires Go ≥ 1.27.3. No cgo, no system GUI toolkit at build time:

```sh
CGO_ENABLED=0 go build -o lan-mouse-ui .
CGO_ENABLED=0 go vet ./...
CGO_ENABLED=0 go test ./...
```

`cargo build` with the `mygo` feature (default) runs this build
automatically and places `lan-mouse-ui` next to the `lan-mouse` binary.

On Linux the mygo runtime `dlopen`s the GTK 3 libraries at run time
(`libgtk-3.so.0` and friends) — a runtime dependency, never a build
dependency. On macOS and Windows nothing from the system is needed at
build or run time.

## Spike (task 2.2)

`main.go` is the framework spike requested by the change: one window
containing

- a list with rows (`ui.List`),
- a text entry (`ui.TextInput`),
- a toggle (`ui.Switch`),
- a button (`ui.PrimaryButton`),
- a menu (`ui.MenuButton`),

plus a menu bar tray item (`mygo.NewTray`).

Run it with `CGO_ENABLED=0 go run .` and interact with the window.

### macOS menu bar status item — answered

The design's open question was whether mygo's native UI can reproduce
the macOS menu bar entry that `lan-mouse-gtk/src/macos_status_item.rs`
provides today.

**It can.** `mygo.NewTray(mygo.TrayOptions{...})` creates a menu bar
item (the macOS equivalent of an `NSStatusItem`) with:

- an icon (`Icon`, with `IconIsTemplate` for the menu bar tint),
- a text title next to the icon (`Title`),
- a tooltip,
- a menu shown on click (`Menu`, built from `mygo.NewMenu` with items
  that carry `Click` callbacks), e.g. *Show window* / *Quit*.

Verified on macOS 15 (arm64, Go 1.27.4, mygo v0.9.2, `CGO_ENABLED=0`):
the tray is created without error and the app runs cleanly; no
"tray unavailable" fallback is logged. The lan-mouse menu bar item is
reachable the same way the GTK one is, and the window can be shown and
focused from the tray menu.

### mygo API notes gathered by the spike

- Views are plain functions of app state:
  `func (s *state) view(c *ui.Context)`, attached as
  `Content: ui.View(s.view)`. The view rebuilds on each frame; there is
  no widget-owned state, which matches the design's "UI as a view over
  service state" (design D8).
- Widgets are builder-chained: `ui.Text(c, "...").FontSize(12)`,
  `ui.Row(c).Gap(8).Children(func() { ... })`.
- Buttons report clicks inline in the view:
  `if ui.PrimaryButton(c, "Add").Clicked() { ... }`; menu items the same
  way: `if m.Item("Clear").Chosen() { ... }`.
- `c.Key("id")` scopes an element's identity so rows and inputs keep
  their state across rebuilds.
- `ui.List(c, &ui.ListState{}, len(rows), rowBuilder)` renders a
  (virtualized) list; `ListState.Key` can stabilize rows by identity.
- Text entry: `ui.TextInput(c, &s.query).Placeholder("...")` binds
  directly to a `*string`; commits are detected with `.Submitted()`.
- The window redraws only when something changes; a goroutine that
  mutated state calls `window.Invalidate()` to request a frame.

## Layout

- `ipc/` — protocol client (see above)
- `app.go` — state, connection goroutine, daemon lifecycle
- `view.go` — the interface
- `main.go` — entry point: window, tray, exit handling

## Verification log (add-mygo-frontend)

Verified end to end on macOS 15 (arm64, Go 1.27.4, mygo v0.9.2), against
the real daemon in an isolated `HOME` unless noted:

- Client list renders exactly what `config.toml` configures (three
  clients: hostname, addresses, port, position).
- Add and delete through the UI write the new/removed entry to
  `config.toml`.
- Editing hostname, port, fixed ips and position in the detail editor
  commits to `config.toml` (and the position change re-creates the
  capture zone service-side); the per-row active switch activates and
  deactivates capture; DNS resolution shows up in the row.
- The listen port and capture/emulation status are shown; a port change
  submits the right `ChangePort` request (verified against a scripted
  service — completing the change service-side needs the emulation
  backend, i.e. accessibility, on macOS).
- The device fingerprint is displayed; authorizing a device by hand and
  removing it via the UI updates `authorized_fingerprints` in
  `config.toml`.
- The incoming-device banner appears on a `ConnectionAttempt`; Approve
  sends `AuthorizeKey`, Reject sends nothing (verified against a
  scripted service).
- The clipboard sharing switch starts and stops clipboard sharing at
  runtime and writes `[clipboard] enabled` both ways.
- An over-limit copy shows the degradation message in the clipboard
  status line; the next successful change clears it.
- Connect-or-spawn: connects to a running service, spawns
  `lan-mouse daemon` when none runs, reconnects after a service exit,
  and SIGINTs the daemon it started on exit — no process left behind.
- The `mygo` menu bar tray item works (the design's open question).

### macOS bundle

`scripts/bundle-macos.sh` builds `Lan Mouse.app` with both binaries in
`Contents/MacOS/`, ad hoc signed, and no bundled GUI toolkit dylibs.
The accessibility grant attaches to the bundle because the daemon runs
as a child of the bundled binary; verifying the grant (System
Preferences → the bundle, surviving an in-place rebuild) is an
interactive TCC step and has not been automated.
