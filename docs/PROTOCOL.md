# Wire protocol v1

All reliable records use Postcard binary serialization with a big-endian
32-bit length prefix. The maximum reliable frame is 16 MiB.

## Identity handshake

`PeerHello` contains protocol version, random nonce, peer public key, display
name, listen address, and an Ed25519 signature over the preceding fields. Each
side then sends group proofs computed as:

`HMAC-SHA256(group_key, "opencord-group-proof-v1" || group_id || nonce_A || nonce_B)`

Nonce order is the lexicographic order of the two peer public keys. Only groups
whose proofs validate are exposed to that connection.

## Event envelope

The authenticated header contains protocol version, event ID, group ID,
channel ID, author public key, author sequence, sender timestamp, payload kind,
and a random 192-bit nonce. The payload is Postcard-encoded and encrypted with
XChaCha20-Poly1305. The header bytes are AEAD associated data. The author signs
the canonical header followed by ciphertext.

The event ID is BLAKE3 over the canonical header without the ID, followed by
ciphertext. Receivers recompute the ID, verify the Ed25519 signature, verify the
AEAD tag, enforce size limits, and insert with unique author sequence.

## Synchronization records

- `Inventory`: maximum stored sequence per `(group, author)`.
- `RequestRange`: inclusive author sequence interval.
- `EventBatch`: up to 128 envelopes or 8 MiB.
- `LiveEvent`: one newly appended envelope.
- `Ping`/`Pong`: liveness and measured round-trip time.

Peers may request ranges in either direction, so any online replica can seed a
newly invited member. All operations are idempotent.

Channel descriptors and screen frames use the same group-key AEAD boundary as
event payloads. They are never exposed as plaintext to the transport layer.

## Voice datagrams

Voice is Opus mono at 48 kHz in 20 ms frames. A compact payload carries a
monotonically wrapping audio sequence and one Opus packet. The payload is
sealed with the group key and a fresh XChaCha20-Poly1305 nonce, then sent in a
QUIC datagram. Loss is not retransmitted.
