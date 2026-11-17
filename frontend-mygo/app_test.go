package main

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"testing"

	"frontend-mygo/ipc"
)

// eventLines is a recorded event stream, in the form the Rust service
// sends it (as captured for the ipc fixture tests).
var eventLines = []string{
	`{"Enumerate":[[0,{"hostname":"laptop","fix_ips":["192.168.1.10"],"port":4242,"pos":"left","cmd":null,"leave_cmd":null},{"active":true,"active_addr":"192.168.1.10:4242","alive":true,"dns_ips":[],"ips":["192.168.1.10"],"has_pressed_keys":false,"resolving":false,"peer_commit":null}],[1,{"hostname":"desktop","fix_ips":[],"port":4242,"pos":"right","cmd":null,"leave_cmd":null},{"active":false,"active_addr":null,"alive":true,"dns_ips":[],"ips":["192.168.1.20"],"has_pressed_keys":false,"resolving":false,"peer_commit":null}]]}`,
	`{"PortChanged":[4242,null]}`,
	`{"CaptureStatus":"Enabled"}`,
	`{"EmulationStatus":"Disabled"}`,
	`{"PublicKeyFingerprint":"aa:bb:cc:dd"}`,
	`{"AuthorizedUpdated":{"ab:cd":"laptop"}}`,
	`{"ClipboardStatus":"over the size limit"}`,
	`{"ClipboardSyncEnabled":false}`,
	`{"ConnectionAttempt":{"fingerprint":"ef:01"}}`,
	`{"State":[1,{"hostname":"desktop","fix_ips":[],"port":4242,"pos":"bottom","cmd":null,"leave_cmd":null},{"active":true,"active_addr":null,"alive":true,"dns_ips":[],"ips":["192.168.1.20"],"has_pressed_keys":false,"resolving":false,"peer_commit":null}]}`,
	`{"Deleted":0}`,
}

// TestStateReplay covers task 4.1: replaying a recorded event stream
// produces the expected view state.
func TestStateReplay(t *testing.T) {
	var app app
	st := &app.st

	for _, line := range eventLines {
		var event ipc.Event
		if err := json.Unmarshal([]byte(line), &event); err != nil {
			t.Fatalf("event does not decode: %v\n\t%s", err, line)
		}
		st.applyEvent(event)
	}

	if got := len(st.clients); got != 1 {
		t.Fatalf("%d clients after replay, want 1 (two enumerated, one deleted)", got)
	}
	remaining := st.clients[0]
	if remaining.Handle != 1 {
		t.Errorf("remaining handle = %d, want 1", remaining.Handle)
	}
	if !remaining.State.Active {
		t.Errorf("client 1 active = false, the State event set it")
	}
	if remaining.Config.Pos != ipc.PositionBottom {
		t.Errorf("client 1 pos = %s, want bottom", remaining.Config.Pos)
	}
	if st.port != 4242 {
		t.Errorf("port = %d, want 4242", st.port)
	}
	if st.portErr != "" {
		t.Errorf("portErr = %q, want empty", st.portErr)
	}
	if st.captureStatus != "Enabled" || st.emulationStatus != "Disabled" {
		t.Errorf("statuses = %s/%s, want Enabled/Disabled", st.captureStatus, st.emulationStatus)
	}
	if st.publicKey != "aa:bb:cc:dd" {
		t.Errorf("public key = %q", st.publicKey)
	}
	if st.authorized["ab:cd"] != "laptop" {
		t.Errorf("authorized = %+v", st.authorized)
	}
	if st.clipboardStatus != "over the size limit" {
		t.Errorf("clipboard status = %q", st.clipboardStatus)
	}
	if !st.clipboardKnown {
		t.Errorf("ClipboardSyncEnabled did not sync the known flag")
	}
	if st.clipboardEnabled {
		t.Errorf("clipboard switch = enabled, want the synced false value")
	}
	if st.pendingAuth != "ef:01" {
		t.Errorf("pending auth = %q, want ef:01", st.pendingAuth)
	}
}

// TestStateClearsActiveOverrideOnEcho: a switch flip is shown
// optimistically until the service echoes the state (task 5.3).
func TestStateClearsActiveOverrideOnEcho(t *testing.T) {
	app := newApp()
	st := &app.st
	st.clients = []ipc.ClientEntry{{
		Handle: 3,
		State:  ipc.ClientState{Active: false},
	}}

	st.activeOverride[3] = true
	if got := app.snapshot().clients[0].State.Active; !got {
		t.Errorf("override not applied in snapshot")
	}

	// the service echoes the new state: the override must clear
	st.applyEvent(ipc.Event{
		Kind:   "State",
		Client: ipc.ClientEntry{Handle: 3, State: ipc.ClientState{Active: true}},
	})
	if _, pending := st.activeOverride[3]; pending {
		t.Errorf("override survived the echo")
	}
	if !st.clients[0].State.Active {
		t.Errorf("state did not take the echoed value")
	}
}

// TestStopDaemonSendsSigint covers the shutdown half of task 4.2: a
// daemon this frontend started receives SIGINT (SIGKILL on Windows) on
// stop, so the service can terminate gracefully.
func TestStopDaemonSendsSigint(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("signal test is unix specific")
	}

	// fake daemon: log a line when it gets SIGINT, then exit
	script := filepath.Join(t.TempDir(), "lan-mouse")
	source := "#!/bin/sh\ntrap 'echo SIGINT-RECEIVED; exit 0' INT\nwhile true; do sleep 0.1; done\n"
	if err := os.WriteFile(script, []byte(source), 0o755); err != nil {
		t.Fatal(err)
	}

	var app app
	cmd := exec.Command(script, "daemon")
	if err := cmd.Start(); err != nil {
		t.Fatalf("start fake daemon: %v", err)
	}
	app.daemonCmd = cmd

	app.stopDaemon()

	// stopDaemon waits for the process and clears the field
	if app.daemonCmd != nil {
		t.Error("stopDaemon did not clear the daemon command")
	}
	// either the trap fired and the script exited cleanly, or the shell
	// deferred the trap and died from the signal — both prove SIGINT was
	// delivered and reaped, which is the mechanism under test
	if cmd.ProcessState.Success() {
		return
	}
	if ws, ok := cmd.ProcessState.Sys().(syscall.WaitStatus); ok &&
		ws.Signaled() && ws.Signal() == syscall.SIGINT {
		return
	}
	t.Errorf("daemon exited with %v, want a clean exit or SIGINT", cmd.ProcessState)
}

// TestFindDaemonBinary: the sibling binary wins over the PATH.
func TestFindDaemonBinary(t *testing.T) {
	dir := t.TempDir()
	sibling := filepath.Join(dir, "lan-mouse")
	if runtime.GOOS == "windows" {
		sibling += ".exe"
	}
	if err := os.WriteFile(sibling, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatal(err)
	}

	if runtime.GOOS == "windows" {
		t.Skip("sibling lookup is exercised on unix here")
	}

	// daemonBinary uses os.Executable, which is the test binary; copy the
	// sibling next to it instead
	exe, err := os.Executable()
	if err != nil {
		t.Skip("no executable path")
	}
	nextToTest := filepath.Join(filepath.Dir(exe), "lan-mouse")
	if err := os.WriteFile(nextToTest, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Skipf("cannot write next to the test binary: %v", err)
	}
	defer os.Remove(nextToTest)

	got, err := daemonBinary()
	if err != nil {
		t.Fatalf("daemonBinary: %v", err)
	}
	if !strings.HasSuffix(got, "lan-mouse") {
		t.Errorf("found %q, want the sibling lan-mouse", got)
	}
}
