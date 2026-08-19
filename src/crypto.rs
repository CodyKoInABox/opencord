use std::{fs, path::Path};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce, aead::Aead};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::model::{
    EventEnvelope, EventHeader, EventHeaderWithoutId, EventId, GroupInvite, MessagePayload,
    PROTOCOL_VERSION, PeerId, UnsignedGroupInvite,
};

const IDENTITY_FILE_VERSION: u16 = 1;

#[derive(Clone)]
pub struct Identity {
    signing_key: SigningKey,
    display_name: String,
}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    version: u16,
    display_name: String,
    secret: String,
}

impl Identity {
    pub fn generate(display_name: impl Into<String>) -> Self {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).expect("operating system randomness unavailable");
        Self {
            signing_key: SigningKey::from_bytes(&secret),
            display_name: sanitize_name(&display_name.into()),
        }
    }

    pub fn load_or_create(path: &Path, requested_name: Option<&str>) -> anyhow::Result<Self> {
        if path.exists() {
            let raw =
                fs::read(path).with_context(|| format!("read identity {}", path.display()))?;
            let file: IdentityFile = postcard::from_bytes(&raw).context("decode identity")?;
            anyhow::ensure!(
                file.version == IDENTITY_FILE_VERSION,
                "unsupported identity file version"
            );
            let bytes = URL_SAFE_NO_PAD
                .decode(file.secret)
                .context("decode identity secret")?;
            let bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid identity secret length"))?;
            let display_name = requested_name
                .map(sanitize_name)
                .unwrap_or_else(|| sanitize_name(&file.display_name));
            return Ok(Self {
                signing_key: SigningKey::from_bytes(&bytes),
                display_name,
            });
        }

        let identity = Self::generate(requested_name.unwrap_or("New peer"));
        identity.save(path)?;
        Ok(identity)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let file = IdentityFile {
            version: IDENTITY_FILE_VERSION,
            display_name: self.display_name.clone(),
            secret: URL_SAFE_NO_PAD.encode(self.signing_key.to_bytes()),
        };
        let encoded = postcard::to_stdvec(&file).context("encode identity")?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, encoded).with_context(|| format!("write {}", temporary.display()))?;
        fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    }

    pub fn peer_id(&self) -> PeerId {
        PeerId(self.signing_key.verifying_key().to_bytes())
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn rename(&mut self, display_name: &str) {
        self.display_name = sanitize_name(display_name);
    }

    pub fn sign(&self, bytes: &[u8]) -> Vec<u8> {
        self.signing_key.sign(bytes).to_bytes().to_vec()
    }

    pub fn seal_event(
        &self,
        group_id: crate::model::GroupId,
        channel_id: crate::model::ChannelId,
        group_secret: &[u8; 32],
        author_sequence: u64,
        sent_at_ms: i64,
        payload: &MessagePayload,
    ) -> anyhow::Result<EventEnvelope> {
        payload.validate()?;
        let mut nonce = [0_u8; 24];
        getrandom::fill(&mut nonce)
            .map_err(|error| anyhow::anyhow!("generate event nonce: {error}"))?;
        let without_id = EventHeaderWithoutId {
            version: PROTOCOL_VERSION,
            group_id,
            channel_id,
            author: self.peer_id(),
            author_sequence,
            sent_at_ms,
            payload_kind: payload.kind(),
            nonce,
        };
        let associated_data = postcard::to_stdvec(&without_id).context("encode event header")?;
        let plaintext = postcard::to_stdvec(payload).context("encode event payload")?;
        let cipher = XChaCha20Poly1305::new(group_secret.into());
        let nonce_value =
            XNonce::try_from(nonce.as_slice()).expect("XChaCha nonce has a fixed length");
        let ciphertext = cipher
            .encrypt(
                &nonce_value,
                chacha20poly1305::aead::Payload {
                    msg: &plaintext,
                    aad: &associated_data,
                },
            )
            .map_err(|_| anyhow::anyhow!("event encryption failed"))?;
        let id = compute_event_id(&without_id, &ciphertext)?;
        let header = EventHeader {
            version: without_id.version,
            id,
            group_id,
            channel_id,
            author: without_id.author,
            author_sequence,
            sent_at_ms,
            payload_kind: without_id.payload_kind,
            nonce,
        };
        let signature = self.sign(&event_signing_bytes(&header, &ciphertext)?);
        Ok(EventEnvelope {
            header,
            ciphertext,
            signature,
        })
    }

    pub fn sign_invite(&self, body: UnsignedGroupInvite) -> anyhow::Result<GroupInvite> {
        let bytes = postcard::to_stdvec(&body).context("encode invite")?;
        Ok(GroupInvite {
            body,
            signature: self.sign(&bytes),
        })
    }
}

pub fn verify_signature(peer: PeerId, bytes: &[u8], signature: &[u8]) -> anyhow::Result<()> {
    let key = VerifyingKey::from_bytes(&peer.0).context("invalid author key")?;
    let signature = Signature::from_slice(signature).context("invalid signature length")?;
    key.verify(bytes, &signature)
        .context("signature verification failed")
}

pub fn verify_invite(invite: &GroupInvite) -> anyhow::Result<()> {
    anyhow::ensure!(
        invite.body.version == PROTOCOL_VERSION,
        "unsupported invite version"
    );
    let bytes = postcard::to_stdvec(&invite.body).context("encode invite for verification")?;
    verify_signature(invite.body.inviter, &bytes, &invite.signature)
}

pub fn validate_and_open_event(
    event: &EventEnvelope,
    group_secret: &[u8; 32],
) -> anyhow::Result<MessagePayload> {
    anyhow::ensure!(
        event.header.version == PROTOCOL_VERSION,
        "unsupported event version"
    );
    let without_id = event.header.without_id();
    let expected_id = compute_event_id(&without_id, &event.ciphertext)?;
    anyhow::ensure!(expected_id == event.header.id, "event ID mismatch");
    verify_signature(
        event.header.author,
        &event_signing_bytes(&event.header, &event.ciphertext)?,
        &event.signature,
    )?;
    let associated_data = postcard::to_stdvec(&without_id).context("encode event header")?;
    let cipher = XChaCha20Poly1305::new(group_secret.into());
    let nonce_value =
        XNonce::try_from(event.header.nonce.as_slice()).expect("XChaCha nonce has a fixed length");
    let plaintext = cipher
        .decrypt(
            &nonce_value,
            chacha20poly1305::aead::Payload {
                msg: &event.ciphertext,
                aad: &associated_data,
            },
        )
        .map_err(|_| anyhow::anyhow!("event authentication failed"))?;
    let payload: MessagePayload =
        postcard::from_bytes(&plaintext).context("decode event payload")?;
    if payload.kind() != event.header.payload_kind {
        bail!("payload kind does not match authenticated header");
    }
    payload.validate()?;
    Ok(payload)
}

fn compute_event_id(header: &EventHeaderWithoutId, ciphertext: &[u8]) -> anyhow::Result<EventId> {
    let header = postcard::to_stdvec(header).context("encode event ID header")?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"opencord-event-v1");
    hasher.update(&header);
    hasher.update(ciphertext);
    Ok(EventId(*hasher.finalize().as_bytes()))
}

fn event_signing_bytes(header: &EventHeader, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut bytes = postcard::to_stdvec(header).context("encode signing header")?;
    bytes.extend_from_slice(ciphertext);
    Ok(bytes)
}

fn sanitize_name(value: &str) -> String {
    let compact = value
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(48)
        .collect::<String>();
    if compact.is_empty() {
        "New peer".to_owned()
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChannelId, GroupId};

    #[test]
    fn signed_encrypted_event_round_trip_and_tamper_detection() {
        let identity = Identity::generate("Alice");
        let group_id = GroupId::random();
        let channel_id = ChannelId::random();
        let secret = [7_u8; 32];
        let payload = MessagePayload::Text {
            body: "encrypted hello".into(),
        };
        let event = identity
            .seal_event(group_id, channel_id, &secret, 1, 123, &payload)
            .unwrap();
        let opened = validate_and_open_event(&event, &secret).unwrap();
        assert!(matches!(opened, MessagePayload::Text { body } if body == "encrypted hello"));

        let mut tampered = event;
        tampered.ciphertext[0] ^= 1;
        assert!(validate_and_open_event(&tampered, &secret).is_err());
    }
}
