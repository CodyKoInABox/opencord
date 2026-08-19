# Opencord architecture

Opencord is a local-first peer mesh. There is no authoritative server and no
global consensus. Every device owns a signed append-only event log in SQLite;
peers that prove knowledge of the same group key exchange gaps in those logs.

## Chosen stack

- **Rust** for predictable memory use, native startup, and one implementation
  of the security-critical core.
- **egui/eframe with wgpu** for a single native process, Vulkan preferred over
  a Direct3D 12 fallback on Windows, and a redraw-on-change UI. There is no
  browser runtime, JavaScript VM, or DOM.
- **SQLite in WAL mode** for durable local state, indexed sync queries, and
  transactional author sequence allocation.
- **QUIC (Quinn)** for authenticated, multiplexed direct peer links. The TLS
  certificate is ephemeral; stable application identity is an Ed25519 key.
- **mDNS** for zero-configuration LAN discovery. Signed invites also carry
  known socket addresses for explicit/WAN connections, with optional local
  router port mapping. There is no rendezvous, STUN, TURN, or relay service.
- **XChaCha20-Poly1305 + Ed25519** for group-content confidentiality,
  integrity, and author authenticity.
- **CPAL + Opus over QUIC datagrams** for low-overhead peer audio. This is
  intentionally smaller than embedding a WebRTC stack. An ICE/WebRTC adapter
  can later replace the transport without changing audio or identity layers.

Blockchain is deliberately excluded. A private chat needs no global ordering,
currency, proof-of-work, or public ledger. Per-author signed logs plus eventual
reconciliation solve the actual consistency problem with far less CPU, storage,
network traffic, and attack surface.

## Process model

```text
egui UI thread
  | commands / snapshots
Tokio network thread ---- mDNS discovery
  |                         |
  +---- QUIC peer sessions--+
  |       reliable sync + group-encrypted voice datagrams
  |
SQLite WAL store <---- crypto/event validation
  |
local encrypted event records and group keys
```

The UI polls compact generation counters and requests repaint only while state
changes. Network and audio callbacks never block on rendering.

## Replication

Each event has `(group_id, author_public_key, author_sequence)`. The author
sequence is allocated transactionally and the complete encrypted envelope is
signed. On connection, peers:

1. exchange signed identity challenges;
2. prove shared group-key possession with HMAC challenges;
3. exchange per-author maximum sequence summaries;
4. request and validate missing ranges;
5. keep the session open and push newly appended events.

This is deterministic, idempotent, and supports a new member rebuilding all
history available on any online peer. SQLite uniqueness constraints reject
duplicates and author-sequence equivocation.

## Prototype boundary

The prototype supports Windows first while keeping OS-specific audio and paths
behind libraries with Linux/macOS implementations. LAN discovery and explicit
socket addresses are implemented. Two peers behind incompatible CGNAT cannot
reach each other without a mutually reachable peer or manual network changes;
Opencord reports this rather than silently depending on central infrastructure.
