package ipc

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"runtime"
	"strings"
	"testing"
	"time"
)

// fakeServer listens on the platform transport (a unix socket in a
// temporary directory, or an ephemeral TCP endpoint on Windows) and
// answers every line with the lines the handler returns.
type fakeServer struct {
	network string
	address string
	close   func()
}

// lineHandler processes one request line and returns the reply lines.
type lineHandler func(line string) []string

func startFakeServer(t *testing.T, handler lineHandler) *fakeServer {
	t.Helper()
	var listener net.Listener
	var network, address string
	switch runtime.GOOS {
	case "windows":
		network = "tcp"
		address = "127.0.0.1:0"
		listener = mustListen(t, network, address)
	default:
		network = "unix"
		// short path: unix socket addresses are limited to ~104 bytes and
		// test temp dirs on macOS are deeper than that
		address = fmt.Sprintf("/tmp/lan-mouse-ipc-test-%d.sock", os.Getpid())
		_ = os.Remove(address) // leftover from a crashed run
		listener = mustListen(t, network, address)
	}

	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go serveConn(conn, handler)
		}
	}()

	return &fakeServer{
		network: network,
		address: address,
		close: func() {
			listener.Close()
			<-done
		},
	}
}

func mustListen(t *testing.T, network, address string) net.Listener {
	t.Helper()
	listener, err := net.Listen(network, address)
	if err != nil {
		t.Fatalf("fake server cannot listen on %s %s: %v", network, address, err)
	}
	return listener
}

func serveConn(conn net.Conn, handler lineHandler) {
	defer conn.Close()
	reader := bufio.NewReader(conn)
	for {
		line, err := reader.ReadString('\n')
		if err != nil {
			return
		}
		for _, reply := range handler(line) {
			if _, err := conn.Write([]byte(reply)); err != nil {
				return
			}
		}
	}
}

func connectFake(t *testing.T, server *fakeServer) *Conn {
	t.Helper()
	conn, err := ConnectTo(server.network, server.address)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	t.Cleanup(func() { conn.Close() })
	return conn
}

// TestTransportEchoAgainstFakeServer covers task 3.1: a request reaches
// the server and the returned event arrives at the client.
func TestTransportEchoAgainstFakeServer(t *testing.T) {
	server := startFakeServer(t, func(line string) []string {
		if !strings.Contains(line, `"Sync"`) {
			return []string{`{"Error":"unexpected request: ` + line + `"}` + "\n"}
		}
		return []string{
			`{"CaptureStatus":"Enabled"}` + "\n",
			`{"Enumerate":[]}` + "\n",
		}
	})
	conn := connectFake(t, server)

	if err := conn.Send(NewSync()); err != nil {
		t.Fatalf("send: %v", err)
	}
	first, err := conn.NextEvent()
	if err != nil {
		t.Fatalf("first event: %v", err)
	}
	if first.Kind != "CaptureStatus" || first.Status != "Enabled" {
		t.Errorf("first event = %s %+v, want CaptureStatus Enabled", first.Kind, first)
	}
	second, err := conn.NextEvent()
	if err != nil {
		t.Fatalf("second event: %v", err)
	}
	if second.Kind != "Enumerate" || len(second.Clients) != 0 {
		t.Errorf("second event = %s, want empty Enumerate", second.Kind)
	}
}

// TestRequestFixtures covers task 3.2 (requests): encoding every request
// the Rust service captured from its own serde output reproduces the
// fixtures byte for byte.
func TestRequestFixtures(t *testing.T) {
	fixtures := readFixtures(t, "testdata/requests.jsonl")

	// the fixture lines in the order the capture test emitted them
	requests := []Request{
		NewHello([]string{"clipboard"}),
		NewActivate(0, true),
		NewCreate(),
		NewChangePort(4242),
		NewDelete(0),
		NewEnumerate(),
		NewResolveDns(0),
		NewUpdateHostname(0, strPtr("laptop")),
		NewUpdateHostname(1, nil),
		NewUpdatePort(0, 4242),
		NewUpdatePosition(0, PositionRight),
		NewUpdateFixIps(0, []string{"192.168.1.10"}),
		NewEnableCapture(),
		NewEnableEmulation(),
		NewSync(),
		NewAuthorizeKey("laptop", "ab:cd"),
		NewRemoveAuthorizedKey("ab:cd"),
		NewUpdateEnterHook(0, strPtr("true")),
		NewUpdateLeaveHook(0, nil),
		NewSaveConfiguration(),
		NewSetClipboardEnabled(true),
		NewSetClipboardEnabled(false),
	}

	if len(requests) != len(fixtures) {
		t.Fatalf("%d requests against %d fixtures", len(requests), len(fixtures))
	}
	for i, request := range requests {
		encoded, err := json.Marshal(request)
		if err != nil {
			t.Fatalf("request %d (%s): %v", i, request.kind, err)
		}
		if string(encoded) != fixtures[i] {
			t.Errorf("request %d:\n got  %s\n want %s", i, encoded, fixtures[i])
		}
	}
}

// TestEventFixtures covers task 3.2 (events): decoding every event line
// the Rust service captured from its own serde output yields the
// expected kinds and payloads.
func TestEventFixtures(t *testing.T) {
	fixtures := readFixtures(t, "testdata/events.jsonl")

	for i, line := range fixtures {
		var event Event
		if err := json.Unmarshal([]byte(line), &event); err != nil {
			t.Errorf("fixture %d does not decode: %v\n\t%s", i, err, line)
			continue
		}
		switch event.Kind {
		case "Hello":
			if event.Hello.Version != 1 || len(event.Hello.Capabilities) != 0 {
				t.Errorf("fixture %d: hello = %+v", i, event.Hello)
			}
		case "Created", "State":
			if event.Client.Handle != 0 || event.Client.Config.Hostname == nil ||
				*event.Client.Config.Hostname != "laptop" ||
				event.Client.Config.Pos != PositionLeft ||
				event.Client.Config.Port != 4242 ||
				len(event.Client.Config.FixIps) != 1 ||
				event.Client.State.PeerCommit == nil || (*event.Client.State.PeerCommit)[0] != 0xab {
				t.Errorf("fixture %d: client = %+v", i, event.Client)
			}
		case "NoSuchClient":
			if event.Handle != 7 {
				t.Errorf("fixture %d: handle = %d, want 7", i, event.Handle)
			}
		case "Deleted":
			if event.Handle != 0 {
				t.Errorf("fixture %d: handle = %d, want 0", i, event.Handle)
			}
		case "PortChanged":
			if event.Port != 4242 {
				t.Errorf("fixture %d: port = %d, want 4242", i, event.Port)
			}
		case "Enumerate":
			if len(event.Clients) != 1 || event.Clients[0].Handle != 0 {
				t.Errorf("fixture %d: clients = %+v", i, event.Clients)
			}
		case "Error":
			if event.Message == "" {
				t.Errorf("fixture %d: empty error message", i)
			}
		case "CaptureStatus", "EmulationStatus":
			if event.Status != "Enabled" && event.Status != "Disabled" {
				t.Errorf("fixture %d: status = %q", i, event.Status)
			}
		case "AuthorizedUpdated":
			if event.AuthorizedKeys["ab:cd"] != "laptop" {
				t.Errorf("fixture %d: keys = %+v", i, event.AuthorizedKeys)
			}
		case "PublicKeyFingerprint":
			if event.Fingerprint != "aa:bb:cc:dd" {
				t.Errorf("fixture %d: fingerprint = %q", i, event.Fingerprint)
			}
		case "DeviceConnected", "DeviceEntered", "IncomingDisconnected":
			if event.Addr != "192.168.1.10:4242" {
				t.Errorf("fixture %d: addr = %q", i, event.Addr)
			}
		case "ConnectionAttempt":
			if event.Fingerprint != "ab:cd" {
				t.Errorf("fixture %d: fingerprint = %q", i, event.Fingerprint)
			}
		case "ClipboardStatus":
			// both the clearing (null) and the message form are fixtures
		default:
			t.Errorf("fixture %d: unexpected kind %q", i, event.Kind)
		}
	}
}

// TestHandshakeNegotiates covers task 1.1's contract from the client
// side: a v1 client gets a v1 Hello reply and keeps the events the
// service sent before the reply.
func TestHandshakeNegotiates(t *testing.T) {
	server := startFakeServer(t, func(line string) []string {
		return []string{
			// initial state broadcast arrives before the reply
			`{"CaptureStatus":"Enabled"}` + "\n",
			`{"Hello":{"version":1,"capabilities":[]}}` + "\n",
		}
	})
	conn := connectFake(t, server)

	reply, before, err := conn.Handshake(nil, 5*time.Second)
	if err != nil {
		t.Fatalf("handshake: %v", err)
	}
	if reply.Version != ProtocolVersion {
		t.Errorf("reply version = %d, want %d", reply.Version, ProtocolVersion)
	}
	if len(before) != 1 || before[0].Kind != "CaptureStatus" {
		t.Errorf("events before reply = %+v, want one CaptureStatus", before)
	}
}

// TestHelloNilCapabilitiesEncodesEmptyArray pins the wire form the
// service's serde can decode.
func TestHelloNilCapabilitiesEncodesEmptyArray(t *testing.T) {
	encoded, err := json.Marshal(NewHello(nil))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(encoded), `"capabilities":[]`) {
		t.Fatalf("hello = %s, want capabilities as an empty array", encoded)
	}
}

// TestHandshakeRejectsFutureVersion covers task 3.3: a service answering
// with a future version produces a clear error, not a decode failure.
func TestHandshakeRejectsFutureVersion(t *testing.T) {
	for name, reply := range map[string]string{
		"hello with future version": `{"Hello":{"version":99,"capabilities":[]}}` + "\n",
		"explicit error":            `{"Error":"incompatible frontend protocol version 99: this service speaks version 99"}` + "\n",
	} {
		t.Run(name, func(t *testing.T) {
			server := startFakeServer(t, func(line string) []string { return []string{reply} })
			conn := connectFake(t, server)

			_, _, err := conn.Handshake(nil, 5*time.Second)
			if err == nil {
				t.Fatal("handshake succeeded, want an error")
			}
			if !errors.Is(err, ErrorIncompatibleVersion) {
				t.Fatalf("error = %v, want ErrorIncompatibleVersion", err)
			}
			// the message names both versions, so the user sees what is wrong
			if !strings.Contains(err.Error(), "99") || !strings.Contains(err.Error(), "version") {
				t.Errorf("error message does not name the versions: %v", err)
			}
		})
	}
}

// TestUnknownEventVariantIgnored: an event kind the client does not know
// decodes without failing, per DOC.md.
func TestUnknownEventVariantIgnored(t *testing.T) {
	server := startFakeServer(t, func(line string) []string {
		return []string{`{"SomeFutureEvent":{"data":42}}` + "\n"}
	})
	conn := connectFake(t, server)

	if err := conn.Send(NewSync()); err != nil {
		t.Fatalf("send: %v", err)
	}
	event, err := conn.NextEvent()
	if err != nil {
		t.Fatalf("next event: %v", err)
	}
	if event.Kind != "SomeFutureEvent" {
		t.Errorf("kind = %q, want SomeFutureEvent", event.Kind)
	}
}

// TestServiceEndpointMatchesDoc checks the platform endpoints against
// DOC.md's transport table.
func TestServiceEndpointMatchesDoc(t *testing.T) {
	network, address, err := serviceEndpoint()
	if err != nil {
		// XDG_RUNTIME_DIR missing on Linux is a legitimate environment
		// state, the client surfaces it as a connection error
		if runtime.GOOS != "linux" {
			t.Fatalf("serviceEndpoint: %v", err)
		}
		return
	}
	switch runtime.GOOS {
	case "windows":
		if network != "tcp" || address != "127.0.0.1:5252" {
			t.Errorf("windows endpoint = %s %s", network, address)
		}
	case "darwin":
		if network != "unix" || !strings.HasSuffix(address, "/Library/Caches/lan-mouse-socket.sock") {
			t.Errorf("macos endpoint = %s %s", network, address)
		}
	default:
		if network != "unix" || !strings.HasSuffix(address, "/lan-mouse-socket.sock") {
			t.Errorf("linux endpoint = %s %s", network, address)
		}
	}
}

func readFixtures(t *testing.T, path string) []string {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read fixtures: %v", err)
	}
	var lines []string
	for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
		if line != "" {
			lines = append(lines, line)
		}
	}
	return lines
}

func strPtr(s string) *string { return &s }
