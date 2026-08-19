#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod settings;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use clap::Parser;
use directories::ProjectDirs;
use opencord::{MessagePayload, Node, NodeOptions};

#[derive(Debug, Parser)]
#[command(
    name = "opencord",
    version,
    about = "Lightweight peer-to-peer desktop chat"
)]
struct Arguments {
    /// Store this profile in a custom directory. Useful for running two local peers.
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Display name used when creating this profile.
    #[arg(long)]
    name: Option<String>,

    /// Direct QUIC UDP listen address.
    #[arg(long, default_value = "0.0.0.0:39217")]
    listen: SocketAddr,

    /// Connect directly to one or more peer addresses after startup.
    #[arg(long)]
    connect: Vec<SocketAddr>,

    /// Disable automatic LAN peer discovery.
    #[arg(long)]
    no_discovery: bool,

    /// Populate an empty profile with sample messages for UI evaluation.
    #[arg(long, hide = true)]
    demo: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "opencord=info".into()),
        )
        .with_target(false)
        .compact()
        .init();
    let arguments = Arguments::parse();
    let data_dir = arguments.data_dir.unwrap_or_else(default_data_dir);
    let settings = settings::AppSettings::load(&data_dir).unwrap_or_default();
    let mut options = NodeOptions::new(data_dir.clone());
    options.display_name = arguments
        .name
        .or_else(|| {
            (!settings.profile_name.trim().is_empty()).then(|| settings.profile_name.clone())
        })
        .or_else(default_display_name);
    options.listen = arguments.listen;
    options.enable_discovery = !arguments.no_discovery;
    let node = Node::start(options).context("start Opencord peer")?;
    for address in arguments.connect {
        node.connect(address)?;
    }
    if arguments.demo && node.groups()?.is_empty() {
        seed_demo(&node)?;
    }

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Opencord")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_icon(Arc::new(app_icon())),
        centered: true,
        wgpu_options: efficient_wgpu_options(),
        ..Default::default()
    };
    eframe::run_native(
        "Opencord",
        native_options,
        Box::new(move |creation| {
            Ok(Box::new(app::OpencordApp::new(
                creation, node, data_dir, settings,
            )))
        }),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn app_icon() -> eframe::egui::IconData {
    const SIZE: usize = 64;
    let mut rgba = vec![0_u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let index = (y * SIZE + x) * 4;
            let dx = (x as f32 - 31.5).abs();
            let dy = (y as f32 - 31.5).abs();
            let rounded = dx <= 27.5 && dy <= 27.5 && {
                let corner_x = (dx - 19.5).max(0.0);
                let corner_y = (dy - 19.5).max(0.0);
                corner_x * corner_x + corner_y * corner_y <= 8.0 * 8.0
            };
            if rounded {
                rgba[index..index + 4].copy_from_slice(&[112, 92, 255, 255]);
            }
            let bubble = (15..=49).contains(&x) && (17..=43).contains(&y);
            if bubble {
                rgba[index..index + 4].copy_from_slice(&[250, 250, 255, 255]);
            }
            let dot = |center_x: i32| {
                let x = x as i32 - center_x;
                let y = y as i32 - 30;
                x * x + y * y <= 3 * 3
            };
            if dot(26) || dot(38) {
                rgba[index..index + 4].copy_from_slice(&[112, 92, 255, 255]);
            }
        }
    }
    eframe::egui::IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

fn efficient_wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::{egui_wgpu::WgpuSetup, wgpu};

    let mut options = eframe::egui_wgpu::WgpuConfiguration::default()
        .with_surface_config(eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY);
    let WgpuSetup::CreateNew(setup) = &mut options.wgpu_setup else {
        return options;
    };

    // OpenGL presentation is unreliable on some Windows drivers. Prefer the
    // lower-residency Vulkan path, with DX12 retained as the universal fallback.
    setup.instance_descriptor.backends = wgpu::Backends::VULKAN | wgpu::Backends::DX12;
    setup.power_preference = wgpu::PowerPreference::LowPower;
    setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
        let selected = adapters
            .iter()
            .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
            .min_by_key(|adapter| {
                let info = adapter.get_info();
                let backend = match info.backend {
                    wgpu::Backend::Vulkan => 0,
                    wgpu::Backend::Dx12 => 1,
                    _ => 2,
                };
                let device = match info.device_type {
                    wgpu::DeviceType::DiscreteGpu => 0,
                    wgpu::DeviceType::IntegratedGpu => 1,
                    wgpu::DeviceType::Other => 2,
                    wgpu::DeviceType::Cpu => 3,
                    wgpu::DeviceType::VirtualGpu => 4,
                };
                (backend, device)
            })
            .cloned()
            .ok_or_else(|| "no Vulkan or DirectX 12 presentation adapter found".to_owned())?;
        let info = selected.get_info();
        tracing::info!(
            backend = ?info.backend,
            device = ?info.device_type,
            name = %info.name,
            "selected UI adapter"
        );
        Ok(selected)
    }));
    setup.device_descriptor = Arc::new(|_adapter| wgpu::DeviceDescriptor {
        label: Some("Opencord UI device"),
        required_limits: wgpu::Limits {
            max_texture_dimension_2d: 8_192,
            ..wgpu::Limits::default()
        },
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    });
    options
}

fn default_data_dir() -> PathBuf {
    ProjectDirs::from("dev", "CodyKoInABox", "Opencord")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".opencord"))
}

fn default_display_name() -> Option<String> {
    std::env::var("USERNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn seed_demo(node: &Node) -> anyhow::Result<()> {
    let (group, general) = node.create_group("Opencord Lab")?;
    node.create_channel(group.id, "architecture")?;
    node.create_channel(group.id, "random")?;
    for body in [
        "Welcome! This is the general channel.",
        "I left a few protocol notes in #architecture.",
        "Want to test voice after everyone joins?",
    ] {
        node.send(general.id, MessagePayload::Text { body: body.into() })?;
    }
    Ok(())
}
