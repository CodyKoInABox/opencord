use std::{fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

macro_rules! byte_id {
    ($name:ident, $size:expr) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Default,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            Serialize,
            Deserialize,
        )]
        pub struct $name(pub [u8; $size]);

        impl $name {
            pub fn random() -> Self {
                let mut value = [0_u8; $size];
                getrandom::fill(&mut value).expect("operating system randomness unavailable");
                Self(value)
            }

            pub fn short(&self) -> String {
                URL_SAFE_NO_PAD.encode(&self.0)[..8].to_owned()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = base64::DecodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let bytes = URL_SAFE_NO_PAD.decode(value)?;
                let bytes: [u8; $size] = bytes
                    .try_into()
                    .map_err(|_| base64::DecodeError::InvalidLength(value.len()))?;
                Ok(Self(bytes))
            }
        }
    };
}

byte_id!(GroupId, 16);
byte_id!(ChannelId, 16);
byte_id!(EventId, 32);
byte_id!(PeerId, 32);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    pub secret: [u8; 32],
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub group_id: GroupId,
    pub name: String,
    pub position: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InviteChannel {
    pub id: ChannelId,
    pub name: String,
    pub position: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnsignedGroupInvite {
    pub version: u16,
    pub group_id: GroupId,
    pub group_name: String,
    pub group_secret: [u8; 32],
    pub channels: Vec<InviteChannel>,
    pub inviter: PeerId,
    pub inviter_name: String,
    pub endpoints: Vec<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupInvite {
    pub body: UnsignedGroupInvite,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessagePayload {
    Text {
        body: String,
    },
    Attachment {
        file_name: String,
        mime: String,
        bytes: Vec<u8>,
        caption: String,
    },
    System {
        body: String,
    },
}

impl MessagePayload {
    pub fn kind(&self) -> u8 {
        match self {
            Self::Text { .. } => 1,
            Self::Attachment { .. } => 2,
            Self::System { .. } => 3,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Text { body } | Self::System { body } => {
                anyhow::ensure!(!body.trim().is_empty(), "message is empty");
                anyhow::ensure!(
                    body.len() <= MAX_TEXT_BYTES,
                    "message exceeds {MAX_TEXT_BYTES} bytes"
                );
            }
            Self::Attachment {
                file_name,
                bytes,
                caption,
                ..
            } => {
                anyhow::ensure!(!file_name.trim().is_empty(), "attachment has no file name");
                anyhow::ensure!(
                    bytes.len() <= MAX_ATTACHMENT_BYTES,
                    "attachment exceeds {MAX_ATTACHMENT_BYTES} bytes"
                );
                anyhow::ensure!(
                    caption.len() <= MAX_TEXT_BYTES,
                    "caption exceeds {MAX_TEXT_BYTES} bytes"
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventHeaderWithoutId {
    pub version: u16,
    pub group_id: GroupId,
    pub channel_id: ChannelId,
    pub author: PeerId,
    pub author_sequence: u64,
    pub sent_at_ms: i64,
    pub payload_kind: u8,
    pub nonce: [u8; 24],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventHeader {
    pub version: u16,
    pub id: EventId,
    pub group_id: GroupId,
    pub channel_id: ChannelId,
    pub author: PeerId,
    pub author_sequence: u64,
    pub sent_at_ms: i64,
    pub payload_kind: u8,
    pub nonce: [u8; 24],
}

impl EventHeader {
    pub fn without_id(&self) -> EventHeaderWithoutId {
        EventHeaderWithoutId {
            version: self.version,
            group_id: self.group_id,
            channel_id: self.channel_id,
            author: self.author,
            author_sequence: self.author_sequence,
            sent_at_ms: self.sent_at_ms,
            payload_kind: self.payload_kind,
            nonce: self.nonce,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub header: EventHeader,
    pub ciphertext: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct TimelineEntry {
    pub event: EventEnvelope,
    pub author_name: String,
    pub payload: MessagePayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorHead {
    pub author: PeerId,
    pub sequence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupInventory {
    pub group_id: GroupId,
    pub heads: Vec<AuthorHead>,
}
