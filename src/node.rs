use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

use anyhow::Context;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::Mutex;
use quinn::{ClientConfig, Endpoint, ServerConfig, crypto::rustls::QuicClientConfig};
use rcgen::CertifiedKey;
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use tokio::{
    runtime::Runtime,
    sync::{broadcast, mpsc},
};

use crate::{
    Identity,
    model::{
        Channel, ChannelId, EventEnvelope, Group, GroupId, GroupInvite, MessagePayload,
        TimelineEntry,
    },
    network::{
        IncomingScreen, IncomingVoice, LiveEvent, NetworkCommand, NetworkContext, NetworkSnapshot,
        ScreenPacket, SharedNetworkState, VoicePacket, run_accept_loop, run_command_loop,
    },
    store::Store,
};

const MDNS_SERVICE: &str = "_opencord._udp.local.";

#[derive(Clone, Debug)]
pub struct NodeOptions {
    pub data_dir: PathBuf,
    pub display_name: Option<String>,
    pub listen: SocketAddr,
    pub enable_discovery: bool,
}

impl NodeOptions {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            display_name: None,
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            enable_discovery: true,
        }
    }
}

pub struct Node {
    identity: Arc<Identity>,
    store: Arc<Mutex<Store>>,
    runtime: Runtime,
    endpoint: Endpoint,
    state: Arc<SharedNetworkState>,
    commands: mpsc::UnboundedSender<NetworkCommand>,
    live_events: broadcast::Sender<LiveEvent>,
    voice_packets: broadcast::Sender<VoicePacket>,
    incoming_voice: broadcast::Sender<IncomingVoice>,
    metadata: broadcast::Sender<Channel>,
    screen_packets: broadcast::Sender<ScreenPacket>,
    incoming_screen: broadcast::Sender<IncomingScreen>,
    discovery: Option<ServiceDaemon>,
    port_mapping: Arc<Mutex<Option<PortMapping>>>,
}

impl Node {
    pub fn start(options: NodeOptions) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&options.data_dir)
            .with_context(|| format!("create {}", options.data_dir.display()))?;
        let identity = Arc::new(Identity::load_or_create(
            &options.data_dir.join("identity.ocid"),
            options.display_name.as_deref(),
        )?);
        let store = Arc::new(Mutex::new(Store::open(
            &options.data_dir.join("opencord.sqlite3"),
        )?));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("opencord-net")
            .build()
            .context("start network runtime")?;
        let runtime_guard = runtime.enter();
        let mut server_config = make_server_config()?;
        let transport =
            Arc::get_mut(&mut server_config.transport).expect("new server transport is unique");
        transport.max_concurrent_bidi_streams(32_u32.into());
        transport.datagram_receive_buffer_size(Some(2 * 1024 * 1024));
        transport.datagram_send_buffer_size(2 * 1024 * 1024);
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(10)));
        let mut endpoint =
            Endpoint::server(server_config, options.listen).context("bind peer UDP socket")?;
        endpoint.set_default_client_config(make_client_config()?);
        let listen_address = endpoint.local_addr()?;
        let state = Arc::new(SharedNetworkState::new(
            listen_address,
            local_addresses(listen_address.port()),
        ));
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (live_events, _) = broadcast::channel(512);
        let (voice_packets, _) = broadcast::channel(256);
        let (incoming_voice, _) = broadcast::channel(256);
        let (metadata, _) = broadcast::channel(64);
        let (screen_packets, _) = broadcast::channel(8);
        let (incoming_screen, _) = broadcast::channel(8);
        let context = NetworkContext {
            identity: identity.clone(),
            store: store.clone(),
            state: state.clone(),
            live_events: live_events.clone(),
            voice_packets: voice_packets.clone(),
            incoming_voice: incoming_voice.clone(),
            metadata: metadata.clone(),
            screen_packets: screen_packets.clone(),
            incoming_screen: incoming_screen.clone(),
        };
        runtime.spawn(run_accept_loop(endpoint.clone(), context.clone()));
        runtime.spawn(run_command_loop(endpoint.clone(), context, command_rx));
        drop(runtime_guard);

        for (_, address) in store.lock().known_endpoints()? {
            if let Ok(address) = address.parse() {
                let _ = commands.send(NetworkCommand::Connect(address));
            }
        }
        let discovery = if options.enable_discovery {
            match start_discovery(
                identity.peer_id(),
                listen_address.port(),
                commands.clone(),
                state.clone(),
            ) {
                Ok(discovery) => Some(discovery),
                Err(error) => {
                    state.note(format!("LAN discovery unavailable: {error}"));
                    None
                }
            }
        } else {
            None
        };
        let port_mapping = Arc::new(Mutex::new(None));
        if !listen_address.ip().is_loopback() {
            start_port_mapping(listen_address.port(), state.clone(), port_mapping.clone());
        }
        Ok(Self {
            identity,
            store,
            runtime,
            endpoint,
            state,
            commands,
            live_events,
            voice_packets,
            incoming_voice,
            metadata,
            screen_packets,
            incoming_screen,
            discovery,
            port_mapping,
        })
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn snapshot(&self) -> NetworkSnapshot {
        self.state.snapshot()
    }

    pub fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.state.set_waker(waker);
    }

    pub fn connect(&self, address: SocketAddr) -> anyhow::Result<()> {
        self.commands
            .send(NetworkCommand::Connect(address))
            .context("network runtime stopped")
    }

    pub fn groups(&self) -> anyhow::Result<Vec<Group>> {
        self.store.lock().groups()
    }

    pub fn channels(&self, group_id: GroupId) -> anyhow::Result<Vec<Channel>> {
        self.store.lock().channels(group_id)
    }

    pub fn create_channel(&self, group_id: GroupId, name: &str) -> anyhow::Result<Channel> {
        let channel = self.store.lock().create_channel(group_id, name)?;
        let _ = self.metadata.send(channel.clone());
        self.state.bump();
        Ok(channel)
    }

    pub fn timeline(
        &self,
        channel_id: ChannelId,
        limit: usize,
    ) -> anyhow::Result<Vec<TimelineEntry>> {
        self.store.lock().timeline(channel_id, limit)
    }

    pub fn create_group(&self, name: &str) -> anyhow::Result<(Group, Channel)> {
        let value = self.store.lock().create_group(name)?;
        self.state.bump();
        Ok(value)
    }

    pub fn invite(&self, group_id: GroupId) -> anyhow::Result<String> {
        let endpoints = self
            .state
            .snapshot()
            .advertised_addresses
            .into_iter()
            .map(|value| value.to_string())
            .collect();
        self.store
            .lock()
            .build_invite(&self.identity, group_id, endpoints)
    }

    pub fn import_invite(&self, value: &str) -> anyhow::Result<GroupInvite> {
        let invite = self.store.lock().import_invite(value)?;
        for endpoint in &invite.body.endpoints {
            if let Ok(address) = endpoint.parse() {
                let _ = self.connect(address);
            }
        }
        self.state.bump();
        Ok(invite)
    }

    pub fn send(
        &self,
        channel_id: ChannelId,
        payload: MessagePayload,
    ) -> anyhow::Result<EventEnvelope> {
        let event = self
            .store
            .lock()
            .append(&self.identity, channel_id, &payload)?;
        let _ = self.live_events.send(LiveEvent {
            event: Arc::new(event.clone()),
            author_name: self.identity.display_name().to_owned(),
        });
        self.state.bump();
        Ok(event)
    }

    pub fn broadcast_voice(&self, group_id: GroupId, packet: Vec<u8>) {
        let _ = self.voice_packets.send(VoicePacket {
            group_id,
            bytes: Arc::new(packet),
        });
    }

    pub fn subscribe_voice(&self) -> broadcast::Receiver<IncomingVoice> {
        self.incoming_voice.subscribe()
    }

    pub(crate) fn audio_channels(
        &self,
    ) -> (
        broadcast::Sender<VoicePacket>,
        broadcast::Receiver<IncomingVoice>,
    ) {
        (self.voice_packets.clone(), self.incoming_voice.subscribe())
    }

    pub(crate) fn screen_sender(&self) -> broadcast::Sender<ScreenPacket> {
        self.screen_packets.clone()
    }

    pub fn subscribe_screen(&self) -> broadcast::Receiver<IncomingScreen> {
        self.incoming_screen.subscribe()
    }

    pub fn block_peer(&self, peer: crate::model::PeerId) -> anyhow::Result<()> {
        self.store
            .lock()
            .block_peer(peer, "Blocked from the desktop UI")?;
        self.state
            .note(format!("Blocked peer {} locally", peer.short()));
        Ok(())
    }

    pub fn unblock_peer(&self, peer: crate::model::PeerId) -> anyhow::Result<()> {
        self.store.lock().unblock_peer(peer)?;
        self.state.note(format!("Unblocked peer {}", peer.short()));
        Ok(())
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(discovery) = self.discovery.take() {
            let _ = discovery.shutdown();
        }
        self.endpoint.close(0_u8.into(), b"Opencord shutting down");
        self.runtime.block_on(self.endpoint.wait_idle());
        self.port_mapping.lock().take();
    }
}

struct PortMapping {
    gateway: igd_next::Gateway,
    external_port: u16,
}

impl Drop for PortMapping {
    fn drop(&mut self) {
        let _ = self
            .gateway
            .remove_port(igd_next::PortMappingProtocol::UDP, self.external_port);
    }
}

fn start_port_mapping(
    port: u16,
    state: Arc<SharedNetworkState>,
    mapping: Arc<Mutex<Option<PortMapping>>>,
) {
    let _ = std::thread::Builder::new()
        .name("opencord-upnp".into())
        .spawn(move || {
            let result = (|| -> anyhow::Result<(igd_next::Gateway, SocketAddr)> {
                let gateway = igd_next::search_gateway(Default::default())
                    .context("no UPnP gateway found")?;
                let route = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
                route.connect(gateway.addr)?;
                let local = SocketAddr::new(route.local_addr()?.ip(), port);
                let external = gateway
                    .get_any_address(
                        igd_next::PortMappingProtocol::UDP,
                        local,
                        0,
                        "Opencord peer transport",
                    )
                    .context("router rejected UDP mapping")?;
                Ok((gateway, external))
            })();
            match result {
                Ok((gateway, external)) => {
                    state.add_advertised(external);
                    state.note(format!("Router mapped public peer address {external}"));
                    *mapping.lock() = Some(PortMapping {
                        gateway,
                        external_port: external.port(),
                    });
                }
                Err(error) => state.note(format!(
                    "Direct internet mapping unavailable ({error}); LAN and manual addresses still work"
                )),
            }
        });
}

fn make_server_config() -> anyhow::Result<ServerConfig> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["opencord.local".into()])?;
    let cert_der = CertificateDer::from(cert);
    let key = PrivatePkcs8KeyDer::from(signing_key.serialize_der());
    ServerConfig::with_single_cert(vec![cert_der], key.into()).context("configure QUIC server")
}

fn make_client_config() -> anyhow::Result<ClientConfig> {
    let rustls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();
    Ok(ClientConfig::new(Arc::new(QuicClientConfig::try_from(
        rustls_config,
    )?)))
}

#[derive(Debug)]
struct SkipServerVerification(Arc<CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Stable identity and authorization are verified by the signed app handshake.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn local_addresses(port: u16) -> Vec<SocketAddr> {
    let mut addresses = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| SocketAddr::new(interface.ip(), port))
        .filter(|address| !address.ip().is_unspecified() && !address.ip().is_loopback())
        .collect::<Vec<_>>();
    addresses.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    addresses.sort();
    addresses.dedup();
    addresses
}

fn start_discovery(
    peer: crate::model::PeerId,
    port: u16,
    commands: mpsc::UnboundedSender<NetworkCommand>,
    state: Arc<SharedNetworkState>,
) -> anyhow::Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new().context("start mDNS daemon")?;
    let instance = format!("opencord-{}", peer.short());
    let hostname = format!("{instance}.local.");
    let peer_string = peer.to_string();
    let properties = [("peer", peer_string.as_str()), ("version", "1")];
    let info = ServiceInfo::new(
        MDNS_SERVICE,
        &instance,
        &hostname,
        "",
        port,
        &properties[..],
    )
    .context("create mDNS service")?
    .enable_addr_auto();
    daemon.register(info).context("register mDNS service")?;
    let receiver = daemon.browse(MDNS_SERVICE).context("browse mDNS service")?;
    std::thread::Builder::new()
        .name("opencord-mdns".into())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                if let ServiceEvent::ServiceResolved(info) = event {
                    if info.get_property_val_str("peer") == Some(peer_string.as_str()) {
                        continue;
                    }
                    for ip in info.addresses {
                        let address = SocketAddr::new(ip.to_ip_addr(), info.port);
                        if address.ip().is_loopback() {
                            continue;
                        }
                        state.note(format!("Discovered LAN peer at {address}"));
                        let _ = commands.send(NetworkCommand::Connect(address));
                    }
                }
            }
        })
        .context("start mDNS listener")?;
    Ok(daemon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn test_options(path: PathBuf, name: &str) -> NodeOptions {
        let mut options = NodeOptions::new(path);
        options.display_name = Some(name.to_owned());
        options.listen = "127.0.0.1:0".parse().unwrap();
        options.enable_discovery = false;
        options
    }

    fn wait_for(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("condition was not reached before timeout");
    }

    #[test]
    fn two_nodes_rebuild_history_then_exchange_live_messages_and_voice() {
        let root = tempfile::tempdir().unwrap();
        let alice = Node::start(test_options(root.path().join("alice"), "Alice")).unwrap();
        let (group, channel) = alice.create_group("Friends").unwrap();
        alice
            .send(
                channel.id,
                MessagePayload::Text {
                    body: "history before Bob joined".into(),
                },
            )
            .unwrap();
        let invite = alice.invite(group.id).unwrap();

        let bob = Node::start(test_options(root.path().join("bob"), "Bob")).unwrap();
        bob.import_invite(&invite).unwrap();
        bob.connect(alice.snapshot().listen_address.unwrap())
            .unwrap();
        wait_for(|| {
            bob.timeline(channel.id, 50)
                .map(|items| items.len() == 1)
                .unwrap_or(false)
        });
        assert!(!alice.snapshot().online_peers.is_empty());

        bob.send(
            channel.id,
            MessagePayload::Text {
                body: "live reply".into(),
            },
        )
        .unwrap();
        wait_for(|| {
            alice
                .timeline(channel.id, 50)
                .map(|items| items.len() == 2)
                .unwrap_or(false)
        });
        let timeline = alice.timeline(channel.id, 50).unwrap();
        assert!(
            matches!(&timeline[1].payload, MessagePayload::Text { body } if body == "live reply")
        );

        let architecture = alice.create_channel(group.id, "architecture").unwrap();
        wait_for(|| {
            bob.channels(group.id)
                .map(|channels| channels.iter().any(|channel| channel.id == architecture.id))
                .unwrap_or(false)
        });

        let mut voice = bob.subscribe_voice();
        wait_for(|| {
            alice.broadcast_voice(group.id, b"encoded opus frame".to_vec());
            match voice.try_recv() {
                Ok(packet) => {
                    packet.group_id == group.id && packet.bytes.as_slice() == b"encoded opus frame"
                }
                Err(_) => false,
            }
        });

        let mut screen = bob.subscribe_screen();
        let screen_sender = alice.screen_sender();
        wait_for(|| {
            let _ = screen_sender.send(ScreenPacket {
                group_id: group.id,
                jpeg: Arc::new(b"jpeg frame bytes".to_vec()),
            });
            match screen.try_recv() {
                Ok(packet) => {
                    packet.group_id == group.id && packet.jpeg.as_slice() == b"jpeg frame bytes"
                }
                Err(_) => false,
            }
        });
    }
}
