use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, SampleRate, SupportedStreamConfig};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const CD_SAMPLE_RATE: u32 = 44100;
const DVD_SAMPLE_RATE: u32 = 48000;
const DVD_DIVISOR: u32 = 8000;
const NO_OUTPUT: &str = "No output device found";

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerConfig {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub loop_start: Option<usize>,
    pub end: Option<usize>,
}

pub struct Player {
    samples: Arc<Vec<f32>>,
    sample_rate: u32,
    loop_start: usize,
    end: usize,
    state: PlayerState,
    playhead: Arc<AtomicUsize>,
}

impl Player {
    pub fn new(config: &PlayerConfig) -> Result<Self, String> {
        let loop_start = config.loop_start.unwrap_or(0);
        let end = config.end.unwrap_or(config.samples.len());

        if config.sample_rate == 0 {
            return Err(String::from("Sample rate must be non-zero"));
        }

        if loop_start >= config.samples.len() {
            return Err(String::from("Loop start beyond input buffer"));
        }

        if end > config.samples.len() {
            return Err(String::from("End beyond input buffer"));
        }

        let device = cpal::default_host()
            .default_output_device()
            .ok_or(NO_OUTPUT)?;

        let stream_config = stream_config(&device, config.sample_rate)?;
        let outrate = stream_config.sample_rate().0;

        let mut playback_samples =
            resample(config.sample_rate, outrate, &config.samples);

        let end = scale_index(config.sample_rate, outrate, end)
            .ok_or("Scaled end too large")?
            .min(playback_samples.len());

        playback_samples.truncate(end);

        let loop_start = scale_index(config.sample_rate, outrate, loop_start)
            .ok_or("Scaled loop start too large")
            .and_then(|start| {
                if start < end {
                    Ok(start)
                } else {
                    Err("Start is after end")
                }
            })?;

        Ok(Player {
            samples: Arc::new(playback_samples),
            sample_rate: outrate,
            loop_start,
            end,
            state: PlayerState::Stopped,
            playhead: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn play(&mut self, looped: bool, play_from: usize) -> Result<(), String> {
        match self.state {
            PlayerState::PlayingLooped(_) | PlayerState::Playing(_) => {
                self.stop();
            }
            _ => {}
        };

        self.playhead.store(play_from, Ordering::Relaxed);

        let device = cpal::default_host()
            .default_output_device()
            .ok_or(NO_OUTPUT)?;

        // It's clunky to have to call this twice, but easier than
        // maintaining device and stream config in the struct
        let stream_config = stream_config(&device, self.sample_rate)?;

        if stream_config.sample_rate().0 != self.sample_rate {
            return Err(format!(
                "Failed to acquire stream config @ {}Hz",
                self.sample_rate
            ));
        }

        let loop_start = if looped { Some(self.loop_start) } else { None };

        let stream = Box::new(
            device
                .build_output_stream(
                    &stream_config.into(),
                    stream_callback(
                        Arc::clone(&self.samples),
                        Arc::clone(&self.playhead),
                        loop_start,
                        self.end,
                    ),
                    move |_| {},
                    None,
                )
                .map_err(|e| e.to_string())?,
        );

        stream.play().map_err(|e| e.to_string())?;

        self.state = if looped {
            PlayerState::PlayingLooped(stream)
        } else {
            PlayerState::Playing(stream)
        };

        Ok(())
    }

    pub fn play_from_start(&mut self) -> Result<(), String> {
        self.play(false, 0)
    }

    pub fn play_from_start_looped(&mut self) -> Result<(), String> {
        self.play(true, 0)
    }

    pub fn stop(&mut self) {
        // Stream gets dropped if state was previously Playing or PlayingLooped
        self.state = PlayerState::Stopped;

        self.playhead.store(0, Ordering::Relaxed);
    }

    pub fn pause(&mut self) {
        let looped = match self.state {
            PlayerState::Paused(_) => {
                return;
            }
            PlayerState::PlayingLooped(_) => true,
            _ => false,
        };

        // Stream gets dropped if state was previously Playing or PlayingLooped
        // Need to stop b/c we need the playhead location before setting Paused
        self.state = PlayerState::Stopped;

        let playhead = self.playhead.load(Ordering::Relaxed);

        self.state = PlayerState::Paused(PlaybackState { looped, playhead });
    }

    pub fn resume(&mut self) -> Result<(), String> {
        match self.state {
            PlayerState::PlayingLooped(_) | PlayerState::Playing(_) => {}
            PlayerState::Stopped => self.play(false, 0)?,
            PlayerState::Paused(PlaybackState { playhead, looped }) => {
                self.play(looped, playhead)?;
            }
        };

        Ok(())
    }

    pub fn playhead(&self) -> usize {
        self.playhead.load(Ordering::Relaxed)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn samples_remaining(&self) -> usize {
        self.samples.len() - self.playhead()
    }

    pub fn state(&self) -> PlayerStateTag {
        self.state.state_tag()
    }
}

fn scale_index(inrate: u32, outrate: u32, index: usize) -> Option<usize> {
    u64::try_from(index)
        .ok()
        .and_then(|idx| idx.checked_mul(outrate.into()))
        .and_then(|idx| (idx / u64::from(inrate)).try_into().ok())
}

fn resample(inrate: u32, outrate: u32, input_samples: &[f32]) -> Vec<f32> {
    let sinc_len = 256usize;
    let f_cutoff = 1f32 + 1f32 / sinc_len as f32;
    /*
    let f_cutoff = 0.95f32;
    */

    let config = SincInterpolationParameters {
        sinc_len,
        f_cutoff,
        oversampling_factor: 128,
        interpolation: SincInterpolationType::Cubic,
        window: WindowFunction::Blackman,
    };

    let mut interpolator = SincFixedIn::new(
        outrate as f64 / inrate as f64,
        1.0,
        config,
        input_samples.len(),
        1,
    )
    .unwrap();

    interpolator
        .process(
            &[input_samples],
            None,
        )
        .unwrap()[0].clone()
}

fn stream_config(
    device: &cpal::Device,
    inrate: u32,
) -> Result<SupportedStreamConfig, String> {
    let preferred_rate = if inrate % DVD_DIVISOR == 0 {
        DVD_SAMPLE_RATE
    } else {
        CD_SAMPLE_RATE
    };

    let config = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .filter(|cfg| {
            cfg.channels() == 1 && cfg.sample_format() == SampleFormat::F32
        })
        .map(|range| {
            let mut cfg =
                range.try_with_sample_rate(SampleRate(preferred_rate));

            if cfg.is_none() {
                if preferred_rate == DVD_SAMPLE_RATE {
                    cfg =
                        range.try_with_sample_rate(SampleRate(CD_SAMPLE_RATE));
                } else {
                    cfg =
                        range.try_with_sample_rate(SampleRate(DVD_SAMPLE_RATE));
                }
            }

            cfg
        })
        .next()
        .ok_or("Could not find appropriate configuration")?
        .ok_or("Could not acquire stream with requested sample rate")?;

    Ok(config)
}

fn stream_callback<T>(
    samples: Arc<Vec<f32>>,
    playhead: Arc<AtomicUsize>,
    loop_start: Option<usize>,
    in_end: usize,
) -> impl FnMut(&mut [f32], &'_ T) {
    let mut offset = playhead.load(Ordering::Relaxed);

    move |buf: &mut [f32], _: &'_ _| {
        let sample_ct = in_end.saturating_sub(offset);

        if let Some(loop_start) = loop_start {
            let loop_len = in_end - loop_start;

            let wrap = |off: usize| {
                if off > in_end {
                    (off - loop_start) % loop_len + loop_start
                } else {
                    off
                }
            };

            let mut write_start = 0usize;
            let mut write_end;

            loop {
                let write_count =
                    sample_ct.min(buf.len() - write_start).min(in_end - offset);

                write_end = write_start + write_count;
                let read_end = offset + write_count;

                buf[write_start..write_end]
                    .copy_from_slice(&samples[offset..read_end]);

                offset += sample_ct;
                offset = wrap(offset);
                write_start += write_count;

                if write_start >= buf.len() {
                    break;
                }
            }
        } else if offset < in_end {
            let write_count = sample_ct.min(buf.len()).min(samples.len() - offset);

            let read_end = offset + write_count;

            buf[..write_count].copy_from_slice(&samples[offset..read_end]);
            buf[write_count..].fill(f32::EQUILIBRIUM);
            offset += buf.len();
        } else {
            buf.fill(f32::EQUILIBRIUM);
        }

        offset = offset.min(samples.len());
        playhead.store(offset, Ordering::Relaxed);
    }
}

enum PlayerState {
    Stopped,

    // Allow for drop
    #[allow(unused)]
    Playing(Box<dyn StreamTrait>),

    // Allow for drop
    #[allow(unused)]
    PlayingLooped(Box<dyn StreamTrait>),

    Paused(PlaybackState),
}

impl PlayerState {
    pub fn state_tag(&self) -> PlayerStateTag {
        match self {
            PlayerState::Stopped => PlayerStateTag::Stopped,
            PlayerState::Playing(_) => PlayerStateTag::Playing,
            PlayerState::PlayingLooped(_) => PlayerStateTag::PlayingLooped,
            PlayerState::Paused(_) => PlayerStateTag::Paused,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerStateTag {
    Stopped,
    Playing,
    PlayingLooped,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaybackState {
    pub playhead: usize,
    pub looped: bool,
}
