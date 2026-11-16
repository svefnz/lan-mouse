# Spec Delta

## Purpose

Transfers the system clipboard between paired lan-mouse peers so that content copied on one machine can be pasted on another, preserving the full set of representations the source application offered.

## ADDED Requirements

### Requirement: Clipboard transfer is triggered by screen entry

When the cursor enters a client, the system SHALL push this machine's clipboard to that client if and only if the local clipboard has changed since the last successful push to that client. Cursor movement alone SHALL NOT modify the destination clipboard.

#### Scenario: Copy then move to the other machine
- **WHEN** the user copies content on machine A and moves the cursor into machine B
- **THEN** A transfers its clipboard to B and the content can be pasted on B

#### Scenario: Move to the other machine without copying
- **WHEN** the user moves the cursor from A into B without changing A's clipboard since the last transfer to B
- **THEN** no transfer occurs and B's clipboard retains its previous content

#### Scenario: Repeated crossings
- **WHEN** the cursor crosses between A and B several times without any clipboard change
- **THEN** no further transfers occur

#### Scenario: Copying the same content twice
- **WHEN** the user copies content, transfers it, later copies byte-identical content, and moves to the other machine
- **THEN** the content is transferred again

#### Scenario: A newer copy supersedes an in-flight transfer
- **WHEN** the local clipboard changes while a transfer to a client is in progress
- **THEN** the destination ends up with the newer content

### Requirement: The full representation set is transferred

The system SHALL transfer every representation the source offers for a clipboard change, not a single chosen format, so the destination can pick whichever representation the receiving application requests.

#### Scenario: Rich content with several representations
- **WHEN** the source clipboard offers plain text, HTML and RTF for the same content
- **THEN** all three arrive on the destination and are available to the receiving application

#### Scenario: Unknown representation type
- **WHEN** the source clipboard offers a representation type the destination platform does not know
- **THEN** that representation is delivered under its original type name with its bytes unchanged

### Requirement: Representations are preserved across platforms

The system SHALL normalize platform-specific representation names to canonical names on transfer and map them back to the destination platform's names, so content survives a transfer between any two supported platforms.

#### Scenario: Plain text from macOS to Linux
- **WHEN** plain text is copied on macOS and the cursor enters a Linux client
- **THEN** the text is available to Linux applications as ordinary plain text

#### Scenario: HTML transferred to Windows
- **WHEN** HTML content is transferred to a Windows peer
- **THEN** the HTML is written in the Windows clipboard HTML framing with correct byte offsets and pastes correctly into a rich text application

### Requirement: Concealed content is never transferred

The system MUST NOT transfer clipboard content that the source application has flagged as concealed, transient, or excluded from clipboard monitoring.

#### Scenario: Copying a password
- **WHEN** the user copies a password from a password manager that marks the clipboard as concealed and then moves to another machine
- **THEN** no clipboard content is transferred and the destination clipboard is unchanged

### Requirement: Transfers are size-capped with graceful degradation

The system SHALL enforce a configurable maximum transfer size, defaulting to 100 MB. Content exceeding the cap SHALL be reduced to the representations that fit rather than failing the transfer as a whole.

#### Scenario: Content within the cap
- **WHEN** a clipboard change totals less than the configured cap
- **THEN** all representations are transferred

#### Scenario: Content exceeding the cap
- **WHEN** a clipboard change totals more than the configured cap and includes both a large image and small text
- **THEN** the text representations are transferred, the large representation is dropped, and the degradation is reported to the user

#### Scenario: A clipboard change larger than the cap alone
- **WHEN** a single representation exceeds the cap and no smaller representation exists
- **THEN** nothing is transferred and the skip is reported to the user

### Requirement: An empty clipboard is never propagated

The system MUST NOT transfer an empty clipboard, so that entering a machine never clears content that was already there.

#### Scenario: Empty source clipboard
- **WHEN** the source clipboard is empty and the cursor enters a client
- **THEN** the destination clipboard retains its previous content

### Requirement: Transfers are atomic at the destination

A destination's clipboard SHALL be updated with a complete transfer or left unchanged. Partially received content MUST NOT be written to the clipboard.

#### Scenario: Transfer interrupted by disconnection
- **WHEN** a transfer is interrupted before completion because the connection drops
- **THEN** the destination clipboard is unchanged

### Requirement: The local switch governs both directions

Each machine SHALL have a local switch that enables or disables clipboard sharing for that machine. When disabled, the machine neither sends nor applies clipboard transfers.

#### Scenario: Disabled on the sending machine
- **WHEN** clipboard sharing is disabled on A and the cursor moves from A into B
- **THEN** nothing is transferred and B's clipboard is unchanged

#### Scenario: Disabled on the receiving machine
- **WHEN** clipboard sharing is disabled on B and the cursor moves from A into B
- **THEN** A's content is not applied to B's clipboard

#### Scenario: Both enabled
- **WHEN** clipboard sharing is enabled on both machines
- **THEN** transfers occur in both directions according to the entry trigger

### Requirement: Only authenticated peers participate

Clipboard transfer SHALL be available only to peers whose certificate fingerprint is in the local authorized list. A peer does not need an outgoing client entry to participate.

#### Scenario: Peer not in the authorized list
- **WHEN** a peer that is not in the authorized list attempts to open a clipboard channel
- **THEN** the channel is refused and no clipboard content is exchanged

#### Scenario: Authorized peer with no outgoing client entry
- **WHEN** a peer is in the authorized list but has no outgoing client entry configured, and it enters this machine
- **THEN** clipboard transfer works in both directions

### Requirement: File lists transfer as paths only

File list clipboard content SHALL be transferred as `text/uri-list` paths. File contents SHALL NOT be transferred.

#### Scenario: Copying files
- **WHEN** the user copies files on one machine and moves to another
- **THEN** the destination receives the file paths as a file list and no file contents are transferred

### Requirement: Clipboard activity never degrades input responsiveness

Clipboard transfer SHALL NOT block or delay pointer and keyboard event handling, regardless of transfer size or clipboard backend latency.

#### Scenario: Large transfer in progress
- **WHEN** a large clipboard transfer is in progress
- **THEN** pointer movement and key events on both machines continue without added latency

#### Scenario: Clipboard backend is slow to respond
- **WHEN** reading the local clipboard takes a long time
- **THEN** cursor entry into the client is not delayed by that read

### Requirement: Transfer problems are reported without interrupting the user

The system SHALL report size degradation and in-progress transfers in the graphical frontend status area, and SHALL NOT report connection failures there.

#### Scenario: Degradation is reported
- **WHEN** a transfer is reduced because of the size cap
- **THEN** the frontend shows that the clipboard was partially synced and why

#### Scenario: Connection failure is not reported
- **WHEN** the clipboard channel to a peer cannot be established
- **THEN** no user-facing message is shown and the failure is only logged
