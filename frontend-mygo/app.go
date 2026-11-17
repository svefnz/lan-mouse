package main

import (
	"errors"
	"fmt"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"sync"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"

	"frontend-mygo/ipc"
)

// serviceStatus is the state of the connection to the lan-mouse service.
type serviceStatus int

const (
	serviceConnecting serviceStatus = iota
	serviceConnected
	serviceLost
)

// state is everything the window shows. It is guarded by app.mu and
// rebuilt from service events only: no widget owns state (design D8).
type state struct {
	status    serviceStatus
	statusMsg string // why the service is unreachable, or an incompatibility

	clients []ipc.ClientEntry
	port    uint16
	portErr string

	captureStatus   string
	emulationStatus string

	publicKey string

	authorized map[string]string

	clipboardEnabled bool
	clipboardKnown   bool // ClipboardSyncEnabled has arrived
	clipboardStatus  string

	// fingerprint awaiting the user's approval, "" when none
	pendingAuth string

	// optimistic overrides for switches the user just flipped and the
	// service has not echoed yet; cleared when the echo arrives
	activeOverride    map[uint64]bool
	clipboardOverride *bool

	// transient, non-modal messages the next frame shows as toasts
	toasts []string
}

// upsertClient replaces the entry with the same handle or appends it,
// keeping the list ordered by handle.
func (s *state) upsertClient(entry ipc.ClientEntry) {
	for i := range s.clients {
		if s.clients[i].Handle == entry.Handle {
			s.clients[i] = entry
			s.sortClients()
			return
		}
	}
	s.clients = append(s.clients, entry)
	s.sortClients()
}

func (s *state) sortClients() {
	sort.SliceStable(s.clients, func(i, j int) bool {
		return s.clients[i].Handle < s.clients[j].Handle
	})
}

func (s *state) clientByHandle(handle uint64) *ipc.ClientEntry {
	for i := range s.clients {
		if s.clients[i].Handle == handle {
			return &s.clients[i]
		}
	}
	return nil
}

func (s *state) removeClient(handle uint64) {
	clients := s.clients[:0]
	for _, client := range s.clients {
		if client.Handle != handle {
			clients = append(clients, client)
		}
	}
	s.clients = clients
}

// toast queues a transient, non-modal message.
func (s *state) toast(message string) {
	s.toasts = append(s.toasts, message)
}

// applyEvent folds one service event into the state.
func (s *state) applyEvent(event ipc.Event) {
	switch event.Kind {
	case "Created", "State":
		s.upsertClient(event.Client)
		// the echo of a switch flip clears its optimistic override
		if override, ok := s.activeOverride[event.Client.Handle]; ok {
			if override == event.Client.State.Active {
				delete(s.activeOverride, event.Client.Handle)
			}
		}
	case "Enumerate":
		s.clients = event.Clients
		s.activeOverride = map[uint64]bool{}
	case "Deleted":
		s.removeClient(event.Handle)
		delete(s.activeOverride, event.Handle)
	case "NoSuchClient":
		// a stale handle; nothing to show
	case "PortChanged":
		s.port = event.Port
		if event.PortError != nil {
			s.portErr = *event.PortError
		} else {
			s.portErr = ""
		}
	case "CaptureStatus":
		s.captureStatus = event.Status
	case "EmulationStatus":
		s.emulationStatus = event.Status
	case "AuthorizedUpdated":
		s.authorized = event.AuthorizedKeys
	case "PublicKeyFingerprint":
		s.publicKey = event.Fingerprint
	case "DeviceConnected":
		s.toast(fmt.Sprintf("device connected: %s", event.Addr))
	case "DeviceEntered":
		s.toast(fmt.Sprintf("device entered: %s (%s)", event.Addr, event.Pos))
	case "IncomingDisconnected":
		s.toast(fmt.Sprintf("%s disconnected", event.Addr))
	case "ConnectionAttempt":
		s.pendingAuth = event.Fingerprint
	case "ClipboardStatus":
		if event.ClipboardMsg != nil {
			s.clipboardStatus = *event.ClipboardMsg
		} else {
			s.clipboardStatus = ""
		}
	case "ClipboardSyncEnabled":
		s.clipboardEnabled = event.ClipboardSyncEnabled != nil && *event.ClipboardSyncEnabled
		s.clipboardKnown = true
		if s.clipboardOverride != nil && *s.clipboardOverride == s.clipboardEnabled {
			s.clipboardOverride = nil
		}
	case "Error":
		s.toast("error: " + event.Message)
	case "Hello":
		// consumed by the handshake
	}
}

// authEntries returns the authorized fingerprints sorted by fingerprint
// so the list does not jump around between frames.
type authEntry struct {
	fingerprint string
	description string
}

func (s *state) authEntries() []authEntry {
	entries := make([]authEntry, 0, len(s.authorized))
	for fingerprint, description := range s.authorized {
		entries = append(entries, authEntry{fingerprint, description})
	}
	sort.SliceStable(entries, func(i, j int) bool {
		return entries[i].fingerprint < entries[j].fingerprint
	})
	return entries
}

// app owns the connection to the service, the daemon it may have
// started, and the state the window shows.
type app struct {
	mu sync.Mutex
	st state

	window *mygo.Window

	// connection; reads happen on the pump goroutine only
	conn *ipc.Conn

	// daemon lifecycle
	daemonMu  sync.Mutex
	daemonCmd *exec.Cmd

	// view-owned state (main thread only): per-row switch bindings,
	// edit drafts and the switch values the service has confirmed
	listState            ui.ListState
	rowSwitches          map[uint64]*bool
	detail               detail
	portDraft            string
	portCommitted        string
	clipboardSwitch      bool
	clipboardKnown       bool
	clipboardSyncedValue bool
	authDescription      string
	authFingerprint      string
}

func newApp() *app {
	return &app{
		rowSwitches: map[uint64]*bool{},
		st:          state{activeOverride: map[uint64]bool{}},
	}
}

// snapshot copies the state for the view, applying optimistic overrides
// and handing the queued toasts to the caller.
func (a *app) snapshot() state {
	a.mu.Lock()
	defer a.mu.Unlock()
	s := a.st
	s.toasts = a.st.toasts
	a.st.toasts = nil
	for i := range s.clients {
		if active, ok := s.activeOverride[s.clients[i].Handle]; ok {
			s.clients[i].State.Active = active
		}
	}
	if s.clipboardOverride != nil {
		s.clipboardEnabled = *s.clipboardOverride
	}
	return s
}

// mutate applies fn to the state and requests a redraw.
func (a *app) mutate(fn func(s *state)) {
	a.mu.Lock()
	fn(&a.st)
	a.mu.Unlock()
	a.invalidate()
}

// setWindow stores the window the event pump invalidates.
func (a *app) setWindow(window *mygo.Window) {
	a.mu.Lock()
	a.window = window
	a.mu.Unlock()
}

func (a *app) invalidate() {
	if a.window != nil {
		a.window.Invalidate()
	}
}

// send writes a request to the service; safe from any goroutine.
func (a *app) send(r ipc.Request) {
	a.mu.Lock()
	conn := a.conn
	a.mu.Unlock()
	if conn == nil {
		return
	}
	if err := conn.Send(r); err != nil {
		log.Printf("send %s failed: %v", r, err)
	}
}

// runConnection drives the connection for the lifetime of the app:
// connect (starting a service first if none is running), handshake,
// pump events, and reconnect after a service exit (task 4.3).
func (a *app) runConnection(done <-chan struct{}) {
	for {
		select {
		case <-done:
			return
		default:
		}

		conn, spawned, err := a.connectOrSpawn()
		if err != nil {
			a.mutate(func(s *state) {
				s.status = serviceConnecting
				s.statusMsg = err.Error()
			})
			if spawned {
				a.stopDaemon()
			}
			waitOrDone(done, time.Second)
			continue
		}

		hello, before, err := conn.Handshake(nil, 10*time.Second)
		if err != nil {
			conn.Close()
			if spawned {
				a.stopDaemon()
			}
			if isVersionError(err) {
				// retrying cannot help: the service speaks another version
				a.mutate(func(s *state) {
					s.status = serviceLost
					s.statusMsg = err.Error()
				})
				<-done
				return
			}
			a.mutate(func(s *state) {
				s.status = serviceConnecting
				s.statusMsg = "handshake failed: " + err.Error()
			})
			waitOrDone(done, time.Second)
			continue
		}
		_ = hello

		a.mu.Lock()
		a.conn = conn
		a.mu.Unlock()

		a.mutate(func(s *state) {
			s.status = serviceConnected
			s.statusMsg = ""
			for _, event := range before {
				s.applyEvent(event)
			}
			// the fresh sync is authoritative
			s.activeOverride = map[uint64]bool{}
			s.clipboardOverride = nil
		})

		// pump events until the service goes away
		for {
			event, err := conn.NextEvent()
			if err != nil {
				break
			}
			a.mutate(func(s *state) { s.applyEvent(event) })
			if event.Kind == "PortChanged" && event.PortError == nil {
				// the service does not persist the port itself; save the
				// config now that the new port is confirmed
				a.send(ipc.NewSaveConfiguration())
			}
		}

		conn.Close()
		a.mu.Lock()
		a.conn = nil
		a.mu.Unlock()

		a.mutate(func(s *state) {
			s.status = serviceLost
			s.statusMsg = "the lan-mouse service exited, waiting for it to come back …"
		})
		// the daemon we started may have died; connectOrSpawn starts a
		// fresh one when the socket is gone
	}
}

func isVersionError(err error) bool {
	return errors.Is(err, ipc.ErrorIncompatibleVersion)
}

// connectOrSpawn connects to a running service, or starts one and waits
// for its socket (task 4.2).
func (a *app) connectOrSpawn() (conn *ipc.Conn, spawned bool, err error) {
	if ipc.IsServiceRunning() {
		conn, err = ipc.Connect()
		return conn, false, err
	}

	if err := a.startDaemon(); err != nil {
		return nil, false, fmt.Errorf("no service running and it could not be started: %w", err)
	}
	spawned = true

	// wait for the service socket to come online
	deadline := time.Now().Add(15 * time.Second)
	for !ipc.IsServiceRunning() {
		if time.Now().After(deadline) {
			return nil, spawned, fmt.Errorf("the service did not come online")
		}
		time.Sleep(100 * time.Millisecond)
	}
	conn, err = ipc.Connect()
	if err != nil {
		return nil, spawned, err
	}
	return conn, spawned, nil
}

// daemonBinary locates the lan-mouse binary: next to this executable
// first, then on the PATH.
func daemonBinary() (string, error) {
	name := "lan-mouse"
	if runtime.GOOS == "windows" {
		name = "lan-mouse.exe"
	}
	if exe, err := os.Executable(); err == nil {
		sibling := filepath.Join(filepath.Dir(exe), name)
		if info, err := os.Stat(sibling); err == nil && !info.IsDir() {
			return sibling, nil
		}
	}
	return exec.LookPath(name)
}

// startDaemon spawns `lan-mouse daemon` as a child process.
func (a *app) startDaemon() error {
	a.daemonMu.Lock()
	defer a.daemonMu.Unlock()
	if a.daemonCmd != nil {
		if a.daemonCmd.ProcessState == nil {
			// still running from before
			return nil
		}
		a.daemonCmd = nil
	}
	binary, err := daemonBinary()
	if err != nil {
		return fmt.Errorf("cannot find the lan-mouse binary: %w", err)
	}
	cmd := exec.Command(binary, "daemon")
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("cannot start %s daemon: %w", binary, err)
	}
	log.Printf("started daemon: %s", binary)
	a.daemonCmd = cmd
	return nil
}

// stopDaemon asks a daemon we started to terminate, with SIGINT so the
// service can shut down gracefully (task 4.2).
func (a *app) stopDaemon() {
	a.daemonMu.Lock()
	cmd := a.daemonCmd
	a.daemonCmd = nil
	a.daemonMu.Unlock()
	if cmd == nil || cmd.Process == nil {
		return
	}
	if runtime.GOOS == "windows" {
		_ = cmd.Process.Kill()
	} else {
		_ = cmd.Process.Signal(os.Interrupt)
	}
	_ = cmd.Wait()
}

// shutdown stops the daemon we started, if any.
func (a *app) shutdown() {
	a.stopDaemon()
}

func waitOrDone(done <-chan struct{}, d time.Duration) {
	select {
	case <-done:
	case <-time.After(d):
	}
}
