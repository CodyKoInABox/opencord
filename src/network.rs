use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce, aead::Aead};
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::{broadcast, mpsc};

use crate::{
    crypto::{Identity, verify_signature},
    model::{Channel, EventEnvelope, GroupId, GroupInventory, PROTOCOL_VERSION, PeerId},
    store::{Store, now_ms},
};

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const SYNC_BATCH_BYTES: usize = 12 * 1024 * 1024;
const GROUP_PROOF_DOMAIN: &[u8] = b"opencord-group-proof-v1";

#[derive(Clone, Debug)]
pub struct OnlinePeer {
    pub id: PeerId,
    pub name: String,
    pub address: SocketAddr,
    pub shared_groups: Vec<GroupId>,
    pub latency_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct NetworkSnapshot {
    pub listen_address: Option<SocketAddr>,
    pub advertised_addresses: Vec<SocketAddr>,
    pub online_peers: Vec<OnlinePeer>,
    pub status: Vec<String>,
    pub generation: u64,
}

#[derive(Default)]
struct Presence {
    peer: Option<OnlinePeer>,
    sessions: usize,
}

pub(crate) struct SharedNetworkState {
    pub(crate) listen_address: SocketAddr,
    advertised_addresses: Mutex<Vec<SocketAddr>>,
    peers: Mutex<BTreeMap<PeerId, Presence>>,
    status: Mutex<VecDeque<String>>,
    generation: AtomicU64,
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl SharedNetworkState {
    pub(crate) fn new(listen_address: SocketAddr, advertised_addresses: Vec<SocketAddr>) -> Self {
        Self {
            listen_address,
            advertised_addresses: Mutex::new(advertised_addresses),
            peers: Mutex::new(BTreeMap::new()),
            status: Mutex::new(VecDeque::from([format!("Listening on {listen_address}")])),
            generation: AtomicU64::new(1),
            waker: Mutex::new(None),
        }
    }

    pub(crate) fn snapshot(&self) -> NetworkSnapshot {
        NetworkSnapshot {
            listen_address: Some(self.listen_address),
            advertised_addresses: self.advertised_addresses.lock().clone(),
            online_peers: self
                .peers
                .lock()
                .values()
                .filter_map(|presence| {
                    (presence.sessions > 0)
                        .then(|| presence.peer.clone())
                        .flatten()
                })
                .collect(),
            status: self.status.lock().iter().cloned().collect(),
            generation: self.generation.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn add_advertised(&self, address: SocketAddr) {
        let mut addresses = self.advertised_addresses.lock();
        if !addresses.contains(&address) {
            addresses.push(address);
            addresses.sort();
            self.bump();
        }
    }

    fn online(&self, peer: OnlinePeer) {
        let mut peers = self.peers.lock();
        let presence = peers.entry(peer.id).or_default();
        presence.peer = Some(peer.clone());
        presence.sessions += 1;
        drop(peers);
        self.note(format!("{} connected from {}", peer.name, peer.address));
    }

    fn offline(&self, peer: PeerId) {
        let offline_name = {
            let mut peers = self.peers.lock();
            peers.get_mut(&peer).and_then(|presence| {
                presence.sessions = presence.sessions.saturating_sub(1);
                (presence.sessions == 0)
                    .then(|| presence.peer.as_ref().map(|info| info.name.clone()))
                    .flatten()
            })
        };
        if let Some(name) = offline_name {
            self.note(format!("{name} went offline"));
        } else {
            self.bump();
        }
    }

    pub(crate) fn note(&self, value: impl Into<String>) {
        let mut status = self.status.lock();
        status.push_back(value.into());
        while status.len() > 12 {
            status.pop_front();
        }
        drop(status);
        self.bump();
    }

    pub(crate) fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        if let Some(waker) = self.waker.lock().clone() {
            waker();
        }
    }

    pub(crate) fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        *self.waker.lock() = Some(waker);
    }
}

#[derive(Clone)]
pub(crate) struct NetworkContext {
    pub identity: Arc<Identity>,
    pub store: Arc<Mutex<Store>>,
    pub state: Arc<SharedNetworkState>,
    pub live_events: broadcast::Sender<LiveEvent>,
    pub voice_packets: broadcast::Sender<VoicePacket>,
    pub incoming_voice: broadcast::Sender<IncomingVoice>,
    pub metadata: broadcast::Sender<Channel>,
    pub screen_packets: broadcast::Sender<ScreenPacket>,
    pub incoming_screen: broadcast::Sender<IncomingScreen>,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveEvent {
    pub event: Arc<EventEnvelope>,
    pub author_name: String,
}

#[derive(Clone, Debug)]
pub(crate) struct VoicePacket {
    pub group_id: GroupId,
    pub bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct IncomingVoice {
    pub peer: PeerId,
    pub group_id: GroupId,
    pub bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub(crate) struct ScreenPacket {
    pub group_id: GroupId,
    pub jpeg: Arc<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct IncomingScreen {
    pub peer: PeerId,
    pub group_id: GroupId,
    pub jpeg: Arc<Vec<u8>>,
}

#[derive(Debug)]
pub(crate) enum NetworkCommand {
    Connect(SocketAddr),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UnsignedPeerHello {
    version: u16,
    nonce: [u8; 32],
    peer: PeerId,
    display_name: String,
    listen_port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PeerHello {
    body: UnsignedPeerHello,
    signature: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GroupProof {
    group_id: GroupId,
    proof: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RangeRequest {
    group_id: GroupId,
    author: PeerId,
    first: u64,
    last: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WireEvent {
    event: EventEnvelope,
    author_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct VoiceDatagram {
    group_id: GroupId,
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Wire {
    Hello(PeerHello),
    GroupProofs(Vec<GroupProof>),
    Channels(Vec<VoiceDatagram>),
    Inventories(Vec<GroupInventory>),
    Requests(Vec<RangeRequest>),
    EventBatch(Vec<WireEvent>),
    SyncDone,
    LiveEvent(WireEvent),
    ChannelUpsert(VoiceDatagram),
    ScreenFrame(VoiceDatagram),
    Ping { sent_at_ms: i64 },
    Pong { sent_at_ms: i64 },
}

pub(crate) async fn run_accept_loop(endpoint: Endpoint, context: NetworkContext) {
    while let Some(incoming) = endpoint.accept().await {
        let context = context.clone();
        tokio::spawn(async move {
            let result = async {
                let connection = incoming.await.context("accept QUIC connection")?;
                let (send, receive) = connection
                    .accept_bi()
                    .await
                    .context("accept protocol stream")?;
                run_session(connection, send, receive, context).await
            }
            .await;
            if let Err(error) = result {
                tracing::debug!(%error, "incoming peer session ended");
            }
        });
    }
}

pub(crate) async fn run_command_loop(
    endpoint: Endpoint,
    context: NetworkContext,
    mut commands: mpsc::UnboundedReceiver<NetworkCommand>,
) {
    while let Some(NetworkCommand::Connect(address)) = commands.recv().await {
        if address == context.state.listen_address {
            continue;
        }
        let endpoint = endpoint.clone();
        let context = context.clone();
        tokio::spawn(async move {
            let result = async {
                let connection = endpoint
                    .connect(address, "opencord.local")
                    .with_context(|| format!("start connection to {address}"))?
                    .await
                    .with_context(|| format!("connect to {address}"))?;
                let (send, receive) = connection.open_bi().await.context("open protocol stream")?;
                run_session(connection, send, receive, context.clone()).await
            }
            .await;
            if let Err(error) = result {
                context
                    .state
                    .note(format!("Could not connect to {address}: {error}"));
            }
        });
    }
}

async fn run_session(
    connection: Connection,
    mut send: SendStream,
    mut receive: RecvStream,
    context: NetworkContext,
) -> anyhow::Result<()> {
    let local_hello = make_hello(&context.identity, context.state.listen_address.port())?;
    send_frame(&mut send, &Wire::Hello(local_hello.clone())).await?;
    let remote_hello = match receive_frame(&mut receive).await? {
        Wire::Hello(hello) => hello,
        _ => bail!("peer did not begin with a hello"),
    };
    verify_hello(&remote_hello)?;
    anyhow::ensure!(
        remote_hello.body.peer != context.identity.peer_id(),
        "refusing self connection"
    );
    anyhow::ensure!(
        !context.store.lock().is_blocked(remote_hello.body.peer)?,
        "peer is blocked locally"
    );

    let groups = context.store.lock().groups()?;
    let local_proofs = groups
        .iter()
        .map(|group| GroupProof {
            group_id: group.id,
            proof: group_proof(
                &group.secret,
                group.id,
                &local_hello.body,
                &remote_hello.body,
            ),
        })
        .collect::<Vec<_>>();
    send_frame(&mut send, &Wire::GroupProofs(local_proofs)).await?;
    let remote_proofs = match receive_frame(&mut receive).await? {
        Wire::GroupProofs(proofs) => proofs,
        _ => bail!("peer did not send group proofs"),
    };
    let shared_groups = groups
        .into_iter()
        .filter_map(|group| {
            let expected = group_proof(
                &group.secret,
                group.id,
                &remote_hello.body,
                &local_hello.body,
            );
            remote_proofs
                .iter()
                .any(|proof| {
                    proof.group_id == group.id && constant_time_eq(&proof.proof, &expected)
                })
                .then_some(group.id)
        })
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        !shared_groups.is_empty(),
        "peer has no authenticated group in common"
    );

    let remote_address = connection.remote_address();
    context.store.lock().remember_peer(
        remote_hello.body.peer,
        &remote_hello.body.display_name,
        Some(&remote_address.to_string()),
    )?;
    context.state.online(OnlinePeer {
        id: remote_hello.body.peer,
        name: remote_hello.body.display_name.clone(),
        address: remote_address,
        shared_groups: shared_groups.iter().copied().collect(),
        latency_ms: None,
    });

    let result = run_authenticated_session(
        &connection,
        &mut send,
        &mut receive,
        &context,
        &remote_hello,
        &shared_groups,
    )
    .await;
    context.state.offline(remote_hello.body.peer);
    result
}

async fn run_authenticated_session(
    connection: &Connection,
    send: &mut SendStream,
    receive: &mut RecvStream,
    context: &NetworkContext,
    remote_hello: &PeerHello,
    shared_groups: &BTreeSet<GroupId>,
) -> anyhow::Result<()> {
    let mut local_channels = Vec::new();
    for group_id in shared_groups {
        let group = context
            .store
            .lock()
            .group(*group_id)?
            .context("shared group disappeared")?;
        for channel in context.store.lock().channels(*group_id)? {
            local_channels.push(seal_voice(
                *group_id,
                &group.secret,
                &postcard::to_stdvec(&channel)?,
            )?);
        }
    }
    send_frame(send, &Wire::Channels(local_channels)).await?;
    let remote_channels = match receive_frame(receive).await? {
        Wire::Channels(channels) => channels,
        _ => bail!("peer did not send channel metadata"),
    };
    for encrypted in remote_channels {
        anyhow::ensure!(
            shared_groups.contains(&encrypted.group_id),
            "channel metadata for unauthorized group"
        );
        let secret = context
            .store
            .lock()
            .group(encrypted.group_id)?
            .context("shared group disappeared")?
            .secret;
        let channel: Channel = postcard::from_bytes(&open_voice(&encrypted, &secret)?)
            .context("decode encrypted channel metadata")?;
        anyhow::ensure!(
            channel.group_id == encrypted.group_id,
            "channel group mismatch"
        );
        context.store.lock().merge_channel(&channel)?;
    }

    let local_inventories = context
        .store
        .lock()
        .inventories()?
        .into_iter()
        .filter(|inventory| shared_groups.contains(&inventory.group_id))
        .collect::<Vec<_>>();
    send_frame(send, &Wire::Inventories(local_inventories.clone())).await?;
    let remote_inventories = match receive_frame(receive).await? {
        Wire::Inventories(inventories) => inventories
            .into_iter()
            .filter(|inventory| shared_groups.contains(&inventory.group_id))
            .collect::<Vec<_>>(),
        _ => bail!("peer did not send inventories"),
    };
    send_frame(
        send,
        &Wire::Requests(missing_ranges(&local_inventories, &remote_inventories)),
    )
    .await?;
    let remote_requests = match receive_frame(receive).await? {
        Wire::Requests(requests) => requests,
        _ => bail!("peer did not send range requests"),
    };
    send_requested_events(send, context, shared_groups, remote_requests).await?;
    receive_requested_events(receive, context, remote_hello).await?;

    let mut live_events = context.live_events.subscribe();
    let mut voice_packets = context.voice_packets.subscribe();
    let mut metadata = context.metadata.subscribe();
    let mut screen_packets = context.screen_packets.subscribe();
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            incoming = receive_frame(receive) => {
                match incoming? {
                    Wire::LiveEvent(wire) => {
                        anyhow::ensure!(shared_groups.contains(&wire.event.header.group_id), "live event for unauthorized group");
                        if context.store.lock().insert_remote_event(&wire.event, &wire.author_name)? {
                            context.state.bump();
                            let _ = context.live_events.send(LiveEvent { event: Arc::new(wire.event), author_name: wire.author_name });
                        }
                    }
                    Wire::ChannelUpsert(encrypted) => {
                        anyhow::ensure!(shared_groups.contains(&encrypted.group_id), "channel metadata for unauthorized group");
                        let secret = context.store.lock().group(encrypted.group_id)?
                            .context("shared group disappeared")?.secret;
                        let channel: Channel = postcard::from_bytes(&open_voice(&encrypted, &secret)?)
                            .context("decode encrypted channel metadata")?;
                        anyhow::ensure!(channel.group_id == encrypted.group_id, "channel group mismatch");
                        if context.store.lock().merge_channel(&channel)? {
                            context.state.bump();
                            let _ = context.metadata.send(channel);
                        }
                    }
                    Wire::ScreenFrame(encrypted) => {
                        anyhow::ensure!(shared_groups.contains(&encrypted.group_id), "screen frame for unauthorized group");
                        let secret = context.store.lock().group(encrypted.group_id)?
                            .context("screen group disappeared")?.secret;
                        let jpeg = open_voice(&encrypted, &secret)?;
                        anyhow::ensure!(jpeg.len() <= 4 * 1024 * 1024, "screen frame exceeds size limit");
                        let _ = context.incoming_screen.send(IncomingScreen {
                            peer: remote_hello.body.peer,
                            group_id: encrypted.group_id,
                            jpeg: Arc::new(jpeg),
                        });
                        context.state.bump();
                    }
                    Wire::Ping { sent_at_ms } => send_frame(send, &Wire::Pong { sent_at_ms }).await?,
                    Wire::Pong { .. } => {}
                    _ => bail!("unexpected record in live session"),
                }
            }
            event = live_events.recv() => {
                match event {
                    Ok(event) if shared_groups.contains(&event.event.header.group_id) && event.event.header.author != remote_hello.body.peer => {
                        send_frame(send, &Wire::LiveEvent(WireEvent { event: (*event.event).clone(), author_name: event.author_name })).await?;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            voice = voice_packets.recv() => {
                if let Ok(packet) = voice
                    && shared_groups.contains(&packet.group_id)
                {
                    let secret = context.store.lock().group(packet.group_id)?
                        .context("voice group disappeared")?.secret;
                    let encrypted = seal_voice(packet.group_id, &secret, &packet.bytes)?;
                    let mut datagram = postcard::to_stdvec(&encrypted).context("encode voice datagram")?;
                    datagram.insert(0, 1);
                    if datagram.len() <= connection.max_datagram_size().unwrap_or(0) {
                        let _ = connection.send_datagram(datagram.into());
                    }
                }
            }
            channel = metadata.recv() => {
                if let Ok(channel) = channel
                    && shared_groups.contains(&channel.group_id)
                {
                    let secret = context.store.lock().group(channel.group_id)?
                        .context("shared group disappeared")?.secret;
                    let encrypted = seal_voice(
                        channel.group_id,
                        &secret,
                        &postcard::to_stdvec(&channel)?,
                    )?;
                    send_frame(send, &Wire::ChannelUpsert(encrypted)).await?;
                }
            }
            screen = screen_packets.recv() => {
                if let Ok(screen) = screen
                    && shared_groups.contains(&screen.group_id)
                {
                    let secret = context.store.lock().group(screen.group_id)?
                        .context("screen group disappeared")?.secret;
                    let encrypted = seal_voice(screen.group_id, &secret, &screen.jpeg)?;
                    send_frame(send, &Wire::ScreenFrame(encrypted)).await?;
                }
            }
            datagram = connection.read_datagram() => {
                let datagram = datagram.context("read voice datagram")?;
                if datagram.first() == Some(&1) {
                    let encrypted: VoiceDatagram = postcard::from_bytes(&datagram[1..]).context("decode voice datagram")?;
                    if shared_groups.contains(&encrypted.group_id) {
                        let secret = context.store.lock().group(encrypted.group_id)?
                            .context("voice group disappeared")?.secret;
                        let bytes = open_voice(&encrypted, &secret)?;
                        let _ = context.incoming_voice.send(IncomingVoice {
                            peer: remote_hello.body.peer,
                            group_id: encrypted.group_id,
                            bytes: Arc::new(bytes),
                        });
                    }
                }
            }
            _ = ping.tick() => send_frame(send, &Wire::Ping { sent_at_ms: now_ms() }).await?,
        }
    }
    Ok(())
}

async fn send_requested_events(
    send: &mut SendStream,
    context: &NetworkContext,
    shared_groups: &BTreeSet<GroupId>,
    requests: Vec<RangeRequest>,
) -> anyhow::Result<()> {
    for request in requests.into_iter().take(10_000) {
        anyhow::ensure!(
            shared_groups.contains(&request.group_id),
            "range request for unauthorized group"
        );
        anyhow::ensure!(
            request.first > 0 && request.first <= request.last,
            "invalid requested range"
        );
        let mut cursor = request.first;
        while cursor <= request.last {
            let events = context.store.lock().events_range(
                request.group_id,
                request.author,
                cursor,
                request.last,
            )?;
            if events.is_empty() {
                break;
            }
            let mut batch = Vec::new();
            let mut bytes = 0_usize;
            for event in events {
                cursor = event.header.author_sequence.saturating_add(1);
                let name = context.store.lock().author_name(event.header.author)?;
                let size = event.ciphertext.len() + event.signature.len() + name.len() + 256;
                if !batch.is_empty() && bytes + size > SYNC_BATCH_BYTES {
                    send_frame(send, &Wire::EventBatch(std::mem::take(&mut batch))).await?;
                    bytes = 0;
                }
                bytes += size;
                batch.push(WireEvent {
                    event,
                    author_name: name,
                });
            }
            if !batch.is_empty() {
                send_frame(send, &Wire::EventBatch(batch)).await?;
            }
            if cursor == 0 {
                break;
            }
        }
    }
    send_frame(send, &Wire::SyncDone).await
}

async fn receive_requested_events(
    receive: &mut RecvStream,
    context: &NetworkContext,
    remote_hello: &PeerHello,
) -> anyhow::Result<()> {
    loop {
        match receive_frame(receive).await? {
            Wire::EventBatch(events) => {
                for wire in events {
                    let name = if wire.author_name.trim().is_empty() {
                        &remote_hello.body.display_name
                    } else {
                        &wire.author_name
                    };
                    if context
                        .store
                        .lock()
                        .insert_remote_event(&wire.event, name)?
                    {
                        context.state.bump();
                    }
                }
            }
            Wire::SyncDone => return Ok(()),
            _ => bail!("unexpected record during event transfer"),
        }
    }
}

fn missing_ranges(local: &[GroupInventory], remote: &[GroupInventory]) -> Vec<RangeRequest> {
    let local_heads = local
        .iter()
        .flat_map(|group| {
            group
                .heads
                .iter()
                .map(move |head| ((group.group_id, head.author), head.sequence))
        })
        .collect::<HashMap<_, _>>();
    remote
        .iter()
        .flat_map(|group| {
            group.heads.iter().filter_map(|head| {
                let local = local_heads
                    .get(&(group.group_id, head.author))
                    .copied()
                    .unwrap_or(0);
                (head.sequence > local).then_some(RangeRequest {
                    group_id: group.group_id,
                    author: head.author,
                    first: local.saturating_add(1),
                    last: head.sequence,
                })
            })
        })
        .collect()
}

fn make_hello(identity: &Identity, listen_port: u16) -> anyhow::Result<PeerHello> {
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("generate handshake nonce: {error}"))?;
    let body = UnsignedPeerHello {
        version: PROTOCOL_VERSION,
        nonce,
        peer: identity.peer_id(),
        display_name: identity.display_name(),
        listen_port,
    };
    let bytes = postcard::to_stdvec(&body).context("encode peer hello")?;
    Ok(PeerHello {
        signature: identity.sign(&bytes),
        body,
    })
}

fn verify_hello(hello: &PeerHello) -> anyhow::Result<()> {
    anyhow::ensure!(
        hello.body.version == PROTOCOL_VERSION,
        "incompatible protocol version"
    );
    anyhow::ensure!(
        !hello.body.display_name.trim().is_empty() && hello.body.display_name.len() <= 192,
        "invalid peer name"
    );
    verify_signature(
        hello.body.peer,
        &postcard::to_stdvec(&hello.body)?,
        &hello.signature,
    )
}

fn group_proof(
    secret: &[u8; 32],
    group_id: GroupId,
    sender: &UnsignedPeerHello,
    receiver: &UnsignedPeerHello,
) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts 32-byte keys");
    mac.update(GROUP_PROOF_DOMAIN);
    mac.update(&group_id.0);
    mac.update(&sender.peer.0);
    mac.update(&sender.nonce);
    mac.update(&receiver.peer.0);
    mac.update(&receiver.nonce);
    mac.finalize().into_bytes().into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    use subtle::ConstantTimeEq;
    bool::from(left.ct_eq(right))
}

fn seal_voice(group_id: GroupId, secret: &[u8; 32], bytes: &[u8]) -> anyhow::Result<VoiceDatagram> {
    let mut nonce = [0_u8; 24];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("generate voice nonce: {error}"))?;
    let nonce_value = XNonce::try_from(nonce.as_slice()).expect("XChaCha nonce has a fixed length");
    let ciphertext = XChaCha20Poly1305::new(secret.into())
        .encrypt(
            &nonce_value,
            chacha20poly1305::aead::Payload {
                msg: bytes,
                aad: &group_id.0,
            },
        )
        .map_err(|_| anyhow::anyhow!("voice encryption failed"))?;
    Ok(VoiceDatagram {
        group_id,
        nonce,
        ciphertext,
    })
}

fn open_voice(datagram: &VoiceDatagram, secret: &[u8; 32]) -> anyhow::Result<Vec<u8>> {
    let nonce_value =
        XNonce::try_from(datagram.nonce.as_slice()).expect("XChaCha nonce has a fixed length");
    XChaCha20Poly1305::new(secret.into())
        .decrypt(
            &nonce_value,
            chacha20poly1305::aead::Payload {
                msg: &datagram.ciphertext,
                aad: &datagram.group_id.0,
            },
        )
        .map_err(|_| anyhow::anyhow!("voice authentication failed"))
}

async fn send_frame(send: &mut SendStream, frame: &Wire) -> anyhow::Result<()> {
    let bytes = postcard::to_stdvec(frame).context("encode wire frame")?;
    anyhow::ensure!(
        bytes.len() <= MAX_FRAME_BYTES,
        "wire frame exceeds size limit"
    );
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn receive_frame(receive: &mut RecvStream) -> anyhow::Result<Wire> {
    let mut length = [0_u8; 4];
    receive
        .read_exact(&mut length)
        .await
        .context("read wire frame length")?;
    let length = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(
        length <= MAX_FRAME_BYTES,
        "incoming wire frame exceeds size limit"
    );
    let mut bytes = vec![0_u8; length];
    receive
        .read_exact(&mut bytes)
        .await
        .context("read wire frame")?;
    postcard::from_bytes(&bytes).context("decode wire frame")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthorHead, GroupInventory};

    #[test]
    fn missing_ranges_request_only_remote_suffixes() {
        let group = GroupId::random();
        let alice = PeerId::random();
        let local = vec![GroupInventory {
            group_id: group,
            heads: vec![AuthorHead {
                author: alice,
                sequence: 2,
            }],
        }];
        let remote = vec![GroupInventory {
            group_id: group,
            heads: vec![AuthorHead {
                author: alice,
                sequence: 7,
            }],
        }];
        let ranges = missing_ranges(&local, &remote);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].first, 3);
        assert_eq!(ranges[0].last, 7);
    }

    #[test]
    fn voice_payloads_are_group_encrypted_and_authenticated() {
        let group = GroupId::random();
        let secret = [9_u8; 32];
        let encrypted = seal_voice(group, &secret, b"opus packet").unwrap();
        assert_eq!(open_voice(&encrypted, &secret).unwrap(), b"opus packet");
        let mut tampered = encrypted;
        tampered.ciphertext[0] ^= 1;
        assert!(open_voice(&tampered, &secret).is_err());
    }
}
