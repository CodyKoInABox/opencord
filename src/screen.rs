use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::Context;
use image::{DynamicImage, codecs::jpeg::JpegEncoder, imageops::FilterType};
use parking_lot::Mutex;
use xcap::Monitor;

use crate::{Node, model::GroupId, network::ScreenPacket};

#[derive(Clone, Debug, Default)]
pub struct ScreenShareSnapshot {
    pub group_id: Option<GroupId>,
    pub frames_sent: u64,
    pub status: String,
}

pub struct ScreenShare {
    running: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    snapshot: Arc<Mutex<ScreenShareSnapshot>>,
}

impl Default for ScreenShare {
    fn default() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            worker: None,
            snapshot: Arc::new(Mutex::new(ScreenShareSnapshot {
                status: "Screen share stopped".into(),
                ..Default::default()
            })),
        }
    }
}

impl ScreenShare {
    pub fn start(&mut self, node: &Node, group_id: GroupId) -> anyhow::Result<()> {
        self.stop();
        let sender = node.screen_sender();
        let running = Arc::new(AtomicBool::new(true));
        self.running = running.clone();
        let snapshot = self.snapshot.clone();
        *snapshot.lock() = ScreenShareSnapshot {
            group_id: Some(group_id),
            status: "Starting screen capture…".into(),
            frames_sent: 0,
        };
        let worker = std::thread::Builder::new()
            .name("opencord-screen".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<()> {
                    let monitors = Monitor::all().context("enumerate displays")?;
                    let monitor = monitors
                        .into_iter()
                        .find(|monitor| monitor.is_primary().unwrap_or(false))
                        .context("no primary display")?;
                    snapshot.lock().status = "Sharing primary display at 2 FPS".into();
                    while running.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        let captured = monitor.capture_image().context("capture display")?;
                        let width = captured.width().min(1_280);
                        let height = ((captured.height() as f64 * width as f64
                            / captured.width() as f64)
                            .round() as u32)
                            .max(1);
                        let resized =
                            image::imageops::resize(&captured, width, height, FilterType::Triangle);
                        let mut jpeg = Vec::with_capacity((width * height / 3) as usize);
                        JpegEncoder::new_with_quality(&mut jpeg, 58)
                            .encode_image(&DynamicImage::ImageRgba8(resized))?;
                        if jpeg.len() <= 4 * 1024 * 1024 {
                            let _ = sender.send(ScreenPacket {
                                group_id,
                                jpeg: Arc::new(jpeg),
                            });
                            snapshot.lock().frames_sent += 1;
                        }
                        let remaining =
                            Duration::from_millis(500).saturating_sub(started.elapsed());
                        let deadline = Instant::now() + remaining;
                        while running.load(Ordering::Relaxed) && Instant::now() < deadline {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    snapshot.lock().status = format!("Screen share stopped: {error}");
                }
            })
            .context("start screen capture thread")?;
        self.worker = Some(worker);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        *self.snapshot.lock() = ScreenShareSnapshot {
            status: "Screen share stopped".into(),
            ..Default::default()
        };
    }

    pub fn snapshot(&self) -> ScreenShareSnapshot {
        self.snapshot.lock().clone()
    }
}

impl Drop for ScreenShare {
    fn drop(&mut self) {
        self.stop();
    }
}
