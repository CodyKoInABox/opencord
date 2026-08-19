use std::{
    collections::HashMap,
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

use crate::settings::{AccentChoice, AppSettings, MessageDensity, ThemeChoice};

const GREEN: Color32 = Color32::from_rgb(72, 200, 142);
const RED: Color32 = Color32::from_rgb(239, 93, 112);

#[derive(Clone, Copy)]
struct ThemePalette {
    dark: bool,
    rail: Color32,
    sidebar: Color32,
    canvas: Color32,
    surface: Color32,
    surface_hover: Color32,
    text: Color32,
    muted: Color32,
    border: Color32,
}

enum MessageAction {
    SaveAttachment(String, Vec<u8>),
    Reply(String),
}

fn theme_palette(theme: ThemeChoice) -> ThemePalette {
    match theme {
        ThemeChoice::Midnight => ThemePalette {
            dark: true,
            rail: Color32::from_rgb(11, 13, 18),
            sidebar: Color32::from_rgb(18, 21, 28),
            canvas: Color32::from_rgb(24, 27, 35),
            surface: Color32::from_rgb(33, 37, 47),
            surface_hover: Color32::from_rgb(43, 48, 60),
            text: Color32::from_rgb(241, 243, 248),
            muted: Color32::from_rgb(148, 156, 174),
            border: Color32::from_rgb(48, 53, 66),
        },
        ThemeChoice::Graphite => ThemePalette {
            dark: true,
            rail: Color32::from_rgb(18, 18, 20),
            sidebar: Color32::from_rgb(25, 25, 28),
            canvas: Color32::from_rgb(31, 31, 35),
            surface: Color32::from_rgb(42, 42, 47),
            surface_hover: Color32::from_rgb(53, 53, 59),
            text: Color32::from_rgb(244, 244, 246),
            muted: Color32::from_rgb(161, 161, 170),
            border: Color32::from_rgb(57, 57, 64),
        },
        ThemeChoice::Aurora => ThemePalette {
            dark: true,
            rail: Color32::from_rgb(8, 17, 23),
            sidebar: Color32::from_rgb(13, 27, 35),
            canvas: Color32::from_rgb(18, 34, 43),
            surface: Color32::from_rgb(27, 46, 56),
            surface_hover: Color32::from_rgb(37, 60, 71),
            text: Color32::from_rgb(237, 246, 247),
            muted: Color32::from_rgb(143, 166, 174),
            border: Color32::from_rgb(42, 66, 76),
        },
        ThemeChoice::Daylight => ThemePalette {
            dark: false,
            rail: Color32::from_rgb(232, 235, 242),
            sidebar: Color32::from_rgb(243, 245, 249),
            canvas: Color32::from_rgb(251, 252, 254),
            surface: Color32::from_rgb(235, 238, 245),
            surface_hover: Color32::from_rgb(224, 229, 238),
            text: Color32::from_rgb(31, 35, 45),
            muted: Color32::from_rgb(102, 111, 130),
            border: Color32::from_rgb(215, 220, 230),
        },
    }
}

fn accent_color(accent: AccentChoice) -> Color32 {
    match accent {
        AccentChoice::Violet => Color32::from_rgb(112, 92, 255),
        AccentChoice::Blue => Color32::from_rgb(66, 133, 244),
        AccentChoice::Mint => Color32::from_rgb(40, 184, 146),
        AccentChoice::Coral => Color32::from_rgb(229, 93, 117),
    }
}

#[derive(Clone)]
enum Modal {
    CreateGroup { name: String },
    JoinGroup { invite: String },
    Invite { value: String },
    Connect { address: String },
    CreateChannel { name: String },
    QuickSwitcher { query: String },
    Settings,
    About,
}

pub struct OpencordApp {
    node: Node,
    data_dir: PathBuf,
    settings: AppSettings,
    profile_name_draft: String,
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
    drafts: HashMap<ChannelId, String>,
    search_open: bool,
    search_query: String,
    modal: Option<Modal>,
    toast: Option<(String, Instant, bool)>,
    last_generation: u64,
}

impl OpencordApp {
    pub fn new(
        creation: &eframe::CreationContext<'_>,
        node: Node,
        data_dir: PathBuf,
        mut settings: AppSettings,
    ) -> Self {
        if settings.profile_name.is_empty() {
            settings.profile_name = node.identity().display_name();
            let _ = settings.save(&data_dir);
        }
        configure_style(&creation.egui_ctx, &settings);
        creation.egui_ctx.set_zoom_factor(settings.ui_scale);
        let wake_context = creation.egui_ctx.clone();
        node.set_waker(Arc::new(move || wake_context.request_repaint()));
        let network = node.snapshot();
        let screen_receiver = node.subscribe_screen();
        let mut app = Self {
            node,
            data_dir,
            profile_name_draft: settings.profile_name.clone(),
            settings,
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
            drafts: HashMap::new(),
            search_open: false,
            search_query: String::new(),
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
            self.save_current_draft();
            self.selected_group = Some(group_id);
            self.selected_channel = None;
            self.refresh();
            self.restore_current_draft();
        }
    }

    fn select_channel(&mut self, channel_id: ChannelId) {
        if self.selected_channel != Some(channel_id) {
            self.save_current_draft();
            self.selected_channel = Some(channel_id);
            self.refresh();
            self.restore_current_draft();
        }
    }

    fn save_current_draft(&mut self) {
        if let Some(channel) = self.selected_channel {
            if self.composer.trim().is_empty() {
                self.drafts.remove(&channel);
            } else {
                self.drafts
                    .insert(channel, std::mem::take(&mut self.composer));
            }
        }
    }

    fn restore_current_draft(&mut self) {
        self.composer = self
            .selected_channel
            .and_then(|channel| self.drafts.remove(&channel))
            .unwrap_or_default();
    }

    fn persist_settings(&mut self, ctx: &egui::Context) {
        self.settings.normalize();
        configure_style(ctx, &self.settings);
        ctx.set_zoom_factor(self.settings.ui_scale);
        match self.settings.save(&self.data_dir) {
            Ok(()) => self.notice("Settings saved"),
            Err(error) => self.error(error),
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
        let palette = theme_palette(self.settings.theme);
        let accent = accent_color(self.settings.accent);
        egui::Panel::left("server_rail")
            .exact_size(72.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(palette.rail)
                    .inner_margin(Margin::symmetric(10, 12)),
            )
            .show(root, |ui| {
                ui.vertical_centered(|ui| {
                    if brand_mark(ui).on_hover_text("About Opencord").clicked() {
                        self.modal = Some(Modal::About);
                    }
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
                    if round_icon_button(ui, ">", accent, "Join with an encrypted invite").clicked()
                    {
                        self.modal = Some(Modal::JoinGroup {
                            invite: String::new(),
                        });
                    }
                });
            });
    }

    fn render_channels(&mut self, root: &mut egui::Ui) {
        let palette = theme_palette(self.settings.theme);
        let accent = accent_color(self.settings.accent);
        egui::Panel::left("channel_sidebar")
            .exact_size(242.0)
            .resizable(false)
            .frame(Frame::new().fill(palette.sidebar))
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
                        .color(palette.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.menu_button("•••", |ui| {
                            if ui.button("Invite people").clicked() {
                                if let Some(group) = self.selected_group {
                                    match self.node.invite(group) {
                                        Ok(value) => self.modal = Some(Modal::Invite { value }),
                                        Err(error) => self.error(error),
                                    }
                                }
                                ui.close();
                            }
                            if ui.button("Create channel").clicked() {
                                self.modal = Some(Modal::CreateChannel {
                                    name: String::new(),
                                });
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Settings").clicked() {
                                self.modal = Some(Modal::Settings);
                                ui.close();
                            }
                        });
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
                            .color(palette.muted),
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
                    let has_draft = self.drafts.contains_key(&channel.id);
                    let response = ui.add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new(
                            RichText::new(format!(
                                "#  {}{}",
                                channel.name,
                                if has_draft { "  •" } else { "" }
                            ))
                            .color(if selected {
                                Color32::WHITE
                            } else {
                                palette.muted
                            }),
                        )
                        .selected(selected)
                        .fill(if selected {
                            accent
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
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), 118.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            if let Some(group_id) = audio.group_id {
                                Frame::new()
                                    .fill(Color32::from_rgb(28, 42, 39))
                                    .inner_margin(12)
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new("● Voice connected")
                                                .color(GREEN)
                                                .strong(),
                                        );
                                        ui.label(
                                            RichText::new("Direct encrypted mesh")
                                                .size(11.0)
                                                .color(palette.muted),
                                        );
                                        ui.horizontal(|ui| {
                                            let muted = audio.muted;
                                            if ui
                                                .button(if muted { "Unmute" } else { "Mute" })
                                                .clicked()
                                            {
                                                self.audio.set_muted(!muted);
                                            }
                                            if ui.button("Leave").clicked() {
                                                self.audio.leave();
                                            }
                                        });
                                        let _ = group_id;
                                    });
                            } else if let Some(group_id) = self.selected_group {
                                Frame::new().fill(palette.surface).inner_margin(12).show(
                                    ui,
                                    |ui| {
                                        ui.label(
                                            RichText::new("Voice lounge")
                                                .strong()
                                                .color(palette.text),
                                        );
                                        ui.label(
                                            RichText::new("Peer-to-peer group call")
                                                .size(11.0)
                                                .color(palette.muted),
                                        );
                                        if ui.button("Join voice").clicked()
                                            && let Err(error) =
                                                self.audio.join(&self.node, group_id)
                                        {
                                            self.error(error);
                                        }
                                    },
                                );
                            }
                        },
                    );
                });
            });
    }

    fn render_members(&mut self, root: &mut egui::Ui) {
        let palette = theme_palette(self.settings.theme);
        egui::Panel::right("members")
            .exact_size(220.0)
            .resizable(false)
            .frame(Frame::new().fill(palette.sidebar).inner_margin(14))
            .show(root, |ui| {
                ui.label(
                    RichText::new(format!("ONLINE — {}", self.network.online_peers.len() + 1))
                        .size(11.0)
                        .strong()
                        .color(palette.muted),
                );
                ui.add_space(10.0);
                member_row(
                    ui,
                    &self.node.identity().display_name(),
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
                ui.label(
                    RichText::new("CONNECTION")
                        .size(11.0)
                        .strong()
                        .color(palette.muted),
                );
                ui.add_space(8.0);
                ui.label(RichText::new("Peer-to-peer mesh").color(GREEN).strong());
                ui.label(
                    RichText::new("End-to-end encrypted")
                        .size(12.0)
                        .color(palette.muted),
                );
                if let Some(address) = self.network.listen_address {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("UDP {address}"))
                            .monospace()
                            .size(10.0)
                            .color(palette.muted),
                    );
                }
            });
    }

    fn render_header(&mut self, root: &mut egui::Ui) {
        let palette = theme_palette(self.settings.theme);
        egui::Panel::top("chat_header")
            .exact_size(58.0)
            .frame(
                Frame::new()
                    .fill(palette.canvas)
                    .inner_margin(Margin::symmetric(18, 10))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(48, 52, 62))),
            )
            .show(root, |ui| {
                let show_status = ui.available_width() >= 560.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("#").size(22.0).color(palette.muted));
                    ui.label(
                        RichText::new(
                            self.current_channel()
                                .map(|c| c.name.as_str())
                                .unwrap_or("welcome"),
                        )
                        .size(16.0)
                        .strong()
                        .color(palette.text),
                    );
                    if show_status {
                        ui.separator();
                        ui.label(
                            RichText::new("End-to-end encrypted • local history")
                                .size(12.0)
                                .color(palette.muted),
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
                        if self.search_open {
                            let search = ui.add_sized(
                                [180.0, 30.0],
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .hint_text("Search this channel")
                                    .id(Id::new("channel_search")),
                            );
                            if search.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Escape))
                            {
                                self.search_open = false;
                                self.search_query.clear();
                            }
                        } else if ui
                            .button("Search")
                            .on_hover_text("Search this channel (Ctrl+F)")
                            .clicked()
                        {
                            self.search_open = true;
                            ui.memory_mut(|memory| memory.request_focus(Id::new("channel_search")));
                        }
                    });
                });
            });
    }

    fn render_composer(&mut self, root: &mut egui::Ui) {
        let palette = theme_palette(self.settings.theme);
        egui::Panel::bottom("composer")
            .frame(
                Frame::new()
                    .fill(palette.canvas)
                    .inner_margin(Margin::symmetric(18, 14)),
            )
            .show(root, |ui| {
                if self.selected_channel.is_none() {
                    return;
                }
                Frame::new()
                    .fill(palette.surface)
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::Button::new(RichText::new("+").size(20.0)).frame(false))
                                .on_hover_text("Attach a file (8 MiB max)")
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new().pick_file()
                            {
                                self.send_attachment(path);
                            }
                            let channel_name = self
                                .current_channel()
                                .map(|channel| channel.name.clone())
                                .unwrap_or_default();
                            let edit = ui.add_sized(
                                [ui.available_width() - 74.0, 38.0],
                                egui::TextEdit::multiline(&mut self.composer)
                                    .desired_rows(1)
                                    .hint_text(format!("Message #{channel_name}"))
                                    .text_color(palette.text)
                                    .frame(Frame::NONE),
                            );
                            let enter = self.settings.enter_to_send
                                && edit.has_focus()
                                && ui.input(|input| {
                                    input.key_pressed(egui::Key::Enter) && !input.modifiers.shift
                                });
                            if enter {
                                self.send_composer();
                            }
                            if ui.button("Send").clicked() {
                                self.send_composer();
                            }
                        });
                    });
                ui.add_space(4.0);
                let send_hint = if self.settings.enter_to_send {
                    "Enter to send • Shift+Enter for a new line"
                } else {
                    "Use Send when your message is ready"
                };
                ui.label(
                    RichText::new(format!("{send_hint} • End-to-end encrypted"))
                        .size(10.0)
                        .color(palette.muted),
                );
            });
    }

    fn render_timeline(&mut self, root: &mut egui::Ui) {
        let palette = theme_palette(self.settings.theme);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(palette.canvas)
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
                                .fill(palette.rail)
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
                        if self.settings.show_channel_intro {
                            channel_intro(ui, self.current_channel(), compact);
                            ui.add_space(if compact { 12.0 } else { 22.0 });
                        }
                        let mut message_action = None;
                        for entry in &self.timeline {
                            if !self.search_query.is_empty()
                                && !message_matches(entry, &self.search_query)
                            {
                                continue;
                            }
                            if let Some(action) = message_row(
                                ui,
                                entry,
                                self.settings.show_message_ids,
                                self.settings.density,
                            ) {
                                message_action = Some(action);
                            }
                            ui.add_space(if self.settings.density == MessageDensity::Compact {
                                0.0
                            } else {
                                4.0
                            });
                        }
                        match message_action {
                            Some(MessageAction::SaveAttachment(name, bytes)) => {
                                if let Some(path) =
                                    rfd::FileDialog::new().set_file_name(&name).save_file()
                                {
                                    match std::fs::write(path, bytes) {
                                        Ok(()) => self.notice("Attachment saved"),
                                        Err(error) => self.error(error),
                                    }
                                }
                            }
                            Some(MessageAction::Reply(quote)) => {
                                if !self.composer.is_empty() {
                                    self.composer.push('\n');
                                }
                                self.composer.push_str(&quote);
                            }
                            None => {}
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
        let mut settings_changed = false;
        let mut save_profile = false;
        let palette = theme_palette(self.settings.theme);
        let title = match &modal {
            Modal::CreateGroup { .. } => "Create a group",
            Modal::JoinGroup { .. } => "Join an encrypted group",
            Modal::Invite { .. } => "Invite peers",
            Modal::Connect { .. } => "Connect directly",
            Modal::CreateChannel { .. } => "Create a text channel",
            Modal::QuickSwitcher { .. } => "Quick switcher",
            Modal::Settings => "Settings",
            Modal::About => "About Opencord",
        };
        egui::Window::new(title)
            .id(Id::new("opencord_modal"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(matches!(modal, Modal::Settings))
            .frame(
                Frame::window(&ctx.global_style())
                    .fill(palette.sidebar)
                    .corner_radius(14)
                    .inner_margin(24),
            )
            .show(ctx, |ui| {
                ui.set_min_width(if matches!(modal, Modal::Settings) { 560.0 } else { 430.0 });
                match &mut modal {
                    Modal::CreateGroup { name } => {
                        ui.label(RichText::new("Start a private peer-to-peer space.").color(palette.muted));
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
                        ui.label(RichText::new("Paste an opencord:// invite from someone you trust.").color(palette.muted));
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
                        ui.label(RichText::new("This invite grants access to encrypted group history. Share it privately.").color(palette.muted));
                        ui.add_space(10.0);
                        ui.add_sized([430.0, 140.0], egui::TextEdit::multiline(value).interactive(false));
                        if primary_button(ui, "Copy encrypted invite").clicked() {
                            ctx.copy_text(value.clone()); self.notice("Invite copied");
                        }
                    }
                    Modal::Connect { address } => {
                        ui.label(RichText::new("Enter a peer's reachable UDP address.").color(palette.muted));
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
                        ui.label(RichText::new("Channels sync with everyone in this group.").color(palette.muted));
                        ui.add_space(10.0);
                        ui.text_edit_singleline(name);
                        if primary_button(ui, "Create channel").clicked() && let Some(group) = self.selected_group {
                            match self.node.create_channel(group, name) {
                                Ok(channel) => { self.selected_channel = Some(channel.id); self.refresh(); keep = false; }
                                Err(error) => self.error(error),
                            }
                        }
                    }
                    Modal::QuickSwitcher { query } => {
                        let search = ui.add_sized(
                            [430.0, 40.0],
                            egui::TextEdit::singleline(query)
                                .hint_text("Jump to a channel…")
                                .id(Id::new("quick_switcher_search")),
                        );
                        search.request_focus();
                        let activate_first = ui.input(|input| input.key_pressed(egui::Key::Enter));
                        ui.add_space(8.0);
                        let needle = query.trim().to_lowercase();
                        let channels = self.channels.clone();
                        let mut selected = None;
                        ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                            for channel in channels
                                .iter()
                                .filter(|channel| channel.name.to_lowercase().contains(&needle))
                            {
                                if ui
                                    .add_sized(
                                        [ui.available_width(), 36.0],
                                        egui::Button::new(format!("#  {}", channel.name))
                                            .selected(self.selected_channel == Some(channel.id)),
                                    )
                                    .clicked()
                                {
                                    selected = Some(channel.id);
                                }
                            }
                        });
                        if activate_first && selected.is_none() {
                            selected = channels
                                .iter()
                                .find(|channel| channel.name.to_lowercase().contains(&needle))
                                .map(|channel| channel.id);
                        }
                        if let Some(channel) = selected {
                            self.select_channel(channel);
                            keep = false;
                        }
                    }
                    Modal::Settings => {
                        ScrollArea::vertical().max_height(590.0).show(ui, |ui| {
                            settings_heading(ui, "Profile", "How you appear to peers");
                            ui.horizontal(|ui| {
                                avatar(ui, &self.profile_name_draft, true, 42.0);
                                ui.add_sized(
                                    [280.0, 38.0],
                                    egui::TextEdit::singleline(&mut self.profile_name_draft)
                                        .hint_text("Display name"),
                                );
                                if ui.button("Save profile").clicked() {
                                    save_profile = true;
                                }
                            });

                            settings_divider(ui);
                            settings_heading(ui, "Appearance", "Theme, accent, and interface scale");
                            ui.label(RichText::new("THEME").size(10.0).strong().color(palette.muted));
                            ui.horizontal_wrapped(|ui| {
                                for theme in ThemeChoice::ALL {
                                    let swatch = theme_palette(theme);
                                    let selected = self.settings.theme == theme;
                                    let response = Frame::new()
                                        .fill(swatch.canvas)
                                        .stroke(Stroke::new(
                                            if selected { 2.0 } else { 1.0 },
                                            if selected { accent_color(self.settings.accent) } else { swatch.border },
                                        ))
                                        .corner_radius(9)
                                        .inner_margin(Margin::symmetric(14, 10))
                                        .show(ui, |ui| {
                                            ui.set_min_width(92.0);
                                            ui.label(RichText::new(theme.label()).color(swatch.text).strong());
                                        })
                                        .response
                                        .interact(Sense::click());
                                    if response.clicked() && self.settings.theme != theme {
                                        self.settings.theme = theme;
                                        settings_changed = true;
                                    }
                                }
                            });
                            ui.add_space(12.0);
                            ui.label(RichText::new("ACCENT").size(10.0).strong().color(palette.muted));
                            ui.horizontal(|ui| {
                                for accent in AccentChoice::ALL {
                                    let selected = self.settings.accent == accent;
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new(accent.label()).color(Color32::WHITE),
                                            )
                                            .fill(accent_color(accent))
                                            .stroke(Stroke::new(
                                                if selected { 2.0 } else { 0.0 },
                                                Color32::WHITE,
                                            )),
                                        )
                                        .clicked()
                                        && !selected
                                    {
                                        self.settings.accent = accent;
                                        settings_changed = true;
                                    }
                                }
                            });
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.label("Interface scale");
                                settings_changed |= ui
                                    .add(egui::Slider::new(&mut self.settings.ui_scale, 0.85..=1.20).step_by(0.05))
                                    .changed();
                            });
                            ui.horizontal(|ui| {
                                ui.label("Message spacing");
                                settings_changed |= ui
                                    .selectable_value(&mut self.settings.density, MessageDensity::Cozy, MessageDensity::Cozy.label())
                                    .changed();
                                settings_changed |= ui
                                    .selectable_value(&mut self.settings.density, MessageDensity::Compact, MessageDensity::Compact.label())
                                    .changed();
                            });

                            settings_divider(ui);
                            settings_heading(ui, "Chat", "Reading and composing messages");
                            settings_changed |= ui.checkbox(&mut self.settings.enter_to_send, "Enter sends a message").changed();
                            settings_changed |= ui.checkbox(&mut self.settings.show_channel_intro, "Show channel introductions").changed();
                            settings_changed |= ui.checkbox(&mut self.settings.show_message_ids, "Show message IDs").changed();
                            settings_changed |= ui.checkbox(&mut self.settings.show_member_list, "Show member list on wide windows").changed();
                            settings_divider(ui);
                            settings_heading(ui, "Connection", "Peer-to-peer transport status");
                            ui.label(format!("{} peer(s) online", self.network.online_peers.len()));
                            if let Some(address) = self.network.listen_address {
                                ui.label(RichText::new(format!("Listening on {address}")).monospace().color(palette.muted));
                            }
                            ui.label(RichText::new("QUIC transport • Ed25519 identities • End-to-end encrypted groups").color(palette.muted));
                        });
                    }
                    Modal::About => {
                        ui.label(
                            RichText::new("Opencord")
                                .size(26.0)
                                .strong()
                                .color(palette.text),
                        );
                        ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(palette.muted));
                        ui.add_space(14.0);
                        ui.label("A fast native peer-to-peer community app with end-to-end encrypted groups, local history, voice, and screen sharing.");
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new("Created by CodyKoInABox")
                                .strong()
                                .color(palette.text),
                        );
                        ui.hyperlink_to("github.com/CodyKoInABox", "https://github.com/CodyKoInABox");
                        ui.hyperlink_to("Opencord on GitHub", "https://github.com/CodyKoInABox/opencord");
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new("AGPL-3.0-or-later")
                                .strong()
                                .color(palette.text),
                        );
                        ui.label(RichText::new("Free software licensed under the GNU Affero General Public License.").color(palette.muted));
                        ui.hyperlink_to("Read the license", "https://www.gnu.org/licenses/agpl-3.0.html");
                        ui.add_space(14.0);
                        ui.label(RichText::new("XChaCha20-Poly1305 • Ed25519 • QUIC • SQLite WAL • Opus").size(11.0).color(palette.muted));
                    }
                }
                ui.add_space(10.0);
                if ui.button("Close").clicked() { keep = false; }
            });
        if save_profile {
            match self.node.rename_profile(&self.profile_name_draft) {
                Ok(name) => {
                    self.profile_name_draft = name.clone();
                    self.settings.profile_name = name;
                    settings_changed = true;
                }
                Err(error) => self.error(error),
            }
        }
        if settings_changed {
            self.persist_settings(ctx);
        }
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
                let text = ui.visuals().text_color();
                Frame::new()
                    .fill(if *error {
                        Color32::from_rgb(83, 35, 44)
                    } else {
                        Color32::from_rgb(32, 70, 58)
                    })
                    .corner_radius(8)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.label(RichText::new(message).color(text).strong());
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl eframe::App for OpencordApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        root.reset_style();
        let ctx = root.ctx().clone();
        let (
            open_settings,
            open_search,
            open_switcher,
            close_overlay,
            previous_channel,
            next_channel,
        ) = ctx.input(|input| {
            (
                input.modifiers.command && input.key_pressed(egui::Key::Comma),
                input.modifiers.command && input.key_pressed(egui::Key::F),
                input.modifiers.command && input.key_pressed(egui::Key::K),
                input.key_pressed(egui::Key::Escape),
                input.modifiers.alt && input.key_pressed(egui::Key::ArrowUp),
                input.modifiers.alt && input.key_pressed(egui::Key::ArrowDown),
            )
        });
        if open_settings {
            self.modal = Some(Modal::Settings);
        }
        if open_search {
            self.search_open = true;
            ctx.memory_mut(|memory| memory.request_focus(Id::new("channel_search")));
        }
        if open_switcher {
            self.modal = Some(Modal::QuickSwitcher {
                query: String::new(),
            });
            ctx.memory_mut(|memory| memory.request_focus(Id::new("quick_switcher_search")));
        }
        if close_overlay {
            if self.search_open {
                self.search_open = false;
                self.search_query.clear();
            } else {
                self.modal = None;
            }
        }
        if (previous_channel || next_channel)
            && let Some(index) = self.selected_channel.and_then(|selected| {
                self.channels
                    .iter()
                    .position(|channel| channel.id == selected)
            })
        {
            let target = if previous_channel {
                index
                    .checked_sub(1)
                    .unwrap_or(self.channels.len().saturating_sub(1))
            } else {
                (index + 1) % self.channels.len().max(1)
            };
            if let Some(channel) = self.channels.get(target) {
                self.select_channel(channel.id);
            }
        }
        let snapshot = self.node.snapshot();
        if snapshot.generation != self.last_generation {
            let previous = self.timeline.last().map(|entry| {
                (
                    entry.event.header.author,
                    entry.event.header.author_sequence,
                )
            });
            self.refresh();
            if let Some(entry) = self.timeline.last()
                && previous
                    != Some((
                        entry.event.header.author,
                        entry.event.header.author_sequence,
                    ))
                && entry.event.header.author != self.node.identity().peer_id()
            {
                self.notice(format!("New message from {}", entry.author_name));
            }
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
        if self.settings.show_member_list && root.available_width() >= 840.0 {
            self.render_members(root);
        }
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
        theme_palette(self.settings.theme)
            .canvas
            .to_normalized_gamma_f32()
    }
}

fn configure_style(ctx: &egui::Context, settings: &AppSettings) {
    let palette = theme_palette(settings.theme);
    let accent = accent_color(settings.accent);
    ctx.set_theme(if palette.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    let mut style = (*ctx.global_style()).clone();
    style.visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    style.visuals.panel_fill = palette.canvas;
    style.visuals.window_fill = palette.sidebar;
    style.visuals.extreme_bg_color = palette.rail;
    style.visuals.faint_bg_color = palette.surface;
    style.visuals.override_text_color = Some(palette.text);
    style.visuals.widgets.noninteractive.bg_fill = palette.canvas;
    style.visuals.widgets.noninteractive.weak_bg_fill = palette.canvas;
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.inactive.bg_fill = palette.surface;
    style.visuals.widgets.inactive.weak_bg_fill = palette.surface;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::TRANSPARENT);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.hovered.bg_fill = palette.surface_hover;
    style.visuals.widgets.hovered.weak_bg_fill = palette.surface_hover;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.border);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.active.bg_fill = accent;
    style.visuals.widgets.active.weak_bg_fill = accent;
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    style.visuals.widgets.open.bg_fill = palette.surface_hover;
    style.visuals.widgets.open.weak_bg_fill = palette.surface_hover;
    style.visuals.widgets.open.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.selection.bg_fill = accent;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    style.visuals.window_corner_radius = CornerRadius::same(12);
    style.spacing.item_spacing = match settings.density {
        MessageDensity::Compact => Vec2::new(7.0, 4.0),
        MessageDensity::Cozy => Vec2::new(8.0, 7.0),
    };
    style.spacing.button_padding = Vec2::new(10.0, 7.0);
    ctx.set_global_style(style);
}

fn brand_mark(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(50.0), Sense::click());
    let accent = ui.visuals().selection.bg_fill;
    ui.painter().rect_filled(
        rect,
        15.0,
        if response.hovered() {
            accent.gamma_multiply(1.15)
        } else {
            accent
        },
    );
    let bubble = rect.shrink2(Vec2::new(12.0, 14.0));
    ui.painter().rect_filled(bubble, 6.0, Color32::WHITE);
    let dot_y = bubble.center().y;
    ui.painter()
        .circle_filled(egui::pos2(bubble.center().x - 5.0, dot_y), 2.2, accent);
    ui.painter()
        .circle_filled(egui::pos2(bubble.center().x + 5.0, dot_y), 2.2, accent);
    response
}

fn server_button(ui: &mut egui::Ui, group: &Group, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(50.0), Sense::click());
    let rounding = if selected || response.hovered() {
        15.0
    } else {
        25.0
    };
    let color = if selected {
        ui.visuals().selection.bg_fill
    } else {
        group_color(group.id)
    };
    ui.painter().rect_filled(rect, rounding, color);
    if selected {
        let marker = egui::Rect::from_center_size(
            egui::pos2(rect.left() - 7.0, rect.center().y),
            Vec2::new(4.0, 26.0),
        );
        ui.painter().rect_filled(marker, 2.0, Color32::WHITE);
    }
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
    ui.painter().rect_filled(
        rect,
        if response.hovered() { 15.0 } else { 25.0 },
        if response.hovered() {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            ui.visuals().widgets.inactive.bg_fill
        },
    );
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
    let display_name = node.identity().display_name();
    Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                avatar(ui, &display_name, true, 34.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(&display_name)
                            .strong()
                            .color(ui.visuals().text_color()),
                    );
                    ui.label(
                        RichText::new(format!("ID {}", node.identity().peer_id().short()))
                            .monospace()
                            .size(9.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("⚙")
                        .on_hover_text("Settings (Ctrl+,)")
                        .clicked()
                    {
                        *modal = Some(Modal::Settings);
                    }
                    if ui
                        .small_button("i")
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
        ui.visuals().text_color(),
    );
    if online {
        ui.painter().circle_filled(
            rect.right_bottom() - Vec2::splat(5.0),
            5.0,
            ui.visuals().window_fill(),
        );
        ui.painter()
            .circle_filled(rect.right_bottom() - Vec2::splat(5.0), 3.5, GREEN);
    }
}

fn member_row(ui: &mut egui::Ui, name: &str, online: bool, speaking: bool) {
    ui.horizontal(|ui| {
        avatar(ui, name, online, 34.0);
        ui.label(RichText::new(name).color(if online {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        }));
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
        ui.painter()
            .circle_filled(rect.center(), 32.0, ui.visuals().widgets.inactive.bg_fill);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "#",
            FontId::proportional(34.0),
            ui.visuals().text_color(),
        );
        ui.add_space(8.0);
    }
    ui.label(
        RichText::new(format!("Welcome to #{name}!"))
            .size(if compact { 23.0 } else { 28.0 })
            .strong()
            .color(ui.visuals().text_color()),
    );
    ui.label(
        RichText::new("This is the beginning of this encrypted channel's replicated history.")
            .color(ui.visuals().weak_text_color()),
    );
}

fn message_row(
    ui: &mut egui::Ui,
    entry: &TimelineEntry,
    show_message_id: bool,
    density: MessageDensity,
) -> Option<MessageAction> {
    let mut action = None;
    let compact = density == MessageDensity::Compact;
    Frame::new()
        .inner_margin(Margin::symmetric(4, if compact { 3 } else { 7 }))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                avatar(
                    ui,
                    &entry.author_name,
                    true,
                    if compact { 32.0 } else { 40.0 },
                );
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&entry.author_name)
                                .strong()
                                .color(ui.visuals().text_color()),
                        );
                        ui.label(
                            RichText::new(relative_time(entry.event.header.sent_at_ms))
                                .size(10.0)
                                .color(ui.visuals().weak_text_color()),
                        );
                        if show_message_id {
                            ui.label(
                                RichText::new(format!(
                                    "{}:{}",
                                    entry.event.header.author.short(),
                                    entry.event.header.author_sequence
                                ))
                                .monospace()
                                .size(9.0)
                                .color(ui.visuals().weak_text_color()),
                            );
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.menu_button("•••", |ui| {
                                if ui.button("Copy text").clicked() {
                                    ui.ctx().copy_text(message_copy_text(entry));
                                    ui.close();
                                }
                                if ui.button("Reply").clicked() {
                                    action = Some(MessageAction::Reply(message_quote(entry)));
                                    ui.close();
                                }
                            });
                        });
                    });
                    match &entry.payload {
                        MessagePayload::Text { body } | MessagePayload::System { body } => {
                            ui.label(
                                RichText::new(body)
                                    .size(15.0)
                                    .color(ui.visuals().text_color()),
                            );
                        }
                        MessagePayload::Attachment {
                            file_name,
                            mime,
                            bytes,
                            caption,
                        } => {
                            Frame::new()
                                .fill(ui.visuals().widgets.inactive.bg_fill)
                                .corner_radius(8)
                                .inner_margin(12)
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("Encrypted attachment")
                                            .size(10.0)
                                            .color(ui.visuals().selection.bg_fill),
                                    );
                                    ui.label(RichText::new(file_name).strong());
                                    ui.label(
                                        RichText::new(format!(
                                            "{} • {}",
                                            mime,
                                            human_bytes(bytes.len())
                                        ))
                                        .size(11.0)
                                        .color(ui.visuals().weak_text_color()),
                                    );
                                    if !caption.is_empty() {
                                        ui.label(RichText::new(caption));
                                    }
                                    if ui.button("Save a local copy").clicked() {
                                        action = Some(MessageAction::SaveAttachment(
                                            file_name.clone(),
                                            bytes.clone(),
                                        ));
                                    }
                                });
                        }
                    }
                });
            });
        });
    action
}

fn message_copy_text(entry: &TimelineEntry) -> String {
    match &entry.payload {
        MessagePayload::Text { body } | MessagePayload::System { body } => body.clone(),
        MessagePayload::Attachment {
            file_name, caption, ..
        } => {
            if caption.is_empty() {
                file_name.clone()
            } else {
                format!("{file_name} — {caption}")
            }
        }
    }
}

fn message_quote(entry: &TimelineEntry) -> String {
    let text = message_copy_text(entry).replace('\n', " ");
    let short = text.chars().take(120).collect::<String>();
    format!("> {}: {short}\n", entry.author_name)
}

fn message_matches(entry: &TimelineEntry, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || entry.author_name.to_lowercase().contains(&needle) {
        return true;
    }
    match &entry.payload {
        MessagePayload::Text { body } | MessagePayload::System { body } => {
            body.to_lowercase().contains(&needle)
        }
        MessagePayload::Attachment {
            file_name,
            mime,
            caption,
            ..
        } => [file_name, mime, caption]
            .iter()
            .any(|value| value.to_lowercase().contains(&needle)),
    }
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
                .color(ui.visuals().text_color()),
        );
        ui.label(
            RichText::new(
                "Fast native chat. Your messages live with you and the peers you choose.",
            )
            .size(16.0)
            .color(ui.visuals().weak_text_color()),
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
            .fill(ui.visuals().selection.bg_fill)
            .corner_radius(7)
            .min_size(Vec2::new(140.0, 38.0)),
    )
}

fn settings_heading(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.label(
        RichText::new(title)
            .size(18.0)
            .strong()
            .color(ui.visuals().text_color()),
    );
    ui.label(
        RichText::new(description)
            .size(12.0)
            .color(ui.visuals().weak_text_color()),
    );
    ui.add_space(10.0);
}

fn settings_divider(ui: &mut egui::Ui) {
    ui.add_space(18.0);
    ui.separator();
    ui.add_space(18.0);
}
