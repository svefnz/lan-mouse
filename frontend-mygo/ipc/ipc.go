// Package ipc implements the lan-mouse frontend protocol: the transport
// to the service's local socket and the request and event messages.
//
// The protocol is documented in DOC.md, section "Frontend Protocol (local
// IPC)"; the types here are transcribed from that section, which is the
// contract. Messages are newline-delimited JSON over a unix socket
// (macOS, Linux) or TCP 127.0.0.1:5252 (Windows).
package ipc

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"
)

// ProtocolVersion is the frontend protocol version the client speaks.
// The service answers a Hello with its own; anything else is rejected.
const ProtocolVersion uint32 = 1

// WindowsServiceAddr is the TCP endpoint the service listens on for
// frontend connections on Windows.
const WindowsServiceAddr = "127.0.0.1:5252"

// Position is the screen edge a client's capture zone sits at.
type Position string

const (
	PositionLeft   Position = "left"
	PositionRight  Position = "right"
	PositionTop    Position = "top"
	PositionBottom Position = "bottom"
)

// ClientConfig is what the user configured for a client.
type ClientConfig struct {
	Hostname *string  `json:"hostname"`
	FixIps   []string `json:"fix_ips"`
	Port     uint16   `json:"port"`
	Pos      Position `json:"pos"`
	Cmd      *string  `json:"cmd"`
	LeaveCmd *string  `json:"leave_cmd"`
}

// ClientState is what the service knows about a client at runtime.
type ClientState struct {
	Active         bool     `json:"active"`
	ActiveAddr     *string  `json:"active_addr"`
	Alive          bool     `json:"alive"`
	DnsIps         []string `json:"dns_ips"`
	Ips            []string `json:"ips"`
	HasPressedKeys bool     `json:"has_pressed_keys"`
	Resolving      bool     `json:"resolving"`
	PeerCommit     *[8]byte `json:"peer_commit"`
}

// ClientEntry is a client as the service reports it: handle, config, state.
type ClientEntry struct {
	Handle uint64
	Config ClientConfig
	State  ClientState
}

// HelloPayload is the handshake message both directions carry.
type HelloPayload struct {
	Version      uint32   `json:"version"`
	Capabilities []string `json:"capabilities"`
}

// Request is a message sent to the service. Build one with the New*
// constructors; the zero value does not encode.
type Request struct {
	kind    string // serde variant tag, e.g. "Activate"
	payload any    // nil for bare-string unit variants
	unit    bool   // unit variant: encoded as a bare JSON string
}

var unitVariants = map[string]bool{
	"Create":            true,
	"EnableCapture":     true,
	"EnableEmulation":   true,
	"Sync":              true,
	"SaveConfiguration": true,
}

// MarshalJSON encodes the externally tagged enum form the service
// expects: unit variants as bare strings, everything else as a
// single-key object {"Tag": payload}.
func (r Request) MarshalJSON() ([]byte, error) {
	if r.kind == "" {
		return nil, errors.New("ipc: empty request")
	}
	if unitVariants[r.kind] {
		return json.Marshal(r.kind)
	}
	var buf bytes.Buffer
	buf.WriteByte('{')
	key, err := json.Marshal(r.kind)
	if err != nil {
		return nil, err
	}
	buf.Write(key)
	buf.WriteByte(':')
	payload, err := json.Marshal(r.payload)
	if err != nil {
		return nil, err
	}
	buf.Write(payload)
	buf.WriteByte('}')
	return buf.Bytes(), nil
}

func (r Request) String() string {
	b, err := r.MarshalJSON()
	if err != nil {
		return r.kind
	}
	return string(b)
}

// NewHello builds the handshake request, sent as the first message.
// A nil capabilities slice is normalized to an empty array: the service
// decodes capabilities as a list, where JSON null is invalid.
func NewHello(capabilities []string) Request {
	if capabilities == nil {
		capabilities = []string{}
	}
	return Request{kind: "Hello", payload: HelloPayload{
		Version:      ProtocolVersion,
		Capabilities: capabilities,
	}}
}

func NewActivate(handle uint64, active bool) Request {
	return Request{kind: "Activate", payload: []any{handle, active}}
}

func NewCreate() Request { return Request{kind: "Create", unit: true} }

func NewChangePort(port uint16) Request {
	return Request{kind: "ChangePort", payload: port}
}

func NewDelete(handle uint64) Request {
	return Request{kind: "Delete", payload: handle}
}

// NewEnumerate requests the full client list. The serde variant is a
// tuple variant without elements, encoded as an empty array.
func NewEnumerate() Request {
	return Request{kind: "Enumerate", payload: []any{}}
}

func NewResolveDns(handle uint64) Request {
	return Request{kind: "ResolveDns", payload: handle}
}

func NewUpdateHostname(handle uint64, hostname *string) Request {
	return Request{kind: "UpdateHostname", payload: []any{handle, hostname}}
}

func NewUpdatePort(handle uint64, port uint16) Request {
	return Request{kind: "UpdatePort", payload: []any{handle, port}}
}

func NewUpdatePosition(handle uint64, pos Position) Request {
	return Request{kind: "UpdatePosition", payload: []any{handle, pos}}
}

func NewUpdateFixIps(handle uint64, ips []string) Request {
	return Request{kind: "UpdateFixIps", payload: []any{handle, ips}}
}

func NewEnableCapture() Request { return Request{kind: "EnableCapture", unit: true} }

func NewEnableEmulation() Request { return Request{kind: "EnableEmulation", unit: true} }

func NewSync() Request { return Request{kind: "Sync", unit: true} }

func NewAuthorizeKey(description, fingerprint string) Request {
	return Request{kind: "AuthorizeKey", payload: []any{description, fingerprint}}
}

func NewRemoveAuthorizedKey(fingerprint string) Request {
	return Request{kind: "RemoveAuthorizedKey", payload: fingerprint}
}

func NewUpdateEnterHook(handle uint64, cmd *string) Request {
	return Request{kind: "UpdateEnterHook", payload: []any{handle, cmd}}
}

func NewUpdateLeaveHook(handle uint64, cmd *string) Request {
	return Request{kind: "UpdateLeaveHook", payload: []any{handle, cmd}}
}

func NewSaveConfiguration() Request {
	return Request{kind: "SaveConfiguration", unit: true}
}

func NewSetClipboardEnabled(enabled bool) Request {
	return Request{kind: "SetClipboardEnabled", payload: enabled}
}

// Event is a message received from the service. Kind carries the serde
// variant tag; only the fields listed for that kind in DOC.md are valid:
//
//	Hello: Hello, Created/State: Client, Enumerate: Clients,
//	Deleted/NoSuchClient: Handle, PortChanged: Port and PortError,
//	Error: Message, CaptureStatus/EmulationStatus: Status,
//	AuthorizedUpdated: AuthorizedKeys,
//	PublicKeyFingerprint: Fingerprint,
//	DeviceConnected: Addr and Fingerprint,
//	DeviceEntered: Fingerprint, Addr and Pos,
//	IncomingDisconnected: Addr,
//	ConnectionAttempt: Fingerprint,
//	ClipboardStatus: ClipboardMsg (nil clears the status line),
//	ClipboardSyncEnabled: ClipboardSyncEnabled.
type Event struct {
	Kind string

	Hello          HelloPayload
	Client         ClientEntry
	Clients        []ClientEntry
	Handle         uint64
	Port           uint16
	PortError      *string
	Message        string
	Status         string
	AuthorizedKeys map[string]string
	Fingerprint    string
	Addr           string
	Pos            Position
	ClipboardMsg   *string
	// ClipboardSyncEnabled is valid when Kind is "ClipboardSyncEnabled";
	// nil until the value has arrived.
	ClipboardSyncEnabled *bool
}

// UnmarshalJSON decodes the externally tagged enum form. Unknown
// variants decode to Kind set and no payload, so a frontend can ignore
// them instead of failing.
func (e *Event) UnmarshalJSON(data []byte) error {
	var obj map[string]json.RawMessage
	if err := json.Unmarshal(data, &obj); err != nil {
		return fmt.Errorf("ipc: event is not an object: %w", err)
	}
	if len(obj) != 1 {
		return fmt.Errorf("ipc: event has %d keys, want exactly 1", len(obj))
	}
	for kind, raw := range obj {
		e.Kind = kind
		return e.decode(kind, raw)
	}
	return nil
}

func (e *Event) decode(kind string, raw json.RawMessage) error {
	switch kind {
	case "Hello":
		return json.Unmarshal(raw, &e.Hello)
	case "Created", "State":
		return decodeTuple(raw, &e.Client.Handle, &e.Client.Config, &e.Client.State)
	case "Enumerate":
		var entries []json.RawMessage
		if err := json.Unmarshal(raw, &entries); err != nil {
			return err
		}
		for _, entry := range entries {
			var client ClientEntry
			if err := decodeTuple(entry, &client.Handle, &client.Config, &client.State); err != nil {
				return err
			}
			e.Clients = append(e.Clients, client)
		}
	case "Deleted", "NoSuchClient":
		return json.Unmarshal(raw, &e.Handle)
	case "PortChanged":
		return decodeTuple(raw, &e.Port, &e.PortError)
	case "Error":
		return json.Unmarshal(raw, &e.Message)
	case "CaptureStatus", "EmulationStatus":
		return json.Unmarshal(raw, &e.Status)
	case "AuthorizedUpdated":
		return json.Unmarshal(raw, &e.AuthorizedKeys)
	case "PublicKeyFingerprint":
		return json.Unmarshal(raw, &e.Fingerprint)
	case "DeviceConnected", "ConnectionAttempt":
		var payload struct {
			Addr        string `json:"addr"`
			Fingerprint string `json:"fingerprint"`
		}
		if err := json.Unmarshal(raw, &payload); err != nil {
			return err
		}
		e.Addr, e.Fingerprint = payload.Addr, payload.Fingerprint
	case "DeviceEntered":
		var payload struct {
			Addr        string   `json:"addr"`
			Fingerprint string   `json:"fingerprint"`
			Pos         Position `json:"pos"`
		}
		if err := json.Unmarshal(raw, &payload); err != nil {
			return err
		}
		e.Addr, e.Fingerprint, e.Pos = payload.Addr, payload.Fingerprint, payload.Pos
	case "IncomingDisconnected":
		return json.Unmarshal(raw, &e.Addr)
	case "ClipboardStatus":
		return json.Unmarshal(raw, &e.ClipboardMsg)
	case "ClipboardSyncEnabled":
		return json.Unmarshal(raw, &e.ClipboardSyncEnabled)
	default:
		// unknown variant: keep Kind, no payload — ignored by callers
	}
	return nil
}

// decodeTuple decodes a JSON array positionally into the given pointers.
func decodeTuple(data json.RawMessage, dsts ...any) error {
	var elems []json.RawMessage
	if err := json.Unmarshal(data, &elems); err != nil {
		return err
	}
	if len(elems) != len(dsts) {
		return fmt.Errorf("ipc: tuple has %d elements, want %d", len(elems), len(dsts))
	}
	for i, dst := range dsts {
		if err := json.Unmarshal(elems[i], dst); err != nil {
			return err
		}
	}
	return nil
}

// ErrorIncompatibleVersion is returned by Handshake when the service
// reports a protocol version this client does not support.
var ErrorIncompatibleVersion = errors.New("incompatible protocol version")

// ConnectionError reports why connecting to the service failed.
type ConnectionError struct {
	Network string // "unix" or "tcp"
	Address string
	Err     error
}

func (e *ConnectionError) Error() string {
	return fmt.Sprintf("ipc: cannot reach service on %s %s: %v", e.Network, e.Address, e.Err)
}

func (e *ConnectionError) Unwrap() error { return e.Err }

// Conn is one connection to the lan-mouse service. Send is safe for
// concurrent use; the event-reading methods must be called from a single
// goroutine.
type Conn struct {
	conn    net.Conn
	reader  *bufio.Reader
	writer  *bufio.Writer
	writeMu sync.Mutex
}

// serviceEndpoint resolves the platform endpoint the service listens on.
func serviceEndpoint() (network, address string, err error) {
	switch runtime.GOOS {
	case "windows":
		return "tcp", WindowsServiceAddr, nil
	case "darwin":
		home, err := os.UserHomeDir()
		if err != nil {
			return "", "", err
		}
		return "unix", filepath.Join(home, "Library", "Caches", "lan-mouse-socket.sock"), nil
	default:
		xdg := os.Getenv("XDG_RUNTIME_DIR")
		if xdg == "" {
			return "", "", errors.New("ipc: XDG_RUNTIME_DIR is not set")
		}
		return "unix", filepath.Join(xdg, "lan-mouse-socket.sock"), nil
	}
}

// IsServiceRunning reports whether a service is accepting connections
// on this platform's frontend endpoint.
func IsServiceRunning() bool {
	network, address, err := serviceEndpoint()
	if err != nil {
		return false
	}
	conn, err := net.DialTimeout(network, address, time.Second)
	if err != nil {
		return false
	}
	conn.Close()
	return true
}

// Connect connects to the running service.
func Connect() (*Conn, error) {
	network, address, err := serviceEndpoint()
	if err != nil {
		return nil, err
	}
	return ConnectTo(network, address)
}

// ConnectTo connects to a service at an explicit endpoint; exported for
// tests to point the client at a fake server.
func ConnectTo(network, address string) (*Conn, error) {
	conn, err := net.DialTimeout(network, address, time.Second)
	if err != nil {
		return nil, &ConnectionError{Network: network, Address: address, Err: err}
	}
	return &Conn{
		conn:   conn,
		reader: bufio.NewReader(conn),
		writer: bufio.NewWriter(conn),
	}, nil
}

// Send writes one request line. It is safe for concurrent use.
func (c *Conn) Send(r Request) error {
	line, err := r.MarshalJSON()
	if err != nil {
		return err
	}
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	if _, err := c.writer.Write(append(line, '\n')); err != nil {
		return err
	}
	return c.writer.Flush()
}

// NextEvent reads the next event line, blocking until one arrives or the
// service closes the connection. A line that does not decode is
// reported as an Error event instead of failing the connection.
func (c *Conn) NextEvent() (Event, error) {
	return c.nextEvent(time.Time{})
}

func (c *Conn) nextEvent(deadline time.Time) (Event, error) {
	if !deadline.IsZero() {
		c.conn.SetReadDeadline(deadline)
		defer c.conn.SetReadDeadline(time.Time{})
	}
	for {
		line, err := c.reader.ReadString('\n')
		if err != nil {
			return Event{}, err
		}
		line = strings.TrimRight(line, "\n")
		var event Event
		if decodeErr := json.Unmarshal([]byte(line), &event); decodeErr != nil {
			return Event{Kind: "Error", Message: "undecodable event: " + decodeErr.Error()}, nil
		}
		return event, nil
	}
}

// Handshake negotiates the protocol version. It sends Hello and reads
// events until the service's Hello reply arrives; events the service
// sent first (the initial state broadcast) are returned so the caller
// does not lose them. An Error reply or a reply naming another version
// surfaces as ErrorIncompatibleVersion.
func (c *Conn) Handshake(capabilities []string, timeout time.Duration) (HelloPayload, []Event, error) {
	if err := c.Send(NewHello(capabilities)); err != nil {
		return HelloPayload{}, nil, err
	}
	deadline := time.Now().Add(timeout)
	var before []Event
	for {
		event, err := c.nextEvent(deadline)
		if err != nil {
			return HelloPayload{}, nil, fmt.Errorf("ipc: handshake failed: %w", err)
		}
		switch event.Kind {
		case "Hello":
			if event.Hello.Version != ProtocolVersion {
				return HelloPayload{}, nil, fmt.Errorf(
					"ipc: %w: service speaks version %d, this client speaks version %d",
					ErrorIncompatibleVersion, event.Hello.Version, ProtocolVersion)
			}
			return event.Hello, before, nil
		case "Error":
			// the service rejects an unsupported version with an error
			// and closes the connection
			return HelloPayload{}, nil, fmt.Errorf("ipc: %w: %s",
				ErrorIncompatibleVersion, event.Message)
		default:
			before = append(before, event)
		}
	}
}

// Close closes the connection.
func (c *Conn) Close() error { return c.conn.Close() }
