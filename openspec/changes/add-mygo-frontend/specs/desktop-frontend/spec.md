# Spec Delta

## Purpose

Lets a user run lan-mouse as a desktop application: manage clients and settings, approve incoming devices, and see what the service is doing, through a graphical frontend that talks to the service over a versioned local protocol.

## ADDED Requirements

### Requirement: The graphical frontend starts and stops the service

Running lan-mouse without a subcommand SHALL show the graphical frontend. If no service is running, the frontend SHALL start one; if a service is already running, the frontend SHALL connect to it instead of starting a second one. When the frontend exits, it SHALL stop a service it started and SHALL leave a service it connected to running.

#### Scenario: Starting with no service running
- **WHEN** the user launches lan-mouse and no service is running
- **THEN** a service starts and the window appears connected to it

#### Scenario: Starting while a service already runs
- **WHEN** the user launches lan-mouse while a service is already running
- **THEN** the window connects to that service and no second service is started

#### Scenario: Exiting the frontend
- **WHEN** the user closes the window after the frontend started the service
- **THEN** the service it started is stopped

### Requirement: The frontend holds no state of its own

All client configuration, connection state and status SHALL live in the service. A frontend SHALL obtain the complete current state when it connects, so any frontend started at any time shows the same picture.

#### Scenario: Connecting to a running service
- **WHEN** a frontend connects to a service that has been running for a while
- **THEN** it displays every configured client and the current capture, emulation and clipboard status without requiring a restart

#### Scenario: Two frontends at once
- **WHEN** two frontends connect to the same service
- **THEN** both show the same state and changes made in one appear in the other

### Requirement: Frontend and service negotiate a protocol version

On connect, the frontend and the service SHALL exchange a protocol version and the set of optional capabilities each supports. A frontend MUST NOT silently mis-handle a service it does not understand.

#### Scenario: Compatible versions
- **WHEN** a frontend and a service support the same protocol version
- **THEN** the frontend proceeds and displays the full state

#### Scenario: Incompatible versions
- **WHEN** a frontend connects to a service whose protocol version it does not support
- **THEN** the frontend reports a clear incompatibility error and does not display partial state

#### Scenario: Service supports capabilities the frontend does not
- **WHEN** the service advertises a capability the frontend does not implement
- **THEN** the frontend continues to work and ignores the unknown capability

### Requirement: Clients can be managed from the frontend

The frontend SHALL allow listing, adding, deleting, activating and deactivating clients, and editing a client's hostname, port, position and fixed addresses.

#### Scenario: Adding a client
- **WHEN** the user adds a client and enters a hostname and position
- **THEN** the client appears in the list and the setting is saved to the configuration file

#### Scenario: Activating a client
- **WHEN** the user enables a client
- **THEN** the service starts capturing that client's screen edge and the row reflects the new state

#### Scenario: Deleting a client
- **WHEN** the user deletes a client
- **THEN** the row disappears and the configuration file no longer contains it

### Requirement: Settings and pairing are reachable from the frontend

The frontend SHALL show the listen port and allow changing it, show the capture and emulation status, display this machine's public key fingerprint, and allow adding and removing authorized peer fingerprints.

#### Scenario: Approving an incoming device
- **WHEN** a device connects whose fingerprint is not authorized
- **THEN** the frontend asks the user to approve or reject that fingerprint

#### Scenario: Removing an authorized device
- **WHEN** the user removes an authorized fingerprint
- **THEN** that device can no longer connect and the change is saved

### Requirement: Service status is visible without interrupting the user

The frontend SHALL surface errors, port changes, capture and emulation status, and clipboard sync problems in a way that does not steal focus or block interaction.

#### Scenario: An error from the service
- **WHEN** the service reports an error
- **THEN** the frontend shows it in a transient, non-modal way

#### Scenario: Clipboard content was only partly synced
- **WHEN** the service reports that clipboard content was reduced because of the size limit
- **THEN** the frontend shows that message in its status area without a modal dialog

### Requirement: The default build needs no system GUI toolkit

The default build SHALL produce the graphical frontend without requiring GTK development headers, `pkg-config`, or any other system GUI toolkit at build time on macOS and Windows. On Linux, the frontend MAY require the GTK 3 runtime libraries at run time but MUST NOT require development headers to build.

#### Scenario: Building on a clean macOS machine
- **WHEN** a user with only the Rust and Go toolchains installed runs the default build
- **THEN** the build succeeds and produces a runnable graphical frontend without installing a system GUI toolkit

#### Scenario: Distributing on macOS
- **WHEN** the application bundle is built
- **THEN** it contains no bundled GUI toolkit dylibs and starts without them

### Requirement: A frontend written in another language can talk to the service

The frontend protocol SHALL be a documented, self-describing local socket protocol that does not depend on the service's internal types, so a frontend in another language can implement it from the documentation alone.

#### Scenario: Implementing a frontend from the documentation
- **WHEN** someone writes a frontend in another language using only the documented protocol
- **THEN** it can connect, receive the initial state, and issue requests without reading the service's source
