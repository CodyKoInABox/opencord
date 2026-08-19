# Security model

## Protected by the prototype

- Message and attachment bodies are encrypted with a random 256-bit group key
  using XChaCha20-Poly1305.
- The immutable envelope header is authenticated as AEAD associated data.
- Every event is signed by its author's persistent Ed25519 identity.
- Peer sessions require a signed challenge and group-key HMAC proof before
  inventory or event exchange.
- The database retains encrypted event ciphertext rather than plaintext chat
  bodies. Plaintext exists transiently when rendered or saved by the user.
- Voice, screen, and channel metadata are app-layer group encrypted as well as
  carried inside QUIC's forward-secret peer transport.

## Trust and availability

Possession of a group invite is membership capability. A member can retain and
copy any content they can decrypt. History availability is best effort: if all
peers delete an event or stay offline, there is no central backup to recover it.
Peer clocks affect display order only; signatures and per-author sequences are
the replication authority.

## Known prototype limitations

- Group-key rotation, device revocation, multi-device identity transfer, safety
  number comparison, spam controls, and formal protocol review are not yet
  implemented.
- The identity seed and group keys are stored in the user's private application
  directory. Production builds should wrap them with Windows DPAPI, macOS
  Keychain, or Linux Secret Service and offer an encrypted export.
- mDNS exposes that Opencord is running and the peer's public identifier on the
  local network. Group names and contents are never advertised.
- Explicit internet addresses require reachable UDP/port forwarding. This is
  an unavoidable availability tradeoff of the no-central-infrastructure rule.
- No prototype should be treated as audited cryptographic software.

See `docs/PROTOCOL.md` for signed and encrypted record details.
