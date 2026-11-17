package main

import (
	"fmt"
	"strings"

	"github.com/egoist/mygo/ui"

	"frontend-mygo/ipc"
)

var positionOptions = []string{"left", "right", "top", "bottom"}

// viewState is the snapshot the current frame renders.
type viewState = state

// detail is the view-local edit state for the selected client. It lives
// in the app (main thread only) and is re-synced from the service state
// whenever the committed values change.
type detail struct {
	handle uint64
	known  bool

	hostname string
	port     string
	ips      string
	pos      string

	// the service-side values the drafts were synced from
	committed struct {
		known    bool
		hostname string
		port     string
		ips      string
		pos      string
	}
}

// view builds the interface from the state, on the main thread, whenever
// the window needs a frame.
func (a *app) view(c *ui.Context) {
	st := a.snapshot()
	theme := c.Theme()

	for _, toast := range st.toasts {
		c.Toast(toast)
	}

	ui.Column(c).Fill().Padding(16).Gap(12).Children(func() {
		a.banner(c, st, theme)
		a.approval(c, st)
		a.clientSection(c, st, theme)
		ui.Divider(c)
		a.settingsSection(c, st, theme)
	})
}

// banner shows connection and port errors: transient problems are
// toasts, persistent ones (service down, port change failed) stay here.
func (a *app) banner(c *ui.Context, st viewState, theme *ui.Theme) {
	var messages []string
	if st.status != serviceConnected && st.statusMsg != "" {
		messages = append(messages, st.statusMsg)
	}
	if st.portErr != "" {
		messages = append(messages, "port change failed: "+st.portErr)
	}
	if len(messages) == 0 {
		return
	}
	ui.Column(c).Gap(4).Children(func() {
		for _, message := range messages {
			ui.Text(c, message).TextColor(theme.Danger)
		}
	})
}

// approval asks the user to accept or refuse an incoming device (task
// 6.3). It is a banner, not a modal: the service keeps refusing the
// device until a decision is made.
func (a *app) approval(c *ui.Context, st viewState) {
	if st.pendingAuth == "" {
		return
	}
	ui.Row(c).Gap(8).Children(func() {
		ui.Text(c, "device "+st.pendingAuth+" wants to connect").Grow(1)
		if ui.PrimaryButton(c.Key("approve"), "Approve").Clicked() {
			a.send(ipc.NewAuthorizeKey("device", st.pendingAuth))
			a.clearPendingAuth()
		}
		if ui.Button(c.Key("reject"), "Reject").Clicked() {
			a.clearPendingAuth()
		}
	})
}

func (a *app) clearPendingAuth() {
	a.mutate(func(s *state) { s.pendingAuth = "" })
}

// clientSection is the list of configured clients with add, delete and
// the detail editor (tasks 5.1–5.3).
func (a *app) clientSection(c *ui.Context, st viewState, theme *ui.Theme) {
	ui.Row(c).Gap(8).Children(func() {
		if ui.PrimaryButton(c.Key("add"), "Add client").Clicked() {
			a.send(ipc.NewCreate())
		}
		if st.status == serviceConnected {
			ui.Text(c, "connected").TextColor(theme.TextMuted)
		}
	})

	if len(st.clients) == 0 {
		ui.Text(c, "no clients configured yet — add one to start sharing").
			TextColor(theme.TextMuted)
	}

	ui.List(c, &a.listState, len(st.clients), func(i int) {
		entry := st.clients[i]
		a.clientRow(c, st, entry, theme)
	}).Grow(1)

	a.clientDetail(c, st, theme)
}

func (a *app) clientRow(c *ui.Context, st viewState, entry ipc.ClientEntry, theme *ui.Theme) {
	name := "(unnamed)"
	if entry.Config.Hostname != nil {
		name = *entry.Config.Hostname
	}

	ui.Row(c).Gap(8).Padding(6).Children(func() {
		// active toggle: starts and stops capture for this client
		bound := a.rowSwitches[entry.Handle]
		if bound == nil {
			value := entry.State.Active
			bound = &value
			a.rowSwitches[entry.Handle] = bound
		}
		*bound = entry.State.Active
		ui.Switch(c.Key(fmt.Sprintf("active-%d", entry.Handle)), bound)
		if *bound != entry.State.Active {
			// the user flipped it this frame: keep it shown and tell the
			// service; its echo clears the override
			a.setActiveOverride(entry.Handle, *bound)
			a.send(ipc.NewActivate(entry.Handle, *bound))
		}

		ui.Column(c).Grow(1).Gap(2).Children(func() {
			ui.Text(c, name)
			ui.Text(c, clientSummary(entry)).FontSize(12).TextColor(theme.TextMuted)
		})

		if ui.Button(c.Key(fmt.Sprintf("edit-%d", entry.Handle)), "Edit").Clicked() {
			a.selectClient(entry.Handle)
		}
	})
}

// clientSummary is the one-line description of a client: addresses,
// port and position (task 5.1).
func clientSummary(entry ipc.ClientEntry) string {
	var addrs []string
	seen := map[string]bool{}
	add := func(list []string) {
		for _, addr := range list {
			if addr != "" && !seen[addr] {
				seen[addr] = true
				addrs = append(addrs, addr)
			}
		}
	}
	if entry.State.ActiveAddr != nil {
		add([]string{*entry.State.ActiveAddr})
	}
	add(entry.Config.FixIps)
	add(entry.State.DnsIps)
	add(entry.State.Ips)
	summary := strings.Join(addrs, ", ")
	if summary == "" {
		summary = "no addresses yet"
	}
	return fmt.Sprintf("%s · port %d · %s", summary, entry.Config.Port, entry.Config.Pos)
}

// selectClient makes handle the client shown in the detail editor.
func (a *app) selectClient(handle uint64) {
	a.detail.handle = handle
	a.detail.known = true
	a.detail.committed.known = false // force a draft sync on the next frame
}

// clientDetail renders the editor for the selected client: hostname,
// port, position, fixed addresses and the active state (task 5.3).
func (a *app) clientDetail(c *ui.Context, st viewState, theme *ui.Theme) {
	if !a.detail.known {
		return
	}
	entry := st.clientByHandle(a.detail.handle)
	if entry == nil {
		a.detail.known = false
		return
	}

	a.syncDetailDrafts(*entry)

	ui.Column(c).Gap(8).Padding(12).Children(func() {
		ui.Text(c, "edit client").Bold()

		ui.Row(c).Gap(8).Children(func() {
			ui.Text(c, "hostname").Width(120)
			if ui.TextInput(c.Key("detail-hostname"), &a.detail.hostname).
				Placeholder("hostname…").Grow(1).Submitted() {
				var hostname *string
				if strings.TrimSpace(a.detail.hostname) != "" {
					hostname = &a.detail.hostname
				}
				a.send(ipc.NewUpdateHostname(a.detail.handle, hostname))
				a.detail.committed.hostname = a.detail.hostname
			}
		})

		ui.Row(c).Gap(8).Children(func() {
			ui.Text(c, "port").Width(120)
			if ui.TextInput(c.Key("detail-port"), &a.detail.port).Submitted() {
				if port, ok := parsePort(a.detail.port); ok {
					a.send(ipc.NewUpdatePort(a.detail.handle, port))
					a.detail.committed.port = a.detail.port
				}
			}
			ui.Text(c, "position").Width(80)
			ui.Select(c.Key("detail-pos"), &a.detail.pos, positionOptions)
			if a.detail.pos != a.detail.committed.pos {
				a.send(ipc.NewUpdatePosition(a.detail.handle, ipc.Position(a.detail.pos)))
				a.detail.committed.pos = a.detail.pos
			}
		})

		ui.Row(c).Gap(8).Children(func() {
			ui.Text(c, "fixed ips").Width(120)
			if ui.TextInput(c.Key("detail-ips"), &a.detail.ips).
				Placeholder("comma separated, e.g. 192.168.1.10, 192.168.1.11").Grow(1).Submitted() {
				a.send(ipc.NewUpdateFixIps(a.detail.handle, parseIps(a.detail.ips)))
				a.detail.committed.ips = a.detail.ips
			}
		})

		ui.Row(c).Gap(8).Children(func() {
			if ui.Button(c.Key("resolve"), "Resolve dns").Clicked() {
				a.send(ipc.NewResolveDns(a.detail.handle))
			}
			if ui.Button(c.Key("delete"), "Delete").Clicked() {
				a.send(ipc.NewDelete(a.detail.handle))
				a.detail.known = false
			}
		})
	})
}

// syncDetailDrafts copies the service's committed values into the edit
// drafts whenever they changed.
func (a *app) syncDetailDrafts(entry ipc.ClientEntry) {
	hostname := ""
	if entry.Config.Hostname != nil {
		hostname = *entry.Config.Hostname
	}
	values := [4]string{
		hostname,
		fmt.Sprintf("%d", entry.Config.Port),
		strings.Join(entry.Config.FixIps, ", "),
		string(entry.Config.Pos),
	}
	committed := &a.detail.committed
	changed := !committed.known ||
		values[0] != committed.hostname ||
		values[1] != committed.port ||
		values[2] != committed.ips ||
		values[3] != committed.pos
	if !changed {
		return
	}
	a.detail.hostname, a.detail.port, a.detail.ips, a.detail.pos = values[0], values[1], values[2], values[3]
	committed.known = true
	committed.hostname, committed.port, committed.ips, committed.pos = values[0], values[1], values[2], values[3]
}

// settingsSection shows and changes the service settings (tasks 6.1,
// 6.2, 7.1 and 7.2).
func (a *app) settingsSection(c *ui.Context, st viewState, theme *ui.Theme) {
	ui.Text(c, "settings").Bold()

	// listen port
	ui.Row(c).Gap(8).Children(func() {
		ui.Text(c, "listen port").Width(120)
		committed := fmt.Sprintf("%d", st.port)
		if a.portCommitted != committed {
			a.portDraft = committed
			a.portCommitted = committed
		}
		if ui.TextInput(c.Key("listen-port"), &a.portDraft).Width(120).Submitted() {
			if port, ok := parsePort(a.portDraft); ok {
				a.send(ipc.NewChangePort(port))
				a.portCommitted = a.portDraft
			}
		}
	})

	// capture and emulation status
	ui.Row(c).Gap(16).Children(func() {
		ui.Text(c, "capture: "+st.captureStatus).TextColor(theme.TextMuted)
		ui.Text(c, "emulation: "+st.emulationStatus).TextColor(theme.TextMuted)
	})

	// clipboard sharing switch, bound to SetClipboardEnabled: adopt the
	// service's value when it changes, send the request when the user
	// flips, and keep the flipped value shown until the echo arrives
	if st.clipboardOverride == nil &&
		(st.clipboardKnown != a.clipboardKnown || st.clipboardEnabled != a.clipboardSyncedValue) {
		a.clipboardSwitch = st.clipboardEnabled
		a.clipboardKnown = st.clipboardKnown
		a.clipboardSyncedValue = st.clipboardEnabled
	}
	ui.Row(c).Gap(8).Children(func() {
		ui.Text(c, "clipboard sharing").Width(120)
		ui.Switch(c.Key("clipboard"), &a.clipboardSwitch)
	})
	if st.clipboardOverride == nil && a.clipboardSwitch != a.clipboardSyncedValue {
		a.send(ipc.NewSetClipboardEnabled(a.clipboardSwitch))
		a.setClipboardOverride(a.clipboardSwitch)
		a.clipboardSyncedValue = a.clipboardSwitch
	}
	if st.clipboardStatus != "" {
		ui.Text(c, st.clipboardStatus).TextColor(theme.TextMuted)
	}

	ui.Text(c, "this device: "+fingerprintOrNone(st.publicKey)).TextColor(theme.TextMuted)

	// authorized devices
	ui.Text(c, "authorized devices")
	for _, entry := range st.authEntries() {
		ui.Row(c).Gap(8).Children(func() {
			ui.Text(c, entry.description+" — "+entry.fingerprint).Grow(1)
			if ui.Button(c.Key("remove-"+entry.fingerprint), "Remove").Clicked() {
				a.send(ipc.NewRemoveAuthorizedKey(entry.fingerprint))
			}
		})
	}

	// authorize a device by hand
	ui.Row(c).Gap(8).Children(func() {
		ui.TextInput(c.Key("auth-desc"), &a.authDescription).Placeholder("description…").Width(160)
		ui.TextInput(c.Key("auth-fp"), &a.authFingerprint).Placeholder("fingerprint…").Grow(1)
		if ui.Button(c.Key("authorize"), "Authorize").Clicked() {
			fingerprint := strings.TrimSpace(a.authFingerprint)
			if fingerprint != "" {
				a.send(ipc.NewAuthorizeKey(strings.TrimSpace(a.authDescription), fingerprint))
				a.authDescription = ""
				a.authFingerprint = ""
			}
		}
	})
}

func fingerprintOrNone(fingerprint string) string {
	if fingerprint == "" {
		return "unknown yet"
	}
	return fingerprint
}

// setActiveOverride keeps a flipped switch shown until the service
// echoes the new state.
func (a *app) setActiveOverride(handle uint64, active bool) {
	a.mutate(func(s *state) { s.activeOverride[handle] = active })
}

// setClipboardOverride keeps a flipped clipboard switch shown until the
// service echoes the new state.
func (a *app) setClipboardOverride(enabled bool) {
	a.mutate(func(s *state) { s.clipboardOverride = &enabled })
}

func parsePort(s string) (uint16, bool) {
	var port int
	_, err := fmt.Sscanf(strings.TrimSpace(s), "%d", &port)
	if err != nil || port < 1 || port > 65535 {
		return 0, false
	}
	return uint16(port), true
}

// parseIps splits a comma or space separated address list.
func parseIps(s string) []string {
	fields := strings.FieldsFunc(s, func(r rune) bool {
		return r == ',' || r == ' ' || r == '\t'
	})
	var ips []string
	for _, field := range fields {
		if trimmed := strings.TrimSpace(field); trimmed != "" {
			ips = append(ips, trimmed)
		}
	}
	return ips
}
