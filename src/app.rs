use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Layout, Margin, RichText,
    ScrollArea, Sense, Stroke, Vec2,
};
use opencord::{
    AudioEngine, Channel, ChannelId, Group, GroupId, IncomingScreen, MessagePayload,
    NetworkSnapshot, Node, ScreenShare, TimelineEntry,
};

const RAIL: Color32 = Color32::from_rgb(17, 19, 24);
const SIDEBAR: Color32 = Color32::from_rgb(25, 28, 35);
const CANVAS: Color32 = Color32::from_rgb(31, 34, 42);
const SURFACE: Color32 = Color32::from_rgb(40, 44, 54);
const SURFACE_HOVER: Color32 = Color32::from_rgb(48, 53, 65);
const TEXT: Color32 = Color32::from_rgb(238, 240, 246);
const MUTED: Color32 = Color32::from_rgb(155, 162, 178);
const ACCENT: Color32 = Color32::from_rgb(109, 94, 252);
const GREEN: Color32 = Color32::from_rgb(72, 200, 142);
const RED: Color32 = Color32::from_rgb(239, 93, 112);

#[derive(Clone)]
enum Modal {
    CreateGroup { name: String },
    JoinGroup { invite: String },
    Invite { value: String },
    Connect { address: String },
    CreateChannel { name: String },
    About,
}

pub struct OpencordApp {
    node: Node,
    audio: AudioEngine,
    screen_share: ScreenShare,
    screen_receiver: tokio::sync::broadcast::Receiver<IncomingScreen>,
    screen_texture: Option<egui::TextureHandle>,
    screen_owner: Option<String>,
    groups: Vec<Group>,
    channels: Vec<Channel>,
    timeline: Vec<TimelineEntry>,
    network: NetworkSnapshot,
    selected_group: Option<GroupId>,
    selected_channel: Option<ChannelId>,
    composer: String,
    modal: Option<Modal>,
    toast: Option<(String, Instant, bool)>,
    last_generation: u64,
}

impl OpencordApp {
    pub fn new(creation: &eframe::CreationContext<'_>, node: Node) -> Self {
        configure_style(&creation.egui_ctx);
        let wake_context = creation.egui_ctx.clone();
        node.set_waker(Arc::new(move || wake_context.request_repaint()));
        let network = node.snapshot();
        let screen_receiver = node.subscribe_screen();
        let mut app = Self {
            node,
            audio: AudioEngine::default(),
            screen_share: ScreenShare::default(),
            screen_receiver,
            screen_texture: None,
            screen_owner: None,
            groups: Vec::new(),
            channels: Vec::new(),
            timeline: Vec::new(),
            network,
            selected_group: None,
            selected_channel: None,
            composer: String::new(),
            modal: None,
            toast: None,
            last_generation: 0,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.network = self.node.snapshot();
        self.last_generation = self.network.generation;
        match self.node.groups() {
            Ok(groups) => self.groups = groups,
            Err(error) => self.error(error),
        }
        if self.selected_group.is_none()
            || !self
                .groups
                .iter()
                .any(|group| Some(group.id) == self.selected_group)
        {
            self.selected_group = self.groups.first().map(|group| group.id);
        }
        self.channels = self
            .selected_group
            .and_then(|group| self.node.channels(group).ok())
            .unwrap_or_default();
        if self.selected_channel.is_none()
            || !self
                .channels
                .iter()
                .any(|channel| Some(channel.id) == self.selected_channel)
        {
            self.selected_channel = self.channels.first().map(|channel| channel.id);
        }
        self.timeline = self
            .selected_channel
            .and_then(|channel| self.node.timeline(channel, 500).ok())
            .unwrap_or_default();
    }

    fn select_group(&mut self, group_id: GroupId) {
        if self.selected_group != Some(group_id) {
            self.selected_group = Some(group_id);
            self.selected_channel = None;
            self.refresh();
        }
    }

    fn select_channel(&mut self, channel_id: ChannelId) {
        if self.selected_channel != Some(channel_id) {
            self.selected_channel = Some(channel_id);
            self.refresh();
        }
    }

    fn send_composer(&mut self) {
        let body = self.composer.trim().to_owned();
        let Some(channel) = self.selected_channel else {
            return;
        };
        if body.is_empty() {
            return;
        }
        match self.node.send(channel, MessagePayload::Text { body }) {
            Ok(_) => {
                self.composer.clear();
                self.refresh();
            }
            Err(error) => self.error(error),
        }
    }

    fn send_attachment(&mut self, path: PathBuf) {
        let Some(channel) = self.selected_channel else {
            return;
        };
        let result = (|| -> anyhow::Result<()> {
            let bytes = std::fs::read(&path)?;
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("attachment")
                .to_owned();
            let mime = mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string();
            self.node.send(
                channel,
                MessagePayload::Attachment {
                    file_name,
                    mime,
                    bytes,
                    caption: String::new(),
                },
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.notice("Encrypted attachment added");
                self.refresh();
            }
            Err(error) => self.error(error),
        }
    }

    fn notice(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now(), false));
    }

    fn error(&mut self, error: impl std::fmt::Display) {
        self.toast = Some((error.to_string(), Instant::now(), true));
    }

    fn current_group(&self) -> Option<&Group> {
        self.selected_group
            .and_then(|id| self.groups.iter().find(|group| group.id == id))
    }

    fn current_channel(&self) -> Option<&Channel> {
        self.selected_channel
            .and_then(|id| self.channels.iter().find(|channel| channel.id == id))
    }

    fn render_server_rail(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("server_rail")
            .exact_size(72.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(RAIL)
                    .inner_margin(Margin::symmetric(10, 12)),
            )
            .show(root, |ui| {
                ui.vertical_centered(|ui| {
                    brand_mark(ui);
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(10.0);
                    let groups = self.groups.clone();
                    for group in groups {
                        let selected = self.selected_group == Some(group.id);
                        if server_button(ui, &group, selected).clicked() {
                            self.select_group(group.id);
                        }
                        ui.add_space(7.0);
                    }
                    if round_icon_button(ui, "+", GREEN, "Create a group").clicked() {
                        self.modal = Some(Modal::CreateGroup {
                            name: String::new(),
                        });
                    }
                    ui.add_space(7.0);
                    if round_icon_button(ui, "↳", ACCENT, "Join with an encrypted invite").clicked()
                    {
                        self.modal = Some(Modal::JoinGroup {
                            invite: String::new(),
                        });
                    }
                });
            });
    }

    fn render_channels(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("channel_sidebar")
            .exact_size(242.0)
            .resizable(false)
            .frame(Frame::new().fill(SIDEBAR))
            .show(root, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new(
                            self.current_group()
                                .map(|g| g.name.as_str())
                                .unwrap_or("No group"),
                        )
                        .size(16.0)
                        .strong()
                        .color(TEXT),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("•••")
                            .on_hover_text("Group actions")
                            .clicked()
                        {
                            self.modal = Some(Modal::About);
                        }
                    });
                });
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("TEXT CHANNELS")
                            .size(11.0)
                            .strong()
                            .color(MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("+")
                            .on_hover_text("Create channel")
                            .clicked()
                        {
                            self.modal = Some(Modal::CreateChannel {
                                name: String::new(),
                            });
                        }
                    });
                });
                ui.add_space(4.0);
                let channels = self.channels.clone();
                for channel in channels {
                    let selected = self.selected_channel == Some(channel.id);
                    let response = ui.add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new(
                            RichText::new(format!("#  {}", channel.name)).color(if selected {
                                TEXT
                            } else {
                                MUTED
                            }),
                        )
                        .selected(selected)
                        .fill(if selected {
                            ACCENT
                        } else {
                            Color32::TRANSPARENT
                        })
                        .corner_radius(6),
                    );
                    if response.clicked() {
                        self.select_channel(channel.id);
                    }
                }

                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), 58.0),
                        Layout::top_down(Align::Min),
                        |ui| user_panel(ui, &self.node, &mut self.modal),
                    );
                    let audio = self.audio.snapshot();
                    if let Some(group_id) = audio.group_id {
                        Frame::new()
                            .fill(Color32::from_rgb(28, 42, 39))
                            .inner_margin(12)
                            .show(ui, |ui| {
                                ui.label(RichText::new("● Voice connected").color(GREEN).strong());
                                ui.label(
                                    RichText::new("Direct encrypted mesh")
                                        .size(11.0)
                                        .color(MUTED),
                                );
                                ui.horizontal(|ui| {
                                    let muted = audio.muted;
                                    if ui.button(if muted { "Unmute" } else { "Mute" }).clicked() {
                                        self.audio.set_muted(!muted);
                                    }
                                    if ui.button("Leave").clicked() {
                                        self.audio.leave();
                                    }
                                });
                                let _ = group_id;
                            });
                    } else if let Some(group_id) = self.selected_group {
                        Frame::new().fill(SURFACE).inner_margin(12).show(ui, |ui| {
                            ui.label(RichText::new("Voice lounge").strong().color(TEXT));
                            ui.label(
                                RichText::new("Peer-to-peer group call")
                                    .size(11.0)
                                    .color(MUTED),
                            );
                            if ui.button("Join voice").clicked()
                                && let Err(error) = self.audio.join(&self.node, group_id)
                            {
                                self.error(error);
                            }
                        });
                    }
                });
            });
    }

    fn render_members(&mut self, root: &mut egui::Ui) {
        egui::Panel::right("members")
            .exact_size(220.0)
            .resizable(false)
            .frame(Frame::new().fill(SIDEBAR).inner_margin(14))
            .show(root, |ui| {
                ui.label(
                    RichText::new(format!("ONLINE — {}", self.network.online_peers.len() + 1))
                        .size(11.0)
                        .strong()
                        .color(MUTED),
                );
                ui.add_space(10.0);
                member_row(
                    ui,
                    self.node.identity().display_name(),
                    true,
                    self.audio.snapshot().group_id.is_some(),
                );
                let peers = self.network.online_peers.clone();
                for peer in peers {
                    if self
                        .selected_group
                        .is_none_or(|group| peer.shared_groups.contains(&group))
                    {
                        ui.horizontal(|ui| {
                            member_row(ui, &peer.name, true, false);
                            if ui
                                .small_button("Block")
                                .on_hover_text("Reject this identity locally")
                                .clicked()
                            {
                                match self.node.block_peer(peer.id) {
                                    Ok(()) => self.notice(format!("Blocked {}", peer.name)),
                                    Err(error) => self.error(error),
                                }
                            }
                        });
                    }
                }
                ui.add_space(20.0);
                ui.label(RichText::new("NETWORK").size(11.0).strong().color(MUTED));
                ui.add_space(8.0);
                ui.label(RichText::new("No server").color(GREEN).strong());
                ui.label(
                    RichText::new("History is replicated only between authenticated peers.")
                        .size(12.0)
                        .color(MUTED),
                );
                if let Some(address) = self.network.listen_address {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("UDP {address}"))
                            .monospace()
                            .size(10.0)
                            .color(MUTED),
                    );
                }
            });
    }

    fn render_header(&mut self, root: &mut egui::Ui) {
        egui::Panel::top("chat_header")
            .exact_size(58.0)
            .frame(
                Frame::new()
                    .fill(CANVAS)
                    .inner_margin(Margin::symmetric(18, 10))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(48, 52, 62))),
            )
            .show(root, |ui| {
                let show_status = ui.available_width() >= 560.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("#").size(22.0).color(MUTED));
                    ui.label(
                        RichText::new(
                            self.current_channel()
                                .map(|c| c.name.as_str())
                                .unwrap_or("welcome"),
                        )
                        .size(16.0)
                        .strong()
                        .color(TEXT),
                    );
                    if show_status {
                        ui.separator();
                        ui.label(
                            RichText::new("End-to-end encrypted • local history")
                                .size(12.0)
                                .color(MUTED),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let sharing = self.screen_share.snapshot().group_id.is_some();
                        if ui
                            .button(if sharing {
                                "Stop sharing"
                            } else {
                                "Share screen"
                            })
                            .clicked()
                        {
                            if sharing {
                                self.screen_share.stop();
                            } else if let Some(group) = self.selected_group
                                && let Err(error) = self.screen_share.start(&self.node, group)
                            {
                                self.error(error);
                            }
                        }
                        if ui.button("Invite").clicked()
                            && let Some(group) = self.selected_group
                        {
                            match self.node.invite(group) {
                                Ok(value) => self.modal = Some(Modal::Invite { value }),
                                Err(error) => self.error(error),
                            }
                        }
                        if ui.button("Connect").clicked() {
                            self.modal = Some(Modal::Connect {
                                address: String::new(),
                            });
                        }
                    });
                });
            });
    }

    fn render_composer(&mut self, root: &mut egui::Ui) {
        egui::Panel::bottom("composer")
            .frame(Frame::new().fill(CANVAS).inner_margin(Margin::symmetric(18, 14)))
            .show(root, |ui| {
                if self.selected_channel.is_none() { return; }
                Frame::new().fill(SURFACE).corner_radius(10).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("+").size(20.0)).frame(false)).on_hover_text("Attach a file (8 MiB max)").clicked()
                            && let Some(path) = rfd::FileDialog::new().pick_file()
                        {
                            self.send_attachment(path);
                        }
                        let channel_name = self.current_channel().map(|channel| channel.name.clone()).unwrap_or_default();
                        let edit = ui.add_sized(
                            [ui.available_width() - 74.0, 38.0],
                            egui::TextEdit::multiline(&mut self.composer)
                                .desired_rows(1)
                                .hint_text(format!("Message #{channel_name}"))
                                .text_color(TEXT)
                                .frame(Frame::NONE),
                        );
                        let enter = edit.has_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter) && !input.modifiers.shift);
                        if enter { self.send_composer(); }
                        if ui.button("Send").clicked() { self.send_composer(); }
                    });
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Enter to send • Shift+Enter for a new line • files are encrypted end to end").size(10.0).color(MUTED));
            });
    }

    fn render_timeline(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(CANVAS)
                    .inner_margin(Margin::symmetric(20, 12)),
            )
            .show(root, |ui| {
                if self.groups.is_empty() {
                    empty_state(ui, &mut self.modal);
                    return;
                }
                let compact = ui.available_height() < 520.0;
                ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.screen_texture.is_some() {
                            Frame::new()
                                .fill(RAIL)
                                .corner_radius(10)
                                .inner_margin(10)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "LIVE • {} is sharing",
                                                self.screen_owner.as_deref().unwrap_or("A peer")
                                            ))
                                            .color(RED)
                                            .strong(),
                                        );
                                        if ui.button("Close view").clicked() {
                                            self.screen_texture = None;
                                            self.screen_owner = None;
                                        }
                                    });
                                    if let Some(texture) = &self.screen_texture {
                                        let available = ui.available_width();
                                        let size = texture.size_vec2();
                                        let shown =
                                            Vec2::new(available, available * size.y / size.x);
                                        ui.image((texture.id(), shown));
                                    }
                                });
                            ui.add_space(12.0);
                        }
                        ui.add_space(if compact { 4.0 } else { 16.0 });
                        channel_intro(ui, self.current_channel(), compact);
                        ui.add_space(if compact { 12.0 } else { 22.0 });
                        let mut save_request = None;
                        for entry in &self.timeline {
                            if let Some(request) = message_row(ui, entry) {
                                save_request = Some(request);
                            }
                            ui.add_space(4.0);
                        }
                        if let Some((name, bytes)) = save_request
                            && let Some(path) =
                                rfd::FileDialog::new().set_file_name(&name).save_file()
                        {
                            match std::fs::write(path, bytes) {
                                Ok(()) => self.notice("Attachment saved"),
                                Err(error) => self.error(error),
                            }
                        }
                        ui.add_space(12.0);
                    });
            });
    }

    fn render_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        let mut keep = true;
        let title = match &modal {
            Modal::CreateGroup { .. } => "Create a group",
            Modal::JoinGroup { .. } => "Join an encrypted group",
            Modal::Invite { .. } => "Invite peers",
            Modal::Connect { .. } => "Connect directly",
            Modal::CreateChannel { .. } => "Create a text channel",
            Modal::About => "About this group",
        };
        egui::Window::new(title)
            .id(Id::new("opencord_modal"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .frame(Frame::window(&ctx.global_style()).fill(SIDEBAR).corner_radius(12).inner_margin(20))
            .show(ctx, |ui| {
                ui.set_min_width(430.0);
                match &mut modal {
                    Modal::CreateGroup { name } => {
                        ui.label(RichText::new("A group is an encrypted peer mesh. No account or server is created.").color(MUTED));
                        ui.add_space(12.0);
                        ui.text_edit_singleline(name).request_focus();
                        ui.add_space(12.0);
                        if primary_button(ui, "Create group").clicked() {
                            match self.node.create_group(name) {
                                Ok(_) => { self.notice("Encrypted group created"); self.refresh(); keep = false; }
                                Err(error) => self.error(error),
                            }
                        }
                    }
                    Modal::JoinGroup { invite } => {
                        ui.label(RichText::new("Paste an opencord:// invite. It contains the signed group capability and peer addresses.").color(MUTED));
                        ui.add_space(10.0);
                        ui.add_sized([430.0, 120.0], egui::TextEdit::multiline(invite).hint_text("opencord://join/…"));
                        if primary_button(ui, "Verify and join").clicked() {
                            match self.node.import_invite(invite) {
                                Ok(_) => { self.notice("Invite verified — rebuilding from online peers"); self.refresh(); keep = false; }
                                Err(error) => self.error(error),
                            }
                        }
                    }
                    Modal::Invite { value } => {
                        ui.label(RichText::new("Anyone with this capability can decrypt existing history. Share it privately.").color(MUTED));
                        ui.add_space(10.0);
                        ui.add_sized([430.0, 140.0], egui::TextEdit::multiline(value).interactive(false));
                        if primary_button(ui, "Copy encrypted invite").clicked() {
                            ctx.copy_text(value.clone()); self.notice("Invite copied");
                        }
                    }
                    Modal::Connect { address } => {
                        ui.label(RichText::new("Enter a peer's reachable UDP address. Identity is verified after connection.").color(MUTED));
                        ui.add_space(10.0);
                        ui.text_edit_singleline(address);
                        if primary_button(ui, "Connect").clicked() {
                            match address.parse::<SocketAddr>().map_err(anyhow::Error::from).and_then(|addr| self.node.connect(addr)) {
                                Ok(()) => { self.notice("Connecting directly…"); keep = false; }
                                Err(error) => self.error(error),
                            }
                        }
                    }
                    Modal::CreateChannel { name } => {
                        ui.label(RichText::new("Channel metadata is encrypted-group authenticated and replicated to peers.").color(MUTED));
                        ui.add_space(10.0);
                        ui.text_edit_singleline(name);
                        if primary_button(ui, "Create channel").clicked() && let Some(group) = self.selected_group {
                            match self.node.create_channel(group, name) {
                                Ok(channel) => { self.selected_channel = Some(channel.id); self.refresh(); keep = false; }
                                Err(error) => self.error(error),
                            }
                        }
                    }
                    Modal::About => {
                        ui.label(RichText::new("Opencord protocol v1").size(18.0).strong().color(TEXT));
                        ui.add_space(8.0);
                        ui.label(RichText::new("Signed per-author logs • XChaCha20-Poly1305 • Ed25519 • QUIC • SQLite WAL • Opus voice").color(MUTED));
                        ui.add_space(8.0);
                        ui.label(RichText::new("No blockchain. No cloud history. No telemetry. No central service.").color(GREEN));
                    }
                }
                ui.add_space(10.0);
                if ui.button("Close").clicked() { keep = false; }
            });
        if keep {
            self.modal = Some(modal);
        }
    }

    fn render_toast(&mut self, ctx: &egui::Context) {
        let Some((message, created, error)) = &self.toast else {
            return;
        };
        if created.elapsed() > Duration::from_secs(4) {
            self.toast = None;
            return;
        }
        egui::Area::new(Id::new("toast"))
            .anchor(Align2::RIGHT_TOP, [-22.0, 74.0])
            .show(ctx, |ui| {
                Frame::new()
                    .fill(if *error {
                        Color32::from_rgb(83, 35, 44)
                    } else {
                        Color32::from_rgb(32, 70, 58)
                    })
                    .corner_radius(8)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.label(RichText::new(message).color(TEXT).strong());
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl eframe::App for OpencordApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        root.reset_style();
        let ctx = root.ctx().clone();
        let snapshot = self.node.snapshot();
        if snapshot.generation != self.last_generation {
            self.refresh();
        }
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| !path.as_os_str().is_empty())
                .collect::<Vec<_>>()
        });
        for path in dropped {
            self.send_attachment(path);
        }
        while let Ok(frame) = self.screen_receiver.try_recv() {
            if self.selected_group == Some(frame.group_id)
                && let Ok(image) = image::load_from_memory(&frame.jpeg)
            {
                let rgba = image.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                self.screen_texture =
                    Some(ctx.load_texture("peer-screen", color, egui::TextureOptions::LINEAR));
                self.screen_owner = self
                    .network
                    .online_peers
                    .iter()
                    .find(|peer| peer.id == frame.peer)
                    .map(|peer| peer.name.clone())
                    .or_else(|| Some(format!("Peer {}", frame.peer.short())));
            }
        }

        self.render_server_rail(root);
        self.render_channels(root);
        self.render_members(root);
        self.render_header(root);
        self.render_composer(root);
        self.render_timeline(root);
        self.render_modal(&ctx);
        self.render_toast(&ctx);
        if self.audio.snapshot().group_id.is_some()
            || self.screen_share.snapshot().group_id.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        CANVAS.to_normalized_gamma_f32()
    }
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = CANVAS;
    style.visuals.window_fill = SIDEBAR;
    style.visuals.extreme_bg_color = RAIL;
    style.visuals.faint_bg_color = SURFACE;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.widgets.noninteractive.bg_fill = CANVAS;
    style.visuals.widgets.noninteractive.weak_bg_fill = CANVAS;
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    style.visuals.widgets.inactive.bg_fill = SURFACE;
    style.visuals.widgets.inactive.weak_bg_fill = SURFACE;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::TRANSPARENT);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    style.visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
    style.visuals.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(76, 82, 98));
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    style.visuals.widgets.active.bg_fill = ACCENT;
    style.visuals.widgets.active.weak_bg_fill = ACCENT;
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    style.visuals.widgets.open.bg_fill = SURFACE_HOVER;
    style.visuals.widgets.open.weak_bg_fill = SURFACE_HOVER;
    style.visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, TEXT);
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    style.visuals.window_corner_radius = CornerRadius::same(12);
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(10.0, 7.0);
    ctx.set_global_style(style);
}

fn brand_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(50.0), Sense::hover());
    ui.painter().rect_filled(rect, 15.0, ACCENT);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "O",
        FontId::proportional(25.0),
        Color32::WHITE,
    );
}

fn server_button(ui: &mut egui::Ui, group: &Group, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(50.0), Sense::click());
    let rounding = if selected || response.hovered() {
        15.0
    } else {
        25.0
    };
    let color = if selected {
        ACCENT
    } else {
        group_color(group.id)
    };
    ui.painter().rect_filled(rect, rounding, color);
    let initials = group
        .name
        .split_whitespace()
        .take(2)
        .filter_map(|part| part.chars().next())
        .collect::<String>()
        .to_uppercase();
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initials,
        FontId::proportional(15.0),
        Color32::WHITE,
    );
    response.on_hover_text(&group.name)
}

fn round_icon_button(
    ui: &mut egui::Ui,
    icon: &str,
    color: Color32,
    tooltip: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(50.0), Sense::click());
    ui.painter().rect_filled(rect, 25.0, SURFACE);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(23.0),
        color,
    );
    response.on_hover_text(tooltip)
}

fn group_color(group: GroupId) -> Color32 {
    Color32::from_rgb(
        65 + group.0[0] % 80,
        70 + group.0[1] % 70,
        90 + group.0[2] % 90,
    )
}

fn user_panel(ui: &mut egui::Ui, node: &Node, modal: &mut Option<Modal>) {
    Frame::new().fill(RAIL).inner_margin(10).show(ui, |ui| {
        ui.horizontal(|ui| {
            avatar(ui, node.identity().display_name(), true, 34.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(node.identity().display_name())
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    RichText::new(format!("ID {}", node.identity().peer_id().short()))
                        .monospace()
                        .size(9.0)
                        .color(MUTED),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .small_button("ⓘ")
                    .on_hover_text("About Opencord")
                    .clicked()
                {
                    *modal = Some(Modal::About);
                }
            });
        });
    });
}

fn avatar(ui: &mut egui::Ui, name: &str, online: bool, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size / 2.0, Color32::from_rgb(75, 84, 108));
    let initial = name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(size * 0.45),
        TEXT,
    );
    if online {
        ui.painter()
            .circle_filled(rect.right_bottom() - Vec2::splat(5.0), 5.0, SIDEBAR);
        ui.painter()
            .circle_filled(rect.right_bottom() - Vec2::splat(5.0), 3.5, GREEN);
    }
}

fn member_row(ui: &mut egui::Ui, name: &str, online: bool, speaking: bool) {
    ui.horizontal(|ui| {
        avatar(ui, name, online, 34.0);
        ui.label(RichText::new(name).color(if online { TEXT } else { MUTED }));
        if speaking {
            ui.label(RichText::new("VOICE").size(9.0).color(GREEN));
        }
    });
    ui.add_space(5.0);
}

fn channel_intro(ui: &mut egui::Ui, channel: Option<&Channel>, compact: bool) {
    let name = channel
        .map(|channel| channel.name.as_str())
        .unwrap_or("welcome");
    if !compact {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 32.0, SURFACE);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "#",
            FontId::proportional(34.0),
            TEXT,
        );
        ui.add_space(8.0);
    }
    ui.label(
        RichText::new(format!("Welcome to #{name}!"))
            .size(if compact { 23.0 } else { 28.0 })
            .strong()
            .color(TEXT),
    );
    ui.label(
        RichText::new("This is the beginning of this encrypted channel's replicated history.")
            .color(MUTED),
    );
}

fn message_row(ui: &mut egui::Ui, entry: &TimelineEntry) -> Option<(String, Vec<u8>)> {
    let mut save = None;
    Frame::new()
        .inner_margin(Margin::symmetric(4, 7))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                avatar(ui, &entry.author_name, true, 40.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&entry.author_name).strong().color(TEXT));
                        ui.label(
                            RichText::new(relative_time(entry.event.header.sent_at_ms))
                                .size(10.0)
                                .color(MUTED),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{}:{}",
                                entry.event.header.author.short(),
                                entry.event.header.author_sequence
                            ))
                            .monospace()
                            .size(9.0)
                            .color(Color32::from_rgb(105, 112, 129)),
                        );
                    });
                    match &entry.payload {
                        MessagePayload::Text { body } | MessagePayload::System { body } => {
                            ui.label(RichText::new(body).size(15.0).color(TEXT));
                        }
                        MessagePayload::Attachment {
                            file_name,
                            mime,
                            bytes,
                            caption,
                        } => {
                            Frame::new()
                                .fill(SURFACE)
                                .corner_radius(8)
                                .inner_margin(12)
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("Encrypted attachment")
                                            .size(10.0)
                                            .color(ACCENT),
                                    );
                                    ui.label(RichText::new(file_name).strong().color(TEXT));
                                    ui.label(
                                        RichText::new(format!(
                                            "{} • {}",
                                            mime,
                                            human_bytes(bytes.len())
                                        ))
                                        .size(11.0)
                                        .color(MUTED),
                                    );
                                    if !caption.is_empty() {
                                        ui.label(RichText::new(caption).color(TEXT));
                                    }
                                    if ui.button("Save a local copy").clicked() {
                                        save = Some((file_name.clone(), bytes.clone()));
                                    }
                                });
                        }
                    }
                });
            });
        });
    save
}

fn relative_time(timestamp_ms: i64) -> String {
    let now = opencord::store::now_ms();
    let seconds = (now.saturating_sub(timestamp_ms) / 1_000).max(0);
    match seconds {
        0..=4 => "just now".into(),
        5..=59 => format!("{seconds}s ago"),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn human_bytes(bytes: usize) -> String {
    if bytes < 1_024 {
        format!("{bytes} B")
    } else if bytes < 1_048_576 {
        format!("{:.1} KiB", bytes as f64 / 1_024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    }
}

fn empty_state(ui: &mut egui::Ui, modal: &mut Option<Modal>) {
    ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
        ui.add_space(120.0);
        ui.label(
            RichText::new("Welcome to Opencord")
                .size(34.0)
                .strong()
                .color(TEXT),
        );
        ui.label(
            RichText::new(
                "Fast native chat. Your messages live with you and the peers you choose.",
            )
            .size(16.0)
            .color(MUTED),
        );
        ui.add_space(20.0);
        if primary_button(ui, "Create an encrypted group").clicked() {
            *modal = Some(Modal::CreateGroup {
                name: String::new(),
            });
        }
        if ui.button("Join with an invite").clicked() {
            *modal = Some(Modal::JoinGroup {
                invite: String::new(),
            });
        }
    });
}

fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).strong().color(Color32::WHITE))
            .fill(ACCENT)
            .corner_radius(7)
            .min_size(Vec2::new(140.0, 38.0)),
    )
}
