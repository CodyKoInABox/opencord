use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::Context;
use cpal::{
    Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use parking_lot::Mutex;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};
use serde::{Deserialize, Serialize};

use crate::{
    Node,
    model::{GroupId, PeerId},
    network::{IncomingVoice, VoicePacket},
};

const OPUS_RATE: u32 = 48_000;
const FRAME_SAMPLES: usize = 960;
const FRAME_DURATION: Duration = Duration::from_millis(20);

#[derive(Clone, Debug, Default)]
pub struct AudioSnapshot {
    pub group_id: Option<GroupId>,
    pub muted: bool,
    pub deafened: bool,
    pub input_level: f32,
    pub status: String,
}

pub struct AudioEngine {
    active: Option<ActiveAudio>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self {
            active: None,
            snapshot: Arc::new(Mutex::new(AudioSnapshot {
                status: "Voice disconnected".into(),
                ..AudioSnapshot::default()
            })),
        }
    }
}

impl AudioEngine {
    pub fn join(&mut self, node: &Node, group_id: GroupId) -> anyhow::Result<()> {
        self.leave();
        match ActiveAudio::start(node, group_id, self.snapshot.clone()) {
            Ok(active) => {
                self.active = Some(active);
                Ok(())
            }
            Err(error) => {
                self.snapshot.lock().status = format!("Voice unavailable: {error}");
                Err(error)
            }
        }
    }

    pub fn leave(&mut self) {
        self.active.take();
        *self.snapshot.lock() = AudioSnapshot {
            status: "Voice disconnected".into(),
            ..AudioSnapshot::default()
        };
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some(active) = &self.active {
            active.muted.store(muted, Ordering::Relaxed);
            let mut snapshot = self.snapshot.lock();
            snapshot.muted = muted;
            if muted {
                snapshot.input_level = 0.0;
            }
        }
    }

    pub fn set_deafened(&self, deafened: bool) {
        if let Some(active) = &self.active {
            active.deafened.store(deafened, Ordering::Relaxed);
            self.snapshot.lock().deafened = deafened;
        }
    }

    pub fn snapshot(&self) -> AudioSnapshot {
        self.snapshot.lock().clone()
    }
}

struct ActiveAudio {
    running: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    _input_stream: Stream,
    _output_stream: Stream,
    encoder: Option<JoinHandle<()>>,
    decoder: Option<JoinHandle<()>>,
}

impl ActiveAudio {
    fn start(
        node: &Node,
        group_id: GroupId,
        snapshot: Arc<Mutex<AudioSnapshot>>,
    ) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let input_device = host
            .default_input_device()
            .context("no default microphone")?;
        let output_device = host
            .default_output_device()
            .context("no default audio output")?;
        let input_supported = input_device
            .default_input_config()
            .context("read microphone format")?;
        let output_supported = output_device
            .default_output_config()
            .context("read speaker format")?;
        let input_format = input_supported.sample_format();
        let output_format = output_supported.sample_format();
        let input_config: StreamConfig = input_supported.into();
        let output_config: StreamConfig = output_supported.into();

        let (capture_tx, capture_rx) = mpsc::sync_channel::<Vec<f32>>(8);
        let ring = HeapRb::<f32>::new((output_config.sample_rate as usize).max(48_000));
        let (playback_producer, playback_consumer) = ring.split();
        let running = Arc::new(AtomicBool::new(true));
        let muted = Arc::new(AtomicBool::new(false));
        let deafened = Arc::new(AtomicBool::new(false));
        let (voice_sender, voice_receiver) = node.audio_channels();

        let input_stream = build_input_stream(
            &input_device,
            input_format,
            input_config,
            capture_tx,
            muted.clone(),
            snapshot.clone(),
        )?;
        let output_stream = build_output_stream(
            &output_device,
            output_format,
            output_config,
            playback_consumer,
        )?;

        let encoder_running = running.clone();
        let encoder_muted = muted.clone();
        let encoder = std::thread::Builder::new()
            .name("opencord-opus-encode".into())
            .spawn(move || {
                encode_loop(
                    encoder_running,
                    encoder_muted,
                    group_id,
                    voice_sender,
                    capture_rx,
                )
            })
            .context("start voice encoder")?;
        let decoder_running = running.clone();
        let decoder_deafened = deafened.clone();
        let output_rate = output_config.sample_rate;
        let decoder = std::thread::Builder::new()
            .name("opencord-opus-decode".into())
            .spawn(move || {
                decode_mix_loop(
                    decoder_running,
                    decoder_deafened,
                    group_id,
                    output_rate,
                    voice_receiver,
                    playback_producer,
                )
            })
            .context("start voice decoder")?;

        input_stream.play().context("start microphone")?;
        output_stream.play().context("start speakers")?;
        *snapshot.lock() = AudioSnapshot {
            group_id: Some(group_id),
            status: "Voice connected - direct encrypted mesh".into(),
            ..AudioSnapshot::default()
        };
        Ok(Self {
            running,
            muted,
            deafened,
            _input_stream: input_stream,
            _output_stream: output_stream,
            encoder: Some(encoder),
            decoder: Some(decoder),
        })
    }
}

impl Drop for ActiveAudio {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(thread) = self.encoder.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.decoder.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Serialize, Deserialize)]
struct EncodedAudioFrame {
    sequence: u32,
    opus: Vec<u8>,
}

fn encode_loop(
    running: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    group_id: GroupId,
    voice_sender: tokio::sync::broadcast::Sender<VoicePacket>,
    capture_rx: mpsc::Receiver<Vec<f32>>,
) {
    let Ok(mut encoder) =
        opus::Encoder::new(OPUS_RATE, opus::Channels::Mono, opus::Application::Voip)
    else {
        return;
    };
    let sequence = AtomicU32::new(0);
    let mut encoded = [0_u8; 1024];
    while running.load(Ordering::Relaxed) {
        let Ok(frame) = capture_rx.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        if muted.load(Ordering::Relaxed) || frame.len() != FRAME_SAMPLES {
            continue;
        }
        if let Ok(length) = encoder.encode_float(&frame, &mut encoded) {
            let packet = EncodedAudioFrame {
                sequence: sequence.fetch_add(1, Ordering::Relaxed),
                opus: encoded[..length].to_vec(),
            };
            if let Ok(bytes) = postcard::to_stdvec(&packet) {
                let _ = voice_sender.send(VoicePacket {
                    group_id,
                    bytes: Arc::new(bytes),
                });
            }
        }
    }
}

struct PeerDecoder {
    decoder: opus::Decoder,
    queued: VecDeque<f32>,
    last_sequence: Option<u32>,
    last_packet: Instant,
}

fn decode_mix_loop<P>(
    running: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    group_id: GroupId,
    output_rate: u32,
    mut voice_receiver: tokio::sync::broadcast::Receiver<IncomingVoice>,
    mut playback: P,
) where
    P: Producer<Item = f32> + Send + 'static,
{
    let mut peers = HashMap::<PeerId, PeerDecoder>::new();
    let mut next_mix = Instant::now() + Duration::from_millis(60);
    while running.load(Ordering::Relaxed) {
        loop {
            match voice_receiver.try_recv() {
                Ok(packet) if packet.group_id == group_id => {
                    let Ok(frame) = postcard::from_bytes::<EncodedAudioFrame>(&packet.bytes) else {
                        continue;
                    };
                    let decoder = peers.entry(packet.peer).or_insert_with(|| PeerDecoder {
                        decoder: opus::Decoder::new(OPUS_RATE, opus::Channels::Mono)
                            .expect("48 kHz mono Opus decoder is supported"),
                        queued: VecDeque::new(),
                        last_sequence: None,
                        last_packet: Instant::now(),
                    });
                    if decoder
                        .last_sequence
                        .is_some_and(|last| !sequence_is_newer(frame.sequence, last))
                    {
                        continue;
                    }
                    let mut pcm = [0_f32; FRAME_SAMPLES * 6];
                    if let Ok(samples) = decoder.decoder.decode_float(&frame.opus, &mut pcm, false)
                    {
                        decoder.queued.extend(&pcm[..samples]);
                        while decoder.queued.len() > FRAME_SAMPLES * 10 {
                            decoder.queued.pop_front();
                        }
                        decoder.last_sequence = Some(frame.sequence);
                        decoder.last_packet = Instant::now();
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return,
            }
        }
        let now = Instant::now();
        if now >= next_mix {
            next_mix += FRAME_DURATION;
            peers.retain(|_, peer| now.duration_since(peer.last_packet) < Duration::from_secs(5));
            if !deafened.load(Ordering::Relaxed) {
                let active = peers
                    .values()
                    .filter(|peer| !peer.queued.is_empty())
                    .count();
                if active > 0 {
                    let gain = 1.0 / (active as f32).sqrt();
                    let mut mixed = [0_f32; FRAME_SAMPLES];
                    for peer in peers.values_mut() {
                        for sample in &mut mixed {
                            *sample += peer.queued.pop_front().unwrap_or(0.0) * gain;
                        }
                    }
                    push_resampled(&mut playback, &mixed, output_rate);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn push_resampled<P: Producer<Item = f32>>(playback: &mut P, input: &[f32], output_rate: u32) {
    let ratio = output_rate as f64 / OPUS_RATE as f64;
    let mut phase = 0.0_f64;
    for &sample in input {
        phase += ratio;
        while phase >= 1.0 {
            let _ = playback.try_push(sample.clamp(-1.0, 1.0));
            phase -= 1.0;
        }
    }
}

fn sequence_is_newer(sequence: u32, previous: u32) -> bool {
    let difference = sequence.wrapping_sub(previous);
    difference != 0 && difference < (u32::MAX / 2)
}

fn build_input_stream(
    device: &Device,
    format: SampleFormat,
    config: StreamConfig,
    sender: mpsc::SyncSender<Vec<f32>>,
    muted: Arc<AtomicBool>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
) -> anyhow::Result<Stream> {
    macro_rules! build {
        ($type:ty) => {
            build_input::<$type>(device, config, sender, muted, snapshot)
        };
    }
    match format {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        _ => anyhow::bail!("microphone sample format {format} is unsupported"),
    }
}

fn build_input<T>(
    device: &Device,
    config: StreamConfig,
    sender: mpsc::SyncSender<Vec<f32>>,
    muted: Arc<AtomicBool>,
    snapshot: Arc<Mutex<AudioSnapshot>>,
) -> anyhow::Result<Stream>
where
    T: SizedSample + Copy + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let ratio = OPUS_RATE as f64 / config.sample_rate as f64;
    let mut phase = 0.0_f64;
    let mut frame = Vec::with_capacity(FRAME_SAMPLES);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            if muted.load(Ordering::Relaxed) {
                frame.clear();
                return;
            }
            let mut peak = 0.0_f32;
            for input in data.chunks(channels) {
                let sample = input
                    .iter()
                    .map(|value| f32::from_sample(*value))
                    .sum::<f32>()
                    / channels as f32;
                peak = peak.max(sample.abs());
                phase += ratio;
                while phase >= 1.0 {
                    frame.push(sample);
                    phase -= 1.0;
                    if frame.len() == FRAME_SAMPLES {
                        let _ = sender.try_send(std::mem::replace(
                            &mut frame,
                            Vec::with_capacity(FRAME_SAMPLES),
                        ));
                    }
                }
            }
            snapshot.lock().input_level = peak.min(1.0);
        },
        |error| tracing::warn!(%error, "microphone stream error"),
        None,
    )?;
    Ok(stream)
}

fn build_output_stream<C>(
    device: &Device,
    format: SampleFormat,
    config: StreamConfig,
    consumer: C,
) -> anyhow::Result<Stream>
where
    C: Consumer<Item = f32> + Send + 'static,
{
    macro_rules! build {
        ($type:ty) => {
            build_output::<$type, C>(device, config, consumer)
        };
    }
    match format {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        _ => anyhow::bail!("speaker sample format {format} is unsupported"),
    }
}

fn build_output<T, C>(
    device: &Device,
    config: StreamConfig,
    mut consumer: C,
) -> anyhow::Result<Stream>
where
    T: SizedSample + FromSample<f32>,
    C: Consumer<Item = f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                let sample = consumer.try_pop().unwrap_or(0.0);
                let output = T::from_sample(sample);
                frame.fill(output);
            }
        },
        |error| tracing::warn!(%error, "speaker stream error"),
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_frame_round_trips_and_fits_a_peer_datagram() {
        let mut encoder =
            opus::Encoder::new(OPUS_RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let mut decoder = opus::Decoder::new(OPUS_RATE, opus::Channels::Mono).unwrap();
        let input = (0..FRAME_SAMPLES)
            .map(|index| {
                ((index as f32 / OPUS_RATE as f32) * 440.0 * std::f32::consts::TAU).sin() * 0.2
            })
            .collect::<Vec<_>>();
        let mut packet = [0_u8; 1024];
        let size = encoder.encode_float(&input, &mut packet).unwrap();
        assert!(size < 1_000);
        let mut output = [0_f32; FRAME_SAMPLES * 2];
        let decoded = decoder
            .decode_float(&packet[..size], &mut output, false)
            .unwrap();
        assert_eq!(decoded, FRAME_SAMPLES);
    }

    #[test]
    fn wrapped_audio_sequences_are_ordered() {
        assert!(sequence_is_newer(0, u32::MAX));
        assert!(!sequence_is_newer(10, 10));
        assert!(!sequence_is_newer(9, 10));
    }
}
