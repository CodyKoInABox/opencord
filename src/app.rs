use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Layout, Margin,
    Rect, RichText, ScrollArea, Sense, Stroke, Vec2,
};
use opencord::{
    AudioEngine, Channel, ChannelId, Group, GroupId, IncomingScreen, MessagePayload,
    NetworkSnapshot, Node, ScreenShare, TimelineEntry,
};

use crate::settings::{AccentChoice, AppSettings, MessageDensity, ThemeChoice};

const GREEN: Color32 = Color32::from_rgb(72, 200, 142);
const RED: Color32 = Color32::from_rgb(239, 93, 112);

const RAIL_WIDTH: f32 = 76.0;
const SIDEBAR_WIDTH: f32 = 252.0;
const MEMBERS_WIDTH: f32 = 232.0;
const HEADER_HEIGHT: f32 = 64.0;
const SPACE_XS: f32 = 4.0;
const SPACE_SM: f32 = 8.0;
const SPACE_MD: f32 = 12.0;
const SPACE_LG: f32 = 16.0;
const SPACE_XL: f32 = 24.0;
const RADIUS_SM: u8 = 6;
const RADIUS_MD: u8 = 10;
const RADIUS_LG: u8 = 14;

trait PointerCursor {
    fn pointer_cursor(self) -> Self;
}

impl PointerCursor for egui::Response {
    fn pointer_cursor(self) -> Self {
        self.on_hover_cursor(CursorIcon::PointingHand)
    }
}

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SettingsSection {
    #[default]
    Profile,
    Appearance,
    Chat,
    Connection,
}

impl SettingsSection {
    const ALL: [Self; 4] = [
        Self::Profile,
        Self::Appearance,
        Self::Chat,
        Self::Connection,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::Appearance => "Appearance",
            Self::Chat => "Chat",
            Self::Connection => "Connection",
        }
    }
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
    settings_section: SettingsSection,
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
            settings_section: SettingsSection::default(),
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

    fn toggle_screen_share(&mut self) {
        if self.screen_share.snapshot().group_id.is_some() {
            self.screen_share.stop();
        } else if let Some(group) = self.selected_group
            && let Err(error) = self.screen_share.start(&self.node, group)
        {
            self.error(error);
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
            .exact_size(RAIL_WIDTH)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(palette.rail)
                    .inner_margin(Margin::symmetric(14, 12)),
            )
            .show(root, |ui| {
                ui.vertical_centered(|ui| {
                    if brand_mark(ui).on_hover_text("About Opencord").clicked() {
                        self.modal = Some(Modal::About);
                    }
                    ui.add_space(SPACE_MD);
                    ui.separator();
                    ui.add_space(SPACE_MD);
                    let groups = self.groups.clone();
                    for group in groups {
                        let selected = self.selected_group == Some(group.id);
                        if server_button(ui, &group, selected).clicked() {
                            self.select_group(group.id);
                        }
                        ui.add_space(SPACE_SM);
                    }
                    if round_icon_button(ui, "+", GREEN, "Create a group").clicked() {
                        self.modal = Some(Modal::CreateGroup {
                            name: String::new(),
                        });
                    }
                    ui.add_space(SPACE_SM);
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
        egui::Panel::left("channel_sidebar")
            .exact_size(SIDEBAR_WIDTH)
            .resizable(false)
            .frame(Frame::new().fill(palette.sidebar))
            .show(root, |ui| {
                egui::Panel::top("group_header")
                    .exact_size(HEADER_HEIGHT)
                    .frame(
                        Frame::new()
                            .fill(palette.sidebar)
                            .inner_margin(Margin::symmetric(SPACE_LG as i8, SPACE_MD as i8))
                            .stroke(Stroke::new(1.0, palette.border)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(
                                    self.current_group()
                                        .map(|group| group.name.as_str())
                                        .unwrap_or("Choose a group"),
                                )
                                .size(15.0)
                                .strong()
                                .color(palette.text),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let menu = ui.menu_button("•••", |ui| {
                                    if secondary_button(ui, "Invite people").clicked() {
                                        if let Some(group) = self.selected_group {
                                            match self.node.invite(group) {
                                                Ok(value) => {
                                                    self.modal = Some(Modal::Invite { value })
                                                }
                                                Err(error) => self.error(error),
                                            }
                                        }
                                        ui.close();
                                    }
                                    if secondary_button(ui, "Create channel").clicked() {
                                        self.modal = Some(Modal::CreateChannel {
                                            name: String::new(),
                                        });
                                        ui.close();
                                    }
                                    ui.separator();
                                    if secondary_button(ui, "Settings").clicked() {
                                        self.modal = Some(Modal::Settings);
                                        ui.close();
                                    }
                                });
                                menu.response.pointer_cursor();
                            });
                        });
                    });

                egui::Panel::bottom("identity_panel")
                    .exact_size(68.0)
                    .frame(Frame::new().fill(palette.rail))
                    .show(ui, |ui| {
                        user_panel(ui, &self.node, &mut self.modal);
                    });

                egui::Panel::bottom("voice_panel")
                    .exact_size(126.0)
                    .frame(
                        Frame::new()
                            .fill(palette.sidebar)
                            .inner_margin(Margin::symmetric(SPACE_MD as i8, SPACE_SM as i8)),
                    )
                    .show(ui, |ui| {
                        let audio = self.audio.snapshot();
                        let connected = audio.group_id.is_some();
                        let card_fill = if connected {
                            if palette.dark {
                                Color32::from_rgb(24, 48, 42)
                            } else {
                                Color32::from_rgb(223, 244, 237)
                            }
                        } else {
                            palette.surface
                        };
                        Frame::new()
                            .fill(card_fill)
                            .corner_radius(RADIUS_MD)
                            .inner_margin(Margin::symmetric(SPACE_MD as i8, SPACE_MD as i8))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(if connected {
                                        "Voice connected"
                                    } else {
                                        "Voice lounge"
                                    })
                                    .strong()
                                    .color(if connected {
                                        GREEN
                                    } else {
                                        palette.text
                                    }),
                                );
                                ui.label(
                                    RichText::new(if connected {
                                        "Direct encrypted mesh"
                                    } else {
                                        "Peer-to-peer group call"
                                    })
                                    .size(11.0)
                                    .color(palette.muted),
                                );
                                ui.add_space(SPACE_XS);
                                if connected {
                                    ui.horizontal(|ui| {
                                        let muted = audio.muted;
                                        if secondary_button(
                                            ui,
                                            if muted { "Unmute" } else { "Mute" },
                                        )
                                        .clicked()
                                        {
                                            self.audio.set_muted(!muted);
                                        }
                                        if secondary_button(ui, "Leave").clicked() {
                                            self.audio.leave();
                                        }
                                    });
                                } else if let Some(group_id) = self.selected_group
                                    && secondary_button(ui, "Join voice").clicked()
                                    && let Err(error) = self.audio.join(&self.node, group_id)
                                {
                                    self.error(error);
                                }
                            });
                    });

                egui::CentralPanel::default()
                    .frame(
                        Frame::new()
                            .fill(palette.sidebar)
                            .inner_margin(Margin::symmetric(SPACE_SM as i8, SPACE_MD as i8)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            section_label(ui, "Text channels");
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if quiet_button(ui, "+")
                                    .on_hover_text("Create channel")
                                    .clicked()
                                {
                                    self.modal = Some(Modal::CreateChannel {
                                        name: String::new(),
                                    });
                                }
                            });
                        });
                        ui.add_space(SPACE_XS);
                        ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for channel in self.channels.clone() {
                                    if channel_button(
                                        ui,
                                        &channel.name,
                                        self.selected_channel == Some(channel.id),
                                        self.drafts.contains_key(&channel.id),
                                    )
                                    .clicked()
                                    {
                                        self.select_channel(channel.id);
                                    }
                                    ui.add_space(2.0);
                                }
                            });
                    });
            });
    }

    fn render_members(&mut self, root: &mut egui::Ui) {
        let palette = theme_palette(self.settings.theme);
        egui::Panel::right("members")
            .exact_size(MEMBERS_WIDTH)
            .resizable(false)
            .frame(Frame::new().fill(palette.sidebar))
            .show(root, |ui| {
                egui::Panel::bottom("member_connection")
                    .exact_size(142.0)
                    .frame(
                        Frame::new()
                            .fill(palette.sidebar)
                            .inner_margin(Margin::symmetric(SPACE_LG as i8, SPACE_MD as i8)),
                    )
                    .show(ui, |ui| {
                        Frame::new()
                            .fill(palette.surface)
                            .corner_radius(RADIUS_MD)
                            .inner_margin(Margin::symmetric(SPACE_MD as i8, SPACE_MD as i8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.painter().circle_filled(
                                        ui.next_widget_position() + Vec2::new(5.0, 8.0),
                                        4.0,
                                        GREEN,
                                    );
                                    ui.add_space(12.0);
                                    ui.label(
                                        RichText::new("Direct mesh").strong().color(palette.text),
                                    );
                                });
                                ui.label(
                                    RichText::new("End-to-end encrypted")
                                        .size(11.0)
                                        .color(palette.muted),
                                );
                                if let Some(address) = self.network.listen_address {
                                    ui.add_space(SPACE_XS);
                                    ui.label(
                                        RichText::new(address.to_string())
                                            .monospace()
                                            .size(9.0)
                                            .color(palette.muted),
                                    );
                                }
                            });
                    });

                egui::CentralPanel::default()
                    .frame(
                        Frame::new()
                            .fill(palette.sidebar)
                            .inner_margin(Margin::symmetric(SPACE_LG as i8, SPACE_LG as i8)),
                    )
                    .show(ui, |ui| {
                        section_label(
                            ui,
                            &format!("Members — {}", self.network.online_peers.len() + 1),
                        );
                        ui.add_space(SPACE_SM);
                        member_row(
                            ui,
                            &self.node.identity().display_name(),
                            true,
                            self.audio.snapshot().group_id.is_some(),
                        );
                        for peer in self.network.online_peers.clone() {
                            if self
                                .selected_group
                                .is_none_or(|group| peer.shared_groups.contains(&group))
                            {
                                let mut block = false;
                                ui.horizontal(|ui| {
                                    member_row(ui, &peer.name, true, false);
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let menu = ui.menu_button("•••", |ui| {
                                            if secondary_button(ui, "Block peer").clicked() {
                                                block = true;
                                                ui.close();
                                            }
                                        });
                                        menu.response.pointer_cursor();
                                    });
                                });
                                if block {
                                    match self.node.block_peer(peer.id) {
                                        Ok(()) => self.notice(format!("Blocked {}", peer.name)),
                                        Err(error) => self.error(error),
                                    }
                                }
                            }
                        }
                    });
            });
    }

    fn render_header(&mut self, root: &mut egui::Ui) {
        let palette = theme_palette(self.settings.theme);
        egui::Panel::top("chat_header")
            .exact_size(HEADER_HEIGHT)
            .frame(
                Frame::new()
                    .fill(palette.canvas)
                    .inner_margin(Margin::symmetric(SPACE_LG as i8, SPACE_MD as i8))
                    .stroke(Stroke::new(1.0, palette.border)),
            )
            .show(root, |ui| {
                let available = ui.available_width();
                let show_status = available >= 680.0;
                let show_connect = available >= 650.0;
                let show_share = available >= 540.0;
                ui.horizontal(|ui| {
                    Frame::new()
                        .fill(palette.surface)
                        .corner_radius(RADIUS_SM)
                        .inner_margin(Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.label(RichText::new("#").size(18.0).color(palette.muted));
                        });
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
                        ui.add_space(SPACE_SM);
                        status_badge(ui, "End-to-end encrypted", GREEN, palette.surface);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let sharing = self.screen_share.snapshot().group_id.is_some();
                        if !show_share || !show_connect {
                            let menu = ui.menu_button("More", |ui| {
                                if !show_share
                                    && secondary_button(
                                        ui,
                                        if sharing {
                                            "Stop sharing"
                                        } else {
                                            "Share screen"
                                        },
                                    )
                                    .clicked()
                                {
                                    self.toggle_screen_share();
                                    ui.close();
                                }
                                if !show_connect
                                    && secondary_button(ui, "Connect directly").clicked()
                                {
                                    self.modal = Some(Modal::Connect {
                                        address: String::new(),
                                    });
                                    ui.close();
                                }
                            });
                            menu.response.pointer_cursor();
                        }
                        if show_share
                            && secondary_button(
                                ui,
                                if sharing {
                                    "Stop sharing"
                                } else {
                                    "Share screen"
                                },
                            )
                            .clicked()
                        {
                            self.toggle_screen_share();
                        }
                        if secondary_button(ui, "Invite").clicked()
                            && let Some(group) = self.selected_group
                        {
                            match self.node.invite(group) {
                                Ok(value) => self.modal = Some(Modal::Invite { value }),
                                Err(error) => self.error(error),
                            }
                        }
                        if show_connect && secondary_button(ui, "Connect").clicked() {
                            self.modal = Some(Modal::Connect {
                                address: String::new(),
                            });
                        }
                        if self.search_open {
                            let search = ui.add_sized(
                                [190.0, 36.0],
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
                        } else if secondary_button(ui, "Search   Ctrl+F")
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
            .exact_size(108.0)
            .frame(
                Frame::new()
                    .fill(palette.canvas)
                    .inner_margin(Margin::symmetric(SPACE_LG as i8, SPACE_SM as i8))
                    .stroke(Stroke::new(1.0, palette.border)),
            )
            .show(root, |ui| {
                if self.selected_channel.is_none() {
                    return;
                }
                Frame::new()
                    .fill(palette.surface)
                    .corner_radius(RADIUS_MD)
                    .stroke(Stroke::new(1.0, palette.border))
                    .inner_margin(Margin::symmetric(SPACE_MD as i8, SPACE_SM as i8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if quiet_button(ui, "+")
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
                                [ui.available_width() - 76.0, 40.0],
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
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Send").strong().color(Color32::WHITE),
                                    )
                                    .fill(accent_color(self.settings.accent))
                                    .corner_radius(RADIUS_SM)
                                    .min_size(Vec2::new(64.0, 36.0)),
                                )
                                .pointer_cursor()
                                .clicked()
                            {
                                self.send_composer();
                            }
                        });
                    });
                ui.add_space(SPACE_XS);
                let send_hint = if self.settings.enter_to_send {
                    "Enter to send  •  Shift+Enter for a new line"
                } else {
                    "Use Send when your message is ready"
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Encrypted").size(10.0).strong().color(GREEN));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(send_hint).size(10.0).color(palette.muted));
                    });
                });
            });
    }

    fn render_timeline(&mut self, root: &mut egui::Ui) {
        let palette = theme_palette(self.settings.theme);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(palette.canvas)
                    .inner_margin(Margin::symmetric(SPACE_LG as i8, SPACE_LG as i8)),
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
                                        if secondary_button(ui, "Close view").clicked() {
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
                        ui.add_space(if compact { SPACE_XS } else { SPACE_SM });
                        if self.settings.show_channel_intro && self.search_query.is_empty() {
                            channel_intro(ui, self.current_channel(), compact);
                            ui.add_space(if compact { SPACE_MD } else { SPACE_XL });
                        } else if !self.search_query.is_empty() {
                            let matches = self
                                .timeline
                                .iter()
                                .filter(|entry| message_matches(entry, &self.search_query))
                                .count();
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} result{} for “{}”",
                                        matches,
                                        if matches == 1 { "" } else { "s" },
                                        self.search_query.trim()
                                    ))
                                    .strong(),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if quiet_button(ui, "Clear").clicked() {
                                        self.search_query.clear();
                                        self.search_open = false;
                                    }
                                });
                            });
                            ui.add_space(SPACE_LG);
                        }
                        let mut message_action = None;
                        for (index, entry) in self.timeline.iter().enumerate() {
                            if !self.search_query.is_empty()
                                && !message_matches(entry, &self.search_query)
                            {
                                continue;
                            }
                            let grouped = index.checked_sub(1).is_some_and(|previous| {
                                let before = &self.timeline[previous];
                                before.event.header.author == entry.event.header.author
                                    && entry
                                        .event
                                        .header
                                        .sent_at_ms
                                        .saturating_sub(before.event.header.sent_at_ms)
                                        < 5 * 60 * 1_000
                            });
                            if let Some(action) = message_row(
                                ui,
                                entry,
                                self.settings.show_message_ids,
                                self.settings.density,
                                grouped,
                            ) {
                                message_action = Some(action);
                            }
                            ui.add_space(if grouped { 0.0 } else { SPACE_XS });
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

    fn render_settings_content(
        &mut self,
        ui: &mut egui::Ui,
        palette: ThemePalette,
        settings_changed: &mut bool,
        save_profile: &mut bool,
    ) {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(164.0, 410.0),
                Layout::top_down(Align::Min),
                |ui| {
                    section_label(ui, "User settings");
                    ui.add_space(SPACE_SM);
                    for section in SettingsSection::ALL {
                        if settings_nav_button(ui, section, self.settings_section == section)
                            .clicked()
                        {
                            self.settings_section = section;
                        }
                        ui.add_space(2.0);
                    }
                    ui.add_space(180.0);
                    ui.label(
                        RichText::new(format!("Opencord {}", env!("CARGO_PKG_VERSION")))
                            .size(10.0)
                            .color(palette.muted),
                    );
                },
            );
            ui.separator();
            ui.add_space(SPACE_LG);
            ui.allocate_ui_with_layout(
                Vec2::new(540.0, 410.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ScrollArea::vertical()
                        .max_height(390.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| match self.settings_section {
                            SettingsSection::Profile => {
                                settings_heading(ui, "Profile", "How you appear to other peers");
                                Frame::new()
                                    .fill(palette.surface)
                                    .corner_radius(RADIUS_LG)
                                    .inner_margin(Margin::symmetric(18, 18))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            avatar(ui, &self.profile_name_draft, true, 52.0);
                                            ui.add_space(SPACE_SM);
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new("Display name")
                                                        .size(11.0)
                                                        .strong()
                                                        .color(palette.muted),
                                                );
                                                ui.add_sized(
                                                    [300.0, 38.0],
                                                    egui::TextEdit::singleline(
                                                        &mut self.profile_name_draft,
                                                    )
                                                    .hint_text("Display name"),
                                                );
                                            });
                                        });
                                        ui.add_space(SPACE_LG);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(format!(
                                                    "Peer ID {}",
                                                    self.node.identity().peer_id().short()
                                                ))
                                                .monospace()
                                                .size(10.0)
                                                .color(palette.muted),
                                            );
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    if primary_button(ui, "Save profile").clicked()
                                                    {
                                                        *save_profile = true;
                                                    }
                                                },
                                            );
                                        });
                                    });
                                ui.add_space(SPACE_LG);
                                ui.label(
                                    RichText::new(
                                        "Your display name is signed with your local identity and shared on new peer connections.",
                                    )
                                    .size(11.0)
                                    .color(palette.muted),
                                );
                            }
                            SettingsSection::Appearance => {
                                settings_heading(
                                    ui,
                                    "Appearance",
                                    "Make Opencord comfortable on your display",
                                );
                                section_label(ui, "Theme");
                                ui.add_space(SPACE_SM);
                                ui.horizontal_wrapped(|ui| {
                                    for theme in ThemeChoice::ALL {
                                        let swatch = theme_palette(theme);
                                        let selected = self.settings.theme == theme;
                                        let response = Frame::new()
                                            .fill(swatch.canvas)
                                            .stroke(Stroke::new(
                                                if selected { 2.0 } else { 1.0 },
                                                if selected {
                                                    accent_color(self.settings.accent)
                                                } else {
                                                    swatch.border
                                                },
                                            ))
                                            .corner_radius(RADIUS_MD)
                                            .inner_margin(Margin::symmetric(16, 14))
                                            .show(ui, |ui| {
                                                ui.set_min_width(100.0);
                                                ui.label(
                                                    RichText::new(theme.label())
                                                        .color(swatch.text)
                                                        .strong(),
                                                );
                                                ui.label(
                                                    RichText::new(if swatch.dark {
                                                        "Dark"
                                                    } else {
                                                        "Light"
                                                    })
                                                    .size(10.0)
                                                    .color(swatch.muted),
                                                );
                                            })
                                            .response
                                            .interact(Sense::click())
                                            .pointer_cursor();
                                        if response.clicked() && !selected {
                                            self.settings.theme = theme;
                                            *settings_changed = true;
                                        }
                                    }
                                });

                                ui.add_space(SPACE_LG);
                                section_label(ui, "Accent color");
                                ui.add_space(SPACE_SM);
                                ui.horizontal(|ui| {
                                    for accent in AccentChoice::ALL {
                                        let selected = self.settings.accent == accent;
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new(accent.label())
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(accent_color(accent))
                                                .stroke(Stroke::new(
                                                    if selected { 2.0 } else { 0.0 },
                                                    Color32::WHITE,
                                                ))
                                                .corner_radius(RADIUS_SM)
                                                .min_size(Vec2::new(76.0, 36.0)),
                                            )
                                            .pointer_cursor()
                                            .clicked()
                                            && !selected
                                        {
                                            self.settings.accent = accent;
                                            *settings_changed = true;
                                        }
                                    }
                                });

                                ui.add_space(SPACE_LG);
                                Frame::new()
                                    .fill(palette.surface)
                                    .corner_radius(RADIUS_MD)
                                    .inner_margin(Margin::symmetric(16, 14))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new("Interface scale").strong(),
                                                );
                                                ui.label(
                                                    RichText::new("Resize text and controls")
                                                        .size(11.0)
                                                        .color(palette.muted),
                                                );
                                            });
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{:.0}%",
                                                            self.settings.ui_scale * 100.0
                                                        ))
                                                        .monospace()
                                                        .size(11.0),
                                                    );
                                                    *settings_changed |= ui
                                                        .scope(|ui| {
                                                            ui.spacing_mut().slider_width = 160.0;
                                                            ui.spacing_mut().slider_rail_height = 4.0;
                                                            ui.visuals_mut()
                                                                .widgets
                                                                .inactive
                                                                .bg_fill = palette.surface_hover;
                                                            ui.add(
                                                            egui::Slider::new(
                                                                &mut self.settings.ui_scale,
                                                                0.85..=1.20,
                                                            )
                                                            .step_by(0.05)
                                                                .show_value(false),
                                                            )
                                                        })
                                                        .inner
                                                        .on_hover_and_drag_cursor(
                                                            CursorIcon::ResizeHorizontal,
                                                        )
                                                        .changed();
                                                },
                                            );
                                        });
                                    });

                                ui.add_space(SPACE_MD);
                                Frame::new()
                                    .fill(palette.surface)
                                    .corner_radius(RADIUS_MD)
                                    .inner_margin(Margin::symmetric(16, 14))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new("Message spacing").strong(),
                                                );
                                                ui.label(
                                                    RichText::new("Choose a cozy or dense timeline")
                                                        .size(11.0)
                                                        .color(palette.muted),
                                                );
                                            });
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    let compact_selected = self.settings.density
                                                        == MessageDensity::Compact;
                                                    let cozy_selected = self.settings.density
                                                        == MessageDensity::Cozy;
                                                    *settings_changed |= ui
                                                        .selectable_value(
                                                            &mut self.settings.density,
                                                            MessageDensity::Compact,
                                                            RichText::new(
                                                                MessageDensity::Compact.label(),
                                                            )
                                                            .color(if compact_selected {
                                                                Color32::WHITE
                                                            } else {
                                                                palette.text
                                                            }),
                                                        )
                                                        .pointer_cursor()
                                                        .changed();
                                                    *settings_changed |= ui
                                                        .selectable_value(
                                                            &mut self.settings.density,
                                                            MessageDensity::Cozy,
                                                            RichText::new(
                                                                MessageDensity::Cozy.label(),
                                                            )
                                                            .color(if cozy_selected {
                                                                Color32::WHITE
                                                            } else {
                                                                palette.text
                                                            }),
                                                        )
                                                        .pointer_cursor()
                                                        .changed();
                                                },
                                            );
                                        });
                                    });
                            }
                            SettingsSection::Chat => {
                                settings_heading(
                                    ui,
                                    "Chat",
                                    "Control how messages are composed and displayed",
                                );
                                *settings_changed |= settings_toggle(
                                    ui,
                                    &mut self.settings.enter_to_send,
                                    "Enter to send",
                                    "Use Shift+Enter to start a new line",
                                );
                                ui.add_space(SPACE_SM);
                                *settings_changed |= settings_toggle(
                                    ui,
                                    &mut self.settings.show_channel_intro,
                                    "Channel introductions",
                                    "Show a short heading at the start of each channel",
                                );
                                ui.add_space(SPACE_SM);
                                *settings_changed |= settings_toggle(
                                    ui,
                                    &mut self.settings.show_message_ids,
                                    "Message IDs",
                                    "Show signed author sequence IDs beside timestamps",
                                );
                                ui.add_space(SPACE_SM);
                                *settings_changed |= settings_toggle(
                                    ui,
                                    &mut self.settings.show_member_list,
                                    "Member list",
                                    "Keep the member panel visible when the window is wide",
                                );
                            }
                            SettingsSection::Connection => {
                                settings_heading(
                                    ui,
                                    "Connection",
                                    "Current peer-to-peer transport status",
                                );
                                Frame::new()
                                    .fill(palette.surface)
                                    .corner_radius(RADIUS_LG)
                                    .inner_margin(Margin::symmetric(18, 18))
                                    .show(ui, |ui| {
                                        status_badge(
                                            ui,
                                            "Listening for peers",
                                            GREEN,
                                            palette.surface_hover,
                                        );
                                        ui.add_space(SPACE_LG);
                                        ui.label(
                                            RichText::new(format!(
                                                "{} peer{} online",
                                                self.network.online_peers.len(),
                                                if self.network.online_peers.len() == 1 {
                                                    ""
                                                } else {
                                                    "s"
                                                }
                                            ))
                                            .size(20.0)
                                            .strong(),
                                        );
                                        if let Some(address) = self.network.listen_address {
                                            ui.label(
                                                RichText::new(format!("UDP {address}"))
                                                    .monospace()
                                                    .color(palette.muted),
                                            );
                                        }
                                        ui.add_space(SPACE_XL);
                                        ui.separator();
                                        ui.add_space(SPACE_LG);
                                        ui.label(
                                            RichText::new("Transport and identity")
                                                .strong(),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "QUIC • Ed25519 • XChaCha20-Poly1305 • local SQLite history",
                                            )
                                            .size(11.0)
                                            .color(palette.muted),
                                        );
                                    });
                            }
                        });
                },
            );
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
        let uses_action_footer = matches!(
            &modal,
            Modal::CreateGroup { .. }
                | Modal::JoinGroup { .. }
                | Modal::Invite { .. }
                | Modal::Connect { .. }
                | Modal::CreateChannel { .. }
        );
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
        let mut window = egui::Window::new(title)
            .id(Id::new("opencord_modal"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .frame(
                Frame::window(&ctx.global_style())
                    .fill(palette.sidebar)
                    .corner_radius(RADIUS_LG)
                    .inner_margin(20),
            );
        if matches!(modal, Modal::Settings) {
            window = window.fixed_size(Vec2::new(780.0, 520.0));
        }
        window.show(ctx, |ui| {
                ui.set_min_width(if matches!(modal, Modal::Settings) { 740.0 } else { 440.0 });
                match &mut modal {
                    Modal::CreateGroup { name } => {
                        ui.label(RichText::new("Start a private peer-to-peer space.").color(palette.muted));
                        ui.add_space(12.0);
                        ui.text_edit_singleline(name).request_focus();
                        let (create, cancel) = modal_actions(ui, "Create group");
                        if cancel {
                            keep = false;
                        }
                        if create {
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
                        let (join, cancel) = modal_actions(ui, "Verify and join");
                        if cancel {
                            keep = false;
                        }
                        if join {
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
                        let (copy, close) = modal_actions(ui, "Copy invite");
                        if close {
                            keep = false;
                        }
                        if copy {
                            ctx.copy_text(value.clone()); self.notice("Invite copied");
                        }
                    }
                    Modal::Connect { address } => {
                        ui.label(RichText::new("Enter a peer's reachable UDP address.").color(palette.muted));
                        ui.add_space(10.0);
                        ui.text_edit_singleline(address);
                        let (connect, cancel) = modal_actions(ui, "Connect");
                        if cancel {
                            keep = false;
                        }
                        if connect {
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
                        let (create, cancel) = modal_actions(ui, "Create channel");
                        if cancel {
                            keep = false;
                        }
                        if create && let Some(group) = self.selected_group {
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
                                    .pointer_cursor()
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
                        self.render_settings_content(
                            ui,
                            palette,
                            &mut settings_changed,
                            &mut save_profile,
                        );
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
                        ui.hyperlink_to("github.com/CodyKoInABox", "https://github.com/CodyKoInABox")
                            .pointer_cursor();
                        ui.hyperlink_to("Opencord on GitHub", "https://github.com/CodyKoInABox/opencord")
                            .pointer_cursor();
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new("AGPL-3.0-or-later")
                                .strong()
                                .color(palette.text),
                        );
                        ui.label(RichText::new("Free software licensed under the GNU Affero General Public License.").color(palette.muted));
                        ui.hyperlink_to("Read the license", "https://www.gnu.org/licenses/agpl-3.0.html")
                            .pointer_cursor();
                        ui.add_space(14.0);
                        ui.label(RichText::new("XChaCha20-Poly1305 • Ed25519 • QUIC • SQLite WAL • Opus").size(11.0).color(palette.muted));
                    }
                }
                if !uses_action_footer {
                    ui.add_space(SPACE_MD);
                    ui.separator();
                    ui.add_space(SPACE_SM);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if secondary_button(ui, "Close").clicked() {
                            keep = false;
                        }
                    });
                }
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
                Frame::new()
                    .fill(if *error {
                        Color32::from_rgb(83, 35, 44)
                    } else {
                        Color32::from_rgb(32, 70, 58)
                    })
                    .corner_radius(RADIUS_MD)
                    .inner_margin(Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.label(RichText::new(message).color(Color32::WHITE).strong());
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
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.open.bg_fill = palette.surface_hover;
    style.visuals.widgets.open.weak_bg_fill = palette.surface_hover;
    style.visuals.widgets.open.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.selection.bg_fill = accent;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    style.visuals.window_corner_radius = CornerRadius::same(RADIUS_LG);
    style.visuals.menu_corner_radius = CornerRadius::same(RADIUS_MD);
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(24.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(11.0));
    style.spacing.item_spacing = match settings.density {
        MessageDensity::Compact => Vec2::new(SPACE_SM, SPACE_XS),
        MessageDensity::Cozy => Vec2::new(SPACE_SM, SPACE_SM),
    };
    style.spacing.button_padding = Vec2::new(SPACE_MD, 7.0);
    style.spacing.interact_size = Vec2::new(36.0, 32.0);
    style.spacing.window_margin = Margin::same(SPACE_LG as i8);
    style.spacing.menu_margin = Margin::same(SPACE_SM as i8);
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    ctx.set_global_style(style);
}

fn brand_mark(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::click());
    let response = response.pointer_cursor();
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
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::click());
    let response = response.pointer_cursor();
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
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::click());
    let response = response.pointer_cursor();
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
        .inner_margin(Margin::symmetric(SPACE_MD as i8, SPACE_SM as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                avatar(ui, &display_name, true, 36.0);
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
                    if quiet_button(ui, "⚙")
                        .on_hover_text("Settings (Ctrl+,)")
                        .clicked()
                    {
                        *modal = Some(Modal::Settings);
                    }
                    if quiet_button(ui, "i")
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
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, RADIUS_LG, ui.visuals().widgets.inactive.bg_fill);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "#",
            FontId::proportional(26.0),
            ui.visuals().text_color(),
        );
        ui.add_space(SPACE_MD);
    }
    ui.label(
        RichText::new(format!("Welcome to #{name}"))
            .size(if compact { 21.0 } else { 25.0 })
            .strong()
            .color(ui.visuals().text_color()),
    );
    ui.label(
        RichText::new("This is the start of this channel.").color(ui.visuals().weak_text_color()),
    );
}

fn message_row(
    ui: &mut egui::Ui,
    entry: &TimelineEntry,
    show_message_id: bool,
    density: MessageDensity,
    grouped: bool,
) -> Option<MessageAction> {
    let mut action = None;
    let compact = density == MessageDensity::Compact;
    let predicted_height = match &entry.payload {
        MessagePayload::Attachment { .. } => 132.0,
        _ if grouped || compact => 32.0,
        _ => 56.0,
    };
    let hover_rect = Rect::from_min_size(
        ui.next_widget_position(),
        Vec2::new(ui.available_width(), predicted_height),
    );
    let hovered = ui.rect_contains_pointer(hover_rect);
    Frame::new()
        .inner_margin(Margin::symmetric(
            SPACE_SM as i8,
            if grouped || compact { 3 } else { 8 },
        ))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                if grouped {
                    ui.add_space(if compact { 40.0 } else { 48.0 });
                } else {
                    avatar(
                        ui,
                        &entry.author_name,
                        true,
                        if compact { 34.0 } else { 40.0 },
                    );
                }
                let content_width = (ui.available_width() - 116.0).max(100.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(content_width, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        if !grouped {
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
                            });
                        }
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
                                    .corner_radius(RADIUS_MD)
                                    .stroke(Stroke::new(
                                        1.0,
                                        ui.visuals().widgets.inactive.bg_stroke.color,
                                    ))
                                    .inner_margin(Margin::symmetric(14, 12))
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
                                        if secondary_button(ui, "Save a local copy").clicked() {
                                            action = Some(MessageAction::SaveAttachment(
                                                file_name.clone(),
                                                bytes.clone(),
                                            ));
                                        }
                                    });
                            }
                        }
                    },
                );
                ui.allocate_ui_with_layout(
                    Vec2::new(108.0, 30.0),
                    Layout::right_to_left(Align::Min),
                    |ui| {
                        if hovered {
                            if message_action_button(ui, "Copy").clicked() {
                                ui.ctx().copy_text(message_copy_text(entry));
                            }
                            if message_action_button(ui, "Reply").clicked() {
                                action = Some(MessageAction::Reply(message_quote(entry)));
                            }
                        }
                    },
                );
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
        if secondary_button(ui, "Join with an invite").clicked() {
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
            .corner_radius(RADIUS_SM)
            .min_size(Vec2::new(140.0, 38.0)),
    )
    .pointer_cursor()
}

fn modal_actions(ui: &mut egui::Ui, primary_label: &str) -> (bool, bool) {
    let mut primary = false;
    let mut cancel = false;
    ui.add_space(SPACE_LG);
    ui.separator();
    ui.add_space(SPACE_SM);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        primary = primary_button(ui, primary_label).clicked();
        cancel = secondary_button(ui, "Cancel").clicked();
    });
    (primary, cancel)
}

fn secondary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = ui.visuals().text_color();
    ui.add(
        egui::Button::new(RichText::new(label).strong().color(text))
            .fill(ui.visuals().widgets.inactive.bg_fill)
            .stroke(Stroke::new(
                1.0,
                ui.visuals().widgets.inactive.bg_stroke.color,
            ))
            .corner_radius(RADIUS_SM)
            .min_size(Vec2::new(0.0, 36.0)),
    )
    .pointer_cursor()
}

fn quiet_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = ui.visuals().text_color();
    ui.add(
        egui::Button::new(RichText::new(label).strong().color(text))
            .frame(false)
            .corner_radius(RADIUS_SM)
            .min_size(Vec2::new(32.0, 32.0)),
    )
    .pointer_cursor()
}

fn message_action_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = ui.visuals().text_color();
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 26.0;
        ui.spacing_mut().button_padding = Vec2::new(8.0, 3.0);
        ui.add_sized(
            [50.0, 26.0],
            egui::Button::new(RichText::new(label).size(10.0).color(text))
                .fill(ui.visuals().widgets.inactive.bg_fill)
                .corner_radius(RADIUS_SM),
        )
        .pointer_cursor()
    })
    .inner
}

fn channel_button(
    ui: &mut egui::Ui,
    name: &str,
    selected: bool,
    has_draft: bool,
) -> egui::Response {
    let size = Vec2::new(ui.available_width(), 36.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response.pointer_cursor();
    let accent = ui.visuals().selection.bg_fill;
    let fill = if selected {
        accent
    } else if response.hovered() {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, RADIUS_SM, fill);
    if selected {
        ui.painter().rect_filled(
            Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + 7.0),
                egui::pos2(rect.left() + 3.0, rect.bottom() - 7.0),
            ),
            2.0,
            Color32::WHITE,
        );
    }
    let text_color = if selected {
        Color32::WHITE
    } else if response.hovered() {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        egui::pos2(rect.left() + 13.0, rect.center().y),
        Align2::LEFT_CENTER,
        "#",
        FontId::proportional(17.0),
        text_color.gamma_multiply(0.8),
    );
    ui.painter().text(
        egui::pos2(rect.left() + 35.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(14.0),
        text_color,
    );
    if has_draft {
        ui.painter().circle_filled(
            egui::pos2(rect.right() - 13.0, rect.center().y),
            3.0,
            if selected { Color32::WHITE } else { accent },
        );
    }
    response
}

fn section_label(ui: &mut egui::Ui, label: &str) {
    ui.label(
        RichText::new(label.to_uppercase())
            .size(10.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
}

fn status_badge(ui: &mut egui::Ui, label: &str, dot: Color32, fill: Color32) {
    Frame::new()
        .fill(fill)
        .corner_radius(RADIUS_SM)
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(7.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 3.5, dot);
                ui.label(
                    RichText::new(label)
                        .size(11.0)
                        .color(ui.visuals().weak_text_color()),
                );
            });
        });
}

fn settings_nav_button(
    ui: &mut egui::Ui,
    section: SettingsSection,
    selected: bool,
) -> egui::Response {
    let text = if selected {
        Color32::WHITE
    } else {
        ui.visuals().text_color()
    };
    ui.add_sized(
        [ui.available_width(), 38.0],
        egui::Button::new(RichText::new(section.label()).strong().color(text))
            .selected(selected)
            .fill(if selected {
                ui.visuals().selection.bg_fill
            } else {
                Color32::TRANSPARENT
            })
            .corner_radius(RADIUS_SM),
    )
    .pointer_cursor()
}

fn settings_toggle(ui: &mut egui::Ui, value: &mut bool, title: &str, description: &str) -> bool {
    let enabled = *value;
    let response = Frame::new()
        .fill(ui.visuals().widgets.inactive.weak_bg_fill)
        .corner_radius(RADIUS_MD)
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).strong());
                    ui.label(
                        RichText::new(description)
                            .size(11.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(40.0, 22.0), Sense::hover());
                    let accent = ui.visuals().selection.bg_fill;
                    ui.painter().rect_filled(
                        rect,
                        11.0,
                        if enabled {
                            accent
                        } else {
                            ui.visuals().widgets.hovered.bg_fill
                        },
                    );
                    let knob_x = if enabled {
                        rect.right() - 11.0
                    } else {
                        rect.left() + 11.0
                    };
                    ui.painter().circle_filled(
                        egui::pos2(knob_x, rect.center().y),
                        8.0,
                        Color32::WHITE,
                    );
                });
            });
        })
        .response
        .interact(Sense::click())
        .pointer_cursor();
    if response.clicked() {
        *value = !*value;
        true
    } else {
        false
    }
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
