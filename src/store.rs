use std::{
    collections::BTreeMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    crypto::{Identity, validate_and_open_event, verify_invite},
    model::{
        AuthorHead, Channel, ChannelId, EventEnvelope, EventHeader, EventId, Group, GroupId,
        GroupInventory, GroupInvite, InviteChannel, MessagePayload, PROTOCOL_VERSION, PeerId,
        TimelineEntry, UnsignedGroupInvite,
    },
};

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let connection =
            Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        let store = Self { connection };
        store.configure()?;
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let store = Self {
            connection: Connection::open_in_memory()?,
        };
        store.configure()?;
        store.migrate()?;
        Ok(store)
    }

    fn configure(&self) -> anyhow::Result<()> {
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(())
    }

    fn migrate(&self) -> anyhow::Result<()> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS groups (
                id BLOB PRIMARY KEY CHECK(length(id) = 16),
                name TEXT NOT NULL,
                secret BLOB NOT NULL CHECK(length(secret) = 32),
                created_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS channels (
                id BLOB PRIMARY KEY CHECK(length(id) = 16),
                group_id BLOB NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                position INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS channels_group_position ON channels(group_id, position);
             CREATE TABLE IF NOT EXISTS peers (
                id BLOB PRIMARY KEY CHECK(length(id) = 32),
                display_name TEXT NOT NULL,
                last_seen_ms INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS peer_endpoints (
                peer_id BLOB NOT NULL REFERENCES peers(id) ON DELETE CASCADE,
                address TEXT NOT NULL,
                last_seen_ms INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(peer_id, address)
             );
             CREATE TABLE IF NOT EXISTS events (
                id BLOB PRIMARY KEY CHECK(length(id) = 32),
                group_id BLOB NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                channel_id BLOB NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
                author BLOB NOT NULL CHECK(length(author) = 32),
                author_sequence INTEGER NOT NULL CHECK(author_sequence > 0),
                sent_at_ms INTEGER NOT NULL,
                payload_kind INTEGER NOT NULL,
                nonce BLOB NOT NULL CHECK(length(nonce) = 24),
                ciphertext BLOB NOT NULL,
                signature BLOB NOT NULL CHECK(length(signature) = 64),
                UNIQUE(group_id, author, author_sequence)
             );
             CREATE INDEX IF NOT EXISTS events_timeline ON events(channel_id, sent_at_ms, author, author_sequence);
             CREATE INDEX IF NOT EXISTS events_sync ON events(group_id, author, author_sequence);",
        )?;
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS blocked_peers (
                peer_id BLOB PRIMARY KEY CHECK(length(peer_id) = 32),
                reason TEXT NOT NULL,
                blocked_at_ms INTEGER NOT NULL
             );",
        )?;
        Ok(())
    }

    pub fn create_group(&mut self, name: &str) -> anyhow::Result<(Group, Channel)> {
        let name = clean_label(name, "New group", 64);
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|error| anyhow::anyhow!("generate group key: {error}"))?;
        let group = Group {
            id: GroupId::random(),
            name,
            secret,
            created_at_ms: now_ms(),
        };
        let channel = Channel {
            id: ChannelId::random(),
            group_id: group.id,
            name: "general".into(),
            position: 0,
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO groups(id, name, secret, created_at_ms) VALUES(?1, ?2, ?3, ?4)",
            params![
                group.id.0.as_slice(),
                group.name,
                group.secret.as_slice(),
                group.created_at_ms
            ],
        )?;
        tx.execute(
            "INSERT INTO channels(id, group_id, name, position) VALUES(?1, ?2, ?3, ?4)",
            params![
                channel.id.0.as_slice(),
                group.id.0.as_slice(),
                channel.name,
                channel.position
            ],
        )?;
        tx.commit()?;
        Ok((group, channel))
    }

    pub fn groups(&self) -> anyhow::Result<Vec<Group>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, secret, created_at_ms FROM groups ORDER BY created_at_ms, name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Group {
                id: GroupId(blob_array(row.get_ref(0)?.as_blob()?)?),
                name: row.get(1)?,
                secret: blob_array(row.get_ref(2)?.as_blob()?)?,
                created_at_ms: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn group(&self, group_id: GroupId) -> anyhow::Result<Option<Group>> {
        self.connection
            .query_row(
                "SELECT id, name, secret, created_at_ms FROM groups WHERE id = ?1",
                params![group_id.0.as_slice()],
                |row| {
                    Ok(Group {
                        id: GroupId(blob_array(row.get_ref(0)?.as_blob()?)?),
                        name: row.get(1)?,
                        secret: blob_array(row.get_ref(2)?.as_blob()?)?,
                        created_at_ms: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn channels(&self, group_id: GroupId) -> anyhow::Result<Vec<Channel>> {
        let mut statement = self.connection.prepare(
            "SELECT id, group_id, name, position FROM channels WHERE group_id = ?1 ORDER BY position, name",
        )?;
        let rows = statement.query_map(params![group_id.0.as_slice()], |row| {
            Ok(Channel {
                id: ChannelId(blob_array(row.get_ref(0)?.as_blob()?)?),
                group_id: GroupId(blob_array(row.get_ref(1)?.as_blob()?)?),
                name: row.get(2)?,
                position: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn append(
        &mut self,
        identity: &Identity,
        channel_id: ChannelId,
        payload: &MessagePayload,
    ) -> anyhow::Result<EventEnvelope> {
        payload.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (group_id, secret): (Vec<u8>, Vec<u8>) = tx.query_row(
            "SELECT g.id, g.secret FROM groups g JOIN channels c ON c.group_id = g.id WHERE c.id = ?1",
            params![channel_id.0.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).context("channel does not exist")?;
        let group_id = GroupId(blob_array(&group_id)?);
        let secret: [u8; 32] = blob_array(&secret)?;
        let next_sequence: i64 = tx.query_row(
            "SELECT COALESCE(MAX(author_sequence), 0) + 1 FROM events WHERE group_id = ?1 AND author = ?2",
            params![group_id.0.as_slice(), identity.peer_id().0.as_slice()],
            |row| row.get(0),
        )?;
        let next_sequence = sql_to_u64(next_sequence)?;
        let event = identity.seal_event(
            group_id,
            channel_id,
            &secret,
            next_sequence,
            now_ms(),
            payload,
        )?;
        insert_event(&tx, &event)?;
        tx.execute(
            "INSERT INTO peers(id, display_name, last_seen_ms) VALUES(?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name, last_seen_ms=excluded.last_seen_ms",
            params![identity.peer_id().0.as_slice(), identity.display_name(), now_ms()],
        )?;
        tx.commit()?;
        Ok(event)
    }

    pub fn insert_remote_event(
        &mut self,
        event: &EventEnvelope,
        author_name: &str,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            !self.is_blocked(event.header.author)?,
            "event author is blocked"
        );
        let group = self
            .group(event.header.group_id)?
            .context("event is for an unknown group")?;
        validate_and_open_event(event, &group.secret)?;
        let channel_exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM channels WHERE id = ?1 AND group_id = ?2)",
            params![
                event.header.channel_id.0.as_slice(),
                event.header.group_id.0.as_slice()
            ],
            |row| row.get(0),
        )?;
        anyhow::ensure!(channel_exists, "event is for an unknown channel");

        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<Vec<u8>> = tx
            .query_row(
                "SELECT id FROM events WHERE group_id=?1 AND author=?2 AND author_sequence=?3",
                params![
                    event.header.group_id.0.as_slice(),
                    event.header.author.0.as_slice(),
                    sql_u64(event.header.author_sequence)?
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            anyhow::ensure!(
                existing.as_slice() == event.header.id.0,
                "author sequence equivocation detected"
            );
            return Ok(false);
        }
        insert_event(&tx, event)?;
        tx.execute(
            "INSERT INTO peers(id, display_name, last_seen_ms) VALUES(?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name, last_seen_ms=excluded.last_seen_ms",
            params![event.header.author.0.as_slice(), clean_label(author_name, "Unknown peer", 48), now_ms()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn timeline(
        &self,
        channel_id: ChannelId,
        limit: usize,
    ) -> anyhow::Result<Vec<TimelineEntry>> {
        let group_secret: Vec<u8> = self.connection.query_row(
            "SELECT g.secret FROM groups g JOIN channels c ON c.group_id=g.id WHERE c.id=?1",
            params![channel_id.0.as_slice()],
            |row| row.get(0),
        )?;
        let group_secret: [u8; 32] = blob_array(&group_secret)?;
        let mut statement = self.connection.prepare(
            "SELECT e.id,e.group_id,e.channel_id,e.author,e.author_sequence,e.sent_at_ms,e.payload_kind,e.nonce,e.ciphertext,e.signature,
                    COALESCE(p.display_name, 'Unknown peer')
             FROM events e LEFT JOIN peers p ON p.id=e.author WHERE e.channel_id=?1
             ORDER BY e.sent_at_ms DESC,e.author DESC,e.author_sequence DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![channel_id.0.as_slice(), limit.min(2_000) as i64],
            |row| {
                let event = event_from_row(row)?;
                let author_name: String = row.get(10)?;
                Ok((event, author_name))
            },
        )?;
        let mut entries = Vec::new();
        for row in rows {
            let (event, author_name) = row?;
            let payload = validate_and_open_event(&event, &group_secret)?;
            entries.push(TimelineEntry {
                event,
                author_name,
                payload,
            });
        }
        entries.reverse();
        Ok(entries)
    }

    pub fn inventories(&self) -> anyhow::Result<Vec<GroupInventory>> {
        let mut result = Vec::new();
        for group in self.groups()? {
            let mut statement = self.connection.prepare(
                "SELECT author, author_sequence FROM events WHERE group_id=?1 ORDER BY author, author_sequence",
            )?;
            let rows = statement
                .query_map(params![group.id.0.as_slice()], |row| {
                    Ok((
                        PeerId(blob_array(row.get_ref(0)?.as_blob()?)?),
                        sql_to_u64(row.get(1)?)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut contiguous = BTreeMap::<PeerId, u64>::new();
            for (author, sequence) in rows {
                let head = contiguous.entry(author).or_default();
                if sequence == head.saturating_add(1) {
                    *head = sequence;
                }
            }
            let heads = contiguous
                .into_iter()
                .map(|(author, sequence)| AuthorHead { author, sequence })
                .collect();
            result.push(GroupInventory {
                group_id: group.id,
                heads,
            });
        }
        Ok(result)
    }

    pub fn events_range(
        &self,
        group_id: GroupId,
        author: PeerId,
        first: u64,
        last: u64,
    ) -> anyhow::Result<Vec<EventEnvelope>> {
        anyhow::ensure!(first > 0 && first <= last, "invalid event range");
        let mut statement = self.connection.prepare(
            "SELECT id,group_id,channel_id,author,author_sequence,sent_at_ms,payload_kind,nonce,ciphertext,signature
             FROM events WHERE group_id=?1 AND author=?2 AND author_sequence BETWEEN ?3 AND ?4 ORDER BY author_sequence LIMIT 128",
        )?;
        let rows = statement.query_map(
            params![
                group_id.0.as_slice(),
                author.0.as_slice(),
                sql_u64(first)?,
                sql_u64(last)?
            ],
            event_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn author_name(&self, author: PeerId) -> anyhow::Result<String> {
        Ok(self
            .connection
            .query_row(
                "SELECT display_name FROM peers WHERE id=?1",
                params![author.0.as_slice()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| format!("Peer {}", author.short())))
    }

    pub fn build_invite(
        &self,
        identity: &Identity,
        group_id: GroupId,
        endpoints: Vec<String>,
    ) -> anyhow::Result<String> {
        let group = self.group(group_id)?.context("group not found")?;
        let channels = self
            .channels(group_id)?
            .into_iter()
            .map(|channel| InviteChannel {
                id: channel.id,
                name: channel.name,
                position: channel.position,
            })
            .collect();
        let body = UnsignedGroupInvite {
            version: PROTOCOL_VERSION,
            group_id,
            group_name: group.name,
            group_secret: group.secret,
            channels,
            inviter: identity.peer_id(),
            inviter_name: identity.display_name().to_owned(),
            endpoints,
            created_at_ms: now_ms(),
        };
        let invite = identity.sign_invite(body)?;
        let bytes = postcard::to_stdvec(&invite).context("encode invite")?;
        Ok(format!(
            "opencord://join/{}",
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
        ))
    }

    pub fn import_invite(&mut self, value: &str) -> anyhow::Result<GroupInvite> {
        let encoded = value
            .trim()
            .strip_prefix("opencord://join/")
            .context("invite must start with opencord://join/")?;
        let bytes =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, encoded)
                .context("decode invite")?;
        let invite: GroupInvite = postcard::from_bytes(&bytes).context("parse invite")?;
        verify_invite(&invite)?;
        anyhow::ensure!(!invite.body.channels.is_empty(), "invite has no channels");
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing_secret: Option<Vec<u8>> = tx
            .query_row(
                "SELECT secret FROM groups WHERE id=?1",
                params![invite.body.group_id.0.as_slice()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(secret) = existing_secret {
            anyhow::ensure!(
                secret.as_slice() == invite.body.group_secret,
                "invite conflicts with existing group key"
            );
        } else {
            tx.execute(
                "INSERT INTO groups(id,name,secret,created_at_ms) VALUES(?1,?2,?3,?4)",
                params![
                    invite.body.group_id.0.as_slice(),
                    clean_label(&invite.body.group_name, "Imported group", 64),
                    invite.body.group_secret.as_slice(),
                    invite.body.created_at_ms
                ],
            )?;
        }
        for channel in &invite.body.channels {
            tx.execute(
                "INSERT OR IGNORE INTO channels(id,group_id,name,position) VALUES(?1,?2,?3,?4)",
                params![
                    channel.id.0.as_slice(),
                    invite.body.group_id.0.as_slice(),
                    clean_label(&channel.name, "channel", 64),
                    channel.position
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO peers(id,display_name,last_seen_ms) VALUES(?1,?2,0)
             ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name",
            params![
                invite.body.inviter.0.as_slice(),
                clean_label(&invite.body.inviter_name, "Inviter", 48)
            ],
        )?;
        for endpoint in &invite.body.endpoints {
            if endpoint.parse::<std::net::SocketAddr>().is_ok() {
                tx.execute(
                    "INSERT OR IGNORE INTO peer_endpoints(peer_id,address,last_seen_ms) VALUES(?1,?2,0)",
                    params![invite.body.inviter.0.as_slice(), endpoint],
                )?;
            }
        }
        tx.commit()?;
        Ok(invite)
    }

    pub fn remember_peer(
        &mut self,
        peer: PeerId,
        name: &str,
        endpoint: Option<&str>,
    ) -> anyhow::Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO peers(id,display_name,last_seen_ms) VALUES(?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,last_seen_ms=excluded.last_seen_ms",
            params![peer.0.as_slice(), clean_label(name, "Unknown peer", 48), now_ms()],
        )?;
        if let Some(endpoint) =
            endpoint.filter(|value| value.parse::<std::net::SocketAddr>().is_ok())
        {
            tx.execute(
                "INSERT INTO peer_endpoints(peer_id,address,last_seen_ms) VALUES(?1,?2,?3)
                 ON CONFLICT(peer_id,address) DO UPDATE SET last_seen_ms=excluded.last_seen_ms",
                params![peer.0.as_slice(), endpoint, now_ms()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn known_endpoints(&self) -> anyhow::Result<Vec<(PeerId, String)>> {
        let mut statement = self
            .connection
            .prepare("SELECT peer_id,address FROM peer_endpoints ORDER BY last_seen_ms DESC")?;
        let rows = statement.query_map([], |row| {
            let bytes: Vec<u8> = row.get(0)?;
            Ok((PeerId(blob_array(&bytes)?), row.get(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn peers(&self) -> anyhow::Result<BTreeMap<PeerId, String>> {
        let mut statement = self
            .connection
            .prepare("SELECT id,display_name FROM peers ORDER BY display_name")?;
        let rows = statement.query_map([], |row| {
            let bytes: Vec<u8> = row.get(0)?;
            Ok((PeerId(blob_array(&bytes)?), row.get(1)?))
        })?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(Into::into)
    }

    pub fn block_peer(&mut self, peer: PeerId, reason: &str) -> anyhow::Result<()> {
        self.connection.execute(
            "INSERT INTO blocked_peers(peer_id,reason,blocked_at_ms) VALUES(?1,?2,?3)
             ON CONFLICT(peer_id) DO UPDATE SET reason=excluded.reason,blocked_at_ms=excluded.blocked_at_ms",
            params![peer.0.as_slice(), clean_label(reason, "Blocked locally", 256), now_ms()],
        )?;
        Ok(())
    }

    pub fn unblock_peer(&mut self, peer: PeerId) -> anyhow::Result<()> {
        self.connection.execute(
            "DELETE FROM blocked_peers WHERE peer_id=?1",
            params![peer.0.as_slice()],
        )?;
        Ok(())
    }

    pub fn is_blocked(&self, peer: PeerId) -> anyhow::Result<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM blocked_peers WHERE peer_id=?1)",
                params![peer.0.as_slice()],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn create_channel(&mut self, group_id: GroupId, name: &str) -> anyhow::Result<Channel> {
        let name = clean_label(name, "new-channel", 64)
            .to_lowercase()
            .chars()
            .map(|character| {
                if character.is_whitespace() {
                    '-'
                } else {
                    character
                }
            })
            .collect::<String>();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM channels WHERE group_id=?1",
            params![group_id.0.as_slice()],
            |row| row.get(0),
        )?;
        let channel = Channel {
            id: ChannelId::random(),
            group_id,
            name,
            position: u32::try_from(position).context("channel position overflow")?,
        };
        tx.execute(
            "INSERT INTO channels(id,group_id,name,position) VALUES(?1,?2,?3,?4)",
            params![
                channel.id.0.as_slice(),
                group_id.0.as_slice(),
                channel.name,
                channel.position
            ],
        )?;
        tx.commit()?;
        Ok(channel)
    }

    pub fn merge_channel(&mut self, channel: &Channel) -> anyhow::Result<bool> {
        let group_exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM groups WHERE id=?1)",
            params![channel.group_id.0.as_slice()],
            |row| row.get(0),
        )?;
        anyhow::ensure!(group_exists, "channel belongs to an unknown group");
        let changed = self.connection.execute(
            "INSERT OR IGNORE INTO channels(id,group_id,name,position) VALUES(?1,?2,?3,?4)",
            params![
                channel.id.0.as_slice(),
                channel.group_id.0.as_slice(),
                clean_label(&channel.name, "channel", 64),
                channel.position
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn event_count(&self, group_id: GroupId) -> anyhow::Result<u64> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM events WHERE group_id=?1",
            params![group_id.0.as_slice()],
            |row| row.get(0),
        )?;
        Ok(sql_to_u64(count)?)
    }
}

fn insert_event(connection: &Connection, event: &EventEnvelope) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO events(id,group_id,channel_id,author,author_sequence,sent_at_ms,payload_kind,nonce,ciphertext,signature)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            event.header.id.0.as_slice(), event.header.group_id.0.as_slice(), event.header.channel_id.0.as_slice(),
            event.header.author.0.as_slice(), sql_u64(event.header.author_sequence)?, event.header.sent_at_ms,
            event.header.payload_kind, event.header.nonce.as_slice(), event.ciphertext, event.signature,
        ],
    )?;
    Ok(())
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventEnvelope> {
    Ok(EventEnvelope {
        header: EventHeader {
            id: EventId(blob_array(row.get_ref(0)?.as_blob()?)?),
            version: PROTOCOL_VERSION,
            group_id: GroupId(blob_array(row.get_ref(1)?.as_blob()?)?),
            channel_id: ChannelId(blob_array(row.get_ref(2)?.as_blob()?)?),
            author: PeerId(blob_array(row.get_ref(3)?.as_blob()?)?),
            author_sequence: sql_to_u64(row.get(4)?)?,
            sent_at_ms: row.get(5)?,
            payload_kind: row.get(6)?,
            nonce: blob_array(row.get_ref(7)?.as_blob()?)?,
        },
        ciphertext: row.get(8)?,
        signature: row.get(9)?,
    })
}

fn blob_array<const N: usize>(bytes: &[u8]) -> rusqlite::Result<[u8; N]> {
    bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            bytes.len(),
            rusqlite::types::Type::Blob,
            format!("expected {N} bytes").into(),
        )
    })
}

fn sql_u64(value: u64) -> anyhow::Result<i64> {
    i64::try_from(value).context("integer exceeds SQLite range")
}

fn sql_to_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Integer,
            format!("negative integer cannot be a sequence: {value}").into(),
        )
    })
}

fn clean_label(value: &str, fallback: &str, max: usize) -> String {
    let value = value
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>();
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_allows_fresh_store_to_rebuild_encrypted_history() {
        let alice = Identity::generate("Alice");
        let mut source = Store::open_in_memory().unwrap();
        let (group, channel) = source.create_group("Friends").unwrap();
        let first = source
            .append(
                &alice,
                channel.id,
                &MessagePayload::Text {
                    body: "first".into(),
                },
            )
            .unwrap();
        let second = source
            .append(
                &alice,
                channel.id,
                &MessagePayload::Text {
                    body: "second".into(),
                },
            )
            .unwrap();
        let invite = source
            .build_invite(&alice, group.id, vec!["127.0.0.1:40123".into()])
            .unwrap();

        let mut fresh = Store::open_in_memory().unwrap();
        fresh.import_invite(&invite).unwrap();
        assert!(fresh.insert_remote_event(&first, "Alice").unwrap());
        assert!(fresh.insert_remote_event(&second, "Alice").unwrap());
        assert!(!fresh.insert_remote_event(&second, "Alice").unwrap());
        let timeline = fresh.timeline(channel.id, 50).unwrap();
        assert_eq!(timeline.len(), 2);
        assert!(matches!(&timeline[0].payload, MessagePayload::Text { body } if body == "first"));
        assert_eq!(fresh.event_count(group.id).unwrap(), 2);
    }

    #[test]
    fn inventories_and_ranges_are_per_author() {
        let alice = Identity::generate("Alice");
        let bob = Identity::generate("Bob");
        let mut store = Store::open_in_memory().unwrap();
        let (group, channel) = store.create_group("Mesh").unwrap();
        store
            .append(
                &alice,
                channel.id,
                &MessagePayload::Text { body: "a".into() },
            )
            .unwrap();
        store
            .append(&bob, channel.id, &MessagePayload::Text { body: "b".into() })
            .unwrap();
        let inventory = store.inventories().unwrap().remove(0);
        assert_eq!(inventory.heads.len(), 2);
        assert_eq!(
            store
                .events_range(group.id, alice.peer_id(), 1, 99)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn blocked_authors_cannot_append_remote_events() {
        let alice = Identity::generate("Alice");
        let bob = Identity::generate("Bob");
        let mut source = Store::open_in_memory().unwrap();
        let (group, channel) = source.create_group("Moderated").unwrap();
        let invite = source.build_invite(&alice, group.id, vec![]).unwrap();

        let mut remote = Store::open_in_memory().unwrap();
        remote.import_invite(&invite).unwrap();
        let event = remote
            .append(
                &bob,
                channel.id,
                &MessagePayload::Text {
                    body: "blocked".into(),
                },
            )
            .unwrap();
        source.block_peer(bob.peer_id(), "test").unwrap();
        assert!(source.insert_remote_event(&event, "Bob").is_err());
        source.unblock_peer(bob.peer_id()).unwrap();
        assert!(source.insert_remote_event(&event, "Bob").unwrap());
    }
}
