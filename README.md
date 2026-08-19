# Opencord

Opencord is a Windows-first, native peer-to-peer group chat prototype. It has
no account service, message server, cloud database, telemetry collector,
rendezvous server, relay, or blockchain. Messages are stored in SQLite on each
member's computer and repaired from other online replicas when peers reconnect.

> [!WARNING]
> This is an unaudited prototype, not production cryptographic software. Read
> [the security model](docs/SECURITY.md) before using it for sensitive data.

## What works

- Native Rust desktop UI with no browser runtime
- Encrypted groups, text channels, messages, and attachments (up to 8 MiB)
- Signed invite capabilities (`opencord://join/...`)
- Automatic LAN discovery with mDNS
- Direct QUIC connections by UDP address, remembered peer endpoints, and
  best-effort UPnP port mapping
- Signed, per-author event logs with automatic missing-range reconciliation
- Full history bootstrap for a new member from any online peer holding it
- Peer-to-peer Opus voice calls with audio active only while joined
- Peer-to-peer screen sharing with an idle-until-enabled capture worker
- Local peer blocking
- SQLite WAL storage containing ciphertext event bodies

## Why this stack

Rust keeps the network, storage, audio, cryptography, and UI in one native
process with predictable resource use. `egui` renders through `wgpu` on Windows;
SQLite provides durable local indexing; Quinn carries direct QUIC sessions;
CPAL and Opus provide compact audio. App-layer content uses
XChaCha20-Poly1305, while persistent identities and events use Ed25519.

WebRTC is not embedded in this prototype: Opus over QUIC is substantially
smaller and the no-central rule rules out hosted STUN/TURN anyway. A future ICE
adapter can improve NAT traversal without changing the event or identity model.
Blockchain is also unnecessary: private group replication needs signed logs and
eventual reconciliation, not global consensus.

See [architecture](docs/ARCHITECTURE.md) and [wire protocol](docs/PROTOCOL.md)
for the design details.

## Build on Windows

Prerequisites:

- Rust 1.95 or newer with the MSVC toolchain
- Visual Studio 2022 Build Tools with **Desktop development with C++**

From a Developer PowerShell:

```powershell
cargo test --all-targets
cargo run --release
```

The optimized executable is written to `target\release\opencord.exe`. Profile
data defaults to `%LOCALAPPDATA%\CodyKoInABox\Opencord`.

Windows Firewall must allow inbound and outbound UDP for direct peers. The
default listen address is `0.0.0.0:39217`.

## Use it

1. Start Opencord and create a group with the `+` button.
2. Choose **Invite**, copy the capability, and send it privately to a peer.
3. The peer uses the arrow button to import it. On the same LAN, discovery and
   synchronization are automatic. For a reachable internet peer, use
   **Connect** with its public UDP address if the invite endpoint is stale.
4. Leave at least one history-holding peer online while a new member rebuilds.

An invite contains the group decryption capability and grants access to the
history held by connected members. Treat it like a password.

To run two isolated profiles on one computer:

```powershell
.\target\release\opencord.exe --data-dir .demo\alice --listen 127.0.0.1:40101 --name Alice --no-discovery
.\target\release\opencord.exe --data-dir .demo\bob   --listen 127.0.0.1:40102 --name Bob   --no-discovery --connect 127.0.0.1:40101
```

Create the group on Alice, export its invite, and import it on Bob. Direct
connection alone does not reveal groups; both peers must possess the invite.

Run `opencord.exe --help` for every command-line option.

## Honest no-server tradeoff

Peers behind incompatible carrier-grade NAT cannot always establish a new
connection without a reachable peer, manual port forwarding, or a relay. Since
Opencord deliberately provides no central rendezvous, STUN, TURN, or relay, it
reports that limitation instead of silently sending metadata or content through
infrastructure. Offline delivery likewise exists only while another replica
holding the message eventually comes online.

## Development checks

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release --locked
```

The integration suite starts two real QUIC nodes and verifies history rebuild,
live exchange, encrypted channel replication, voice packets, and screen frames.

### Current Windows baseline

The optimized demo profile on the development machine reached a usable window
in about 0.57 seconds, held at 0% measured idle CPU, used 157.6 MiB resident RAM,
and produced a 16.8 MiB executable. The Vulkan graphics driver is the largest
resident cost; DX12 and direct OpenGL were both worse on this machine. These are
machine-specific prototype measurements and a baseline for further UI-renderer
work, not universal guarantees.

Licensed under AGPL-3.0-or-later.
