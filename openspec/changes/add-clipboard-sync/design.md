# Design

## Context

See `proposal.md` for motivation and `specs/clipboard-sync/spec.md` for the behavior contract. What shapes the approach:

- The peer protocol (`lan-mouse-proto`) is a fixed 13-byte datagram per event over DTLS/UDP. Both receive loops (`src/connect.rs`, `src/listen.rs`) read into a `[0u8; MAX_EVENT_SIZE]` buffer and treat any `recv` error as end-of-connection, so a message larger than the buffer closes the connection rather than being dropped. The protocol has no retransmission, no flow control, and no length prefix.
- Input capture and emulation are separate crates exposing a `Stream` plus an async trait, with one gated module per OS and a dummy backend. `src/service.rs` is a single-threaded `select!` loop that dispatches capture, emulation, resolver and frontend events.
- The service already emits an event exactly at the moment the cursor enters a client (`ICaptureEvent::ClientEntered`), and separately when a remote device enters this machine (`EmulationEvent::Entered`).
- Peer trust is established by certificate fingerprint against `authorized_keys`; a peer in that list may already type into this machine. A `ClientConfig` entry is only an outgoing-connection convenience, not a trust boundary.
- No clipboard code exists anywhere in the workspace today.

## Goals / Non-Goals

**Goals:**

- Carry clipboard content on a path that cannot affect input latency or the existing wire protocol.
- Preserve the full representation set, including rich content, across all supported platforms.
- Keep the trigger predictable: the user can tell why their clipboard changed and why it did not.

**Non-Goals:**

- Not a general file transfer feature; file lists carry paths only.
- Not a clipboard manager; no history, no pinning, no search.
- Not a synchronization service; no conflict resolution beyond "the most recent local change wins".

## Decisions

### D1 — Sync on screen entry, not on every change

Transfer is triggered by `ICaptureEvent::ClientEntered`. Continuous mirroring was rejected: it needs fan-out to every peer with per-peer partial failure, races with a paste happening on the receiver, and keeps every copy resident on every machine. Entry-triggered sync has exactly one destination per transfer, so partial failure and fan-out disappear.

### D2 — Gate transfers on a change counter, not on content hashing

Each machine keeps `local_seq`, incremented on every local clipboard change regardless of whether the bytes differ from before. Each peer has `last_pushed[peer]`, the `local_seq` value at the last successful push.

```
ClientEntered(B):
    if local_seq == last_pushed[B] -> skip entirely
    else                           -> transfer, then last_pushed[B] = local_seq
```

A content hash alone is not sufficient: re-copying byte-identical content must transfer again, and a hash cannot express "this changed" when the bytes match. Hashing is still used at the receiver to suppress the write-back echo.

Consequence worth stating: the policy is per machine and asymmetric. "Disabled on A" and "disabled on B" are independent decisions, and either one is enough to prevent a transfer.

### D3 — Reliable side channel over TLS on the same configured port, not chunking over DTLS

```
UDP :4242  DTLS 1.2/1.3  -> input events, 13 bytes, loss tolerant
TCP :4242  TLS 1.3       -> clipboard, length-prefixed frames, reliable and ordered
```

TCP and UDP port namespaces are independent, so both bind the same configured port with no new configuration key.

Chunking over the existing DTLS connection was rejected because it requires a reassembly state machine, retransmission, checksums, MTU tuning, and a change to `MAX_EVENT_SIZE` — which is a shared invariant whose violation currently *closes* the connection on old peers. The side channel leaves `lan-mouse-proto` untouched, so a peer running an older build simply cannot open the clipboard channel and continues to exchange input normally.

### D4 — Snapshot on change, not on entry

The clipboard watcher reads all representations and caches them in memory when the change fires, at the same moment it increments `local_seq`. Entry then performs only a hash comparison and a frame write, with no OS call.

Reading at entry was rejected: on Wayland the data must be pulled through a pipe from the source application, and on macOS lazy providers can be slow, so a read at entry can stall for hundreds of milliseconds — precisely at the moment the cursor must feel responsive. Caching also means the snapshot survives the source application exiting.

### D5 — Configurable cap, default 100 MB, degrade to smaller representations

`max_size_mb` in the `[clipboard]` section, clamped on load to a sane upper bound to prevent a typo from causing an allocation failure. The cap applies twice:

- At snapshot time: representations that would exceed the cap are not stored, only their `(type, size)` metadata, so resident memory is bounded by the cap rather than "allocated then discarded".
- At send time: the largest subset of representations that fits is transferred.

Degrading rather than failing keeps the common case useful — a huge image next to a text selection still yields pastable text.

### D6 — Canonical name mapping with raw passthrough for unknown types

Representation names differ per platform (`public.utf8-plain-text`, `text/plain;charset=utf-8`, `CF_UNICODETEXT`), so raw transfer without normalization produces content the destination cannot see. A static table maps known types to canonical names and back; types absent from the table travel under their original name with unchanged bytes and are written with the same name at the destination.

`CF_HTML` is the one format that needs active rewriting: its Windows framing embeds `StartHTML`/`EndHTML`/`StartFragment`/`EndFragment` byte offsets that must be recalculated for the destination's header length.

Restricting to a fixed set of five canonical formats was rejected as contradicting "all paste content formats": private application formats would silently disappear.

### D7 — Concealed clipboard content is filtered at the source

Checked before the snapshot is stored, so concealed content never reaches memory, the network, or the peer's clipboard. The check differs per platform: a representation type present in the list on macOS, a registered format's presence on Windows, a MIME hint on KDE. This is a correctness requirement, not a configuration option.

### D8 — Trust is the existing fingerprint whitelist

Any peer whose certificate fingerprint is in `authorized_keys` may open the clipboard channel. Requiring a `ClientConfig` entry as well was rejected because it breaks the incoming-only setup and adds a second trust boundary that duplicates the first. A peer that can already send `Ctrl+V` gains no new capability from a clipboard channel.

### D9 — File lists carry paths, not contents

`text/uri-list` is already covered by the mapping table, so this needs no extra code. Paths are only meaningful with a shared mount, but a visible broken path is a clearer signal than silent omission. Inline file transfer is a separate feature with its own concerns (temp directories, path traversal, permissions) and is excluded.

### D10 — One local switch, both directions

`[clipboard] enabled` governs sending and receiving on that machine. Splitting send and receive was rejected as an unneeded fourth state combination; per-peer switches were rejected because the fingerprint whitelist already provides the exclusion mechanism.

### D11 — Status line for degradation and progress, silence for connection failures

New `FrontendEvent` variant rendered as a small line under the device row. Notifications were rejected as too noisy for a high-frequency operation and would require three separate platform integrations. Connection failures are logged only: they are expected whenever the peer is not running a build with clipboard support.

### D12 — `clipboard-rs` behind a local trait

A new `input-clipboard` crate exposes `Clipboard` as a `Stream` of snapshots, mirroring `input-capture`/`input-emulation`, with a `clipboard-rs` implementation and a dummy fallback. Its `available_formats` / `get_buffer` / `set_buffer` API is already the raw `(type, bytes)` model this design needs, and it removes hand-written work for X11's INCR protocol, `CF_HTML`, `CF_HDROP`, and Objective-C pasteboard calls. The trait boundary keeps the dependency replaceable per platform if a backend proves inadequate.

### D13 — Hand-rolled length-prefixed frame, no serde

```
u32 frame_len (big endian)
  u64 origin_seq          <- sender's local_seq, used for echo suppression
  u16 n_items
  n_items * {
      u16 name_len, name_utf8
      u32 data_len, data
  }
```

Serializing the whole frame with `serde_json` or `bincode` would copy the payload an extra time and, for JSON, inflate binary content. Around twenty lines per side is cheaper than that.

### D14 — Connect lazily and cache the stream

The clipboard connection is opened on first need and cached per peer, invalidated on failure. Tying its lifetime to the DTLS connection was rejected: it would require adding parallel connection management to both the connector and the listener for a saving of a few milliseconds on a LAN.

### D15 — Grow the inbound buffer incrementally

The receiver appends as bytes arrive rather than reserving `frame_len` up front, and rejects a declared length above the cap before reading the body. A hostile or corrupted length field must not be able to trigger a large allocation.

### D16 — Watcher runs off the service loop

The clipboard watcher is blocking, so it runs on its own thread and communicates with the service through a channel, per the project's async rules. The service loop gains one `select!` arm; it never performs a clipboard OS call itself.

## Risks / Trade-offs

- **100 MB cap implies a large memory footprint** (snapshot plus in-flight buffer) → incremental allocation (D15), metadata-only storage over the cap (D5), and a clamped upper bound on the configured value.
- **`clipboard-rs` Wayland support is recent** → the trait boundary (D12) allows replacing one backend; the Wayland backend is validated against a real compositor before the change is considered done.
- **A paste issued while a large transfer is still in flight returns stale content** → the status line reports progress (D11); the user's recovery is to paste again. No paste interception exists on any supported platform, so this cannot be fully solved.
- **Echo loops between peers** → receiver records the expected content hash before writing and suppresses the resulting watcher event; `origin_seq` prevents re-forwarding.
- **`CF_HTML` offset rewriting is easy to get subtly wrong** → round-trip tests with a fixed header fixture, and a manual paste check into a rich text application.
- **Firewall rules may need a TCP rule for the configured port** → verified per platform during implementation; the existing rule set is UDP-focused.
- **Asymmetric policy surprises users** ("it works one way only") → the switch semantics are documented in `README.md` alongside the existing configuration reference.

## Migration Plan

- Configuration is additive. A missing `[clipboard]` section is treated as `enabled = true` and `max_size_mb = 100`, consistent with D8's position that an authorized peer gains no new capability from clipboard access. Setting `enabled = false` disables the feature without removing the section.
- No data migration. No protocol version bump: peers that predate this change cannot open the clipboard channel and exchange input exactly as before.
- Rollback: set `enabled = false`, or revert the binary. The clipboard channel is independent of the input path, so reverting does not affect input routing.

## Open Questions

- Does `clipboard-rs` on Wayland report the complete representation set for a single selection, including formats offered only on demand? If not, the Wayland backend may need `wl-clipboard-rs` directly; the trait boundary contains the change.
- Which exact type names must be probed for concealed content on each supported platform, and whether probing is cheap enough to do inside the watcher callback.
