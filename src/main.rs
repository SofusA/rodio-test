use std::fs;
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use parking_lot::Mutex;
use rodio::cpal::BufferSize;
use rodio::cpal::traits::HostTrait;
use rodio::queue::queue;
use rodio::{Decoder, Source};
use tokio::sync::watch::{self, Receiver, Sender};
use tokio::task::JoinHandle;
use tokio::time::sleep;

pub struct Sink {
    player: Option<rodio::Player>,
    mixer: Option<rodio::MixerDeviceSink>,
    sender: Option<Arc<rodio::queue::SourcesQueueInput>>,
    track_finished: Sender<()>,
    track_handle: Option<JoinHandle<()>>,
    duration_played: Arc<Mutex<Duration>>,
}

impl Default for Sink {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink {
    pub fn new() -> Self {
        let (track_finished, _) = watch::channel(());
        Self {
            player: None,
            mixer: None,
            sender: None,
            track_finished,
            track_handle: Default::default(),
            duration_played: Default::default(),
        }
    }

    pub fn track_finished(&self) -> Receiver<()> {
        self.track_finished.subscribe()
    }

    pub fn position(&self) -> Duration {
        let position = self
            .player
            .as_ref()
            .map(|x| x.get_pos())
            .unwrap_or_default();

        let duration_played = *self.duration_played.lock();

        if position < duration_played {
            return Default::default();
        }

        position - duration_played
    }

    pub fn play(&self) {
        if let Some(player) = &self.player {
            player.play();
        }
    }

    pub fn pause(&self) {
        if let Some(player) = &self.player {
            player.pause();
        }
    }

    pub fn seek(&self, duration: Duration) {
        if let Some(player) = &self.player {
            player.try_seek(duration).unwrap();
        }
    }

    pub fn clear(&mut self) {
        self.clear_queue();

        self.player = None;
        self.mixer = None;
        self.sender = None;

        *self.duration_played.lock() = Default::default();

        if let Some(handle) = self.track_handle.take() {
            handle.abort();
        }
    }

    pub fn clear_queue(&mut self) {
        *self.duration_played.lock() = Default::default();

        if let Some(player) = self.player.as_ref() {
            player.clear();
        };
    }

    pub fn is_empty(&self) -> bool {
        self.player.is_none()
    }

    pub fn query_track(&mut self, track_path: &Path) -> QueryTrackResult {
        let file = fs::File::open(track_path).unwrap();

        let source = Decoder::try_from(file).unwrap();

        let sample_rate = source.sample_rate();
        let same_sample_rate = self
            .mixer
            .as_ref()
            .map(|mixer| mixer.config().sample_rate() == sample_rate)
            .unwrap_or(true);

        if !same_sample_rate {
            return QueryTrackResult::RecreateStreamRequired;
        }

        let needs_stream = self.mixer.is_none() || self.player.is_none();

        if needs_stream {
            let mut mixer = open_default_stream(sample_rate);
            mixer.log_on_drop(false);

            let (sender, receiver) = queue(true);
            let player = rodio::Player::connect_new(mixer.mixer());
            player.append(receiver);
            set_volume(&player, &1.0);

            self.player = Some(player);
            self.sender = Some(sender);
            self.mixer = Some(mixer);
        }

        let track_finished = self.track_finished.clone();
        let track_duration = source.total_duration().unwrap_or_default();

        let duration_played = self.duration_played.clone();
        let signal = self.sender.as_ref().unwrap().append_with_signal(source);

        let track_handle = tokio::spawn(async move {
            loop {
                if signal.try_recv().is_ok() {
                    *duration_played.lock() += track_duration;
                    track_finished.send(()).expect("infallible");
                    break;
                }
                sleep(Duration::from_millis(200)).await;
            }
        });

        self.track_handle = Some(track_handle);

        QueryTrackResult::Queued
    }

    pub fn sync_volume(&self) {
        if let Some(player) = &self.player {
            set_volume(player, &1.0);
        }
    }
}

fn set_volume(sink: &rodio::Player, volume: &f32) {
    let volume = volume.clamp(0.0, 1.0).powi(3);
    sink.set_volume(volume);
}

fn open_default_stream(sample_rate: NonZero<u32>) -> rodio::MixerDeviceSink {
    rodio::DeviceSinkBuilder::from_default_device()
        .and_then(|x| {
            x.with_sample_rate(sample_rate)
                .with_buffer_size(BufferSize::Fixed(1024))
                .open_stream()
        })
        .or_else(|original_err| {
            let mut devices = rodio::cpal::default_host().output_devices().unwrap();

            devices
                .find_map(|d| {
                    rodio::DeviceSinkBuilder::from_device(d)
                        .and_then(|x| {
                            x.with_sample_rate(sample_rate)
                                .with_buffer_size(BufferSize::Fixed(1024))
                                .open_stream()
                        })
                        .ok()
                })
                .ok_or(original_err)
        })
        .unwrap()
}

pub enum QueryTrackResult {
    Queued,
    RecreateStreamRequired,
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.clear();
    }
}

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    file: PathBuf,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let mut sink = Sink::new();

    sink.query_track(&cli.file);

    loop {
        println!("position: {}", sink.position().as_secs());

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
