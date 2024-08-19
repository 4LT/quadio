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

#[derive(Debug)]
pub struct Player {
    samples: Arc<Vec<f32>>,
    playback_rate: u32,
    loop_start: usize,
    end: usize,
    state: PlayerState,
    playhead: Arc<AtomicUsize>,
    input_rate: u32,
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
        let playback_rate = stream_config.sample_rate().0;

        let mut playback_samples =
            resample(config.sample_rate, playback_rate, &config.samples);

        let end = scale_index(config.sample_rate, playback_rate, end)
            .ok_or("Scaled end too large")?
            .min(playback_samples.len());

        playback_samples.truncate(end);

        let loop_start =
            scale_index(config.sample_rate, playback_rate, loop_start)
                .ok_or("Scaled loop start too large")
                .and_then(|start| {
                    if start < end {
                        Ok(start)
                    } else {
                        Err("Loop start is AT or AFTER end")
                    }
                })?;

        Ok(Player {
            samples: Arc::new(playback_samples),
            playback_rate,
            loop_start,
            end,
            state: PlayerState::Stopped,
            playhead: Arc::new(AtomicUsize::new(0)),
            input_rate: config.sample_rate,
        })
    }

    pub fn play(
        &mut self,
        play_from: usize,
        looped: bool,
    ) -> Result<(), String> {
        let play_from =
            scale_index(self.input_rate, self.playback_rate, play_from)
                .ok_or("Bad playhead position")?;

        self.play_from_playback_position(play_from, looped)
    }

    fn play_from_playback_position(
        &mut self,
        play_from: usize,
        looped: bool,
    ) -> Result<(), String> {
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
        let stream_config = stream_config(&device, self.playback_rate)?;

        if stream_config.sample_rate().0 != self.playback_rate {
            return Err(format!(
                "Failed to acquire stream config @ {}Hz",
                self.playback_rate
            ));
        }

        let loop_start = if looped { Some(self.loop_start) } else { None };
        let channels = stream_config.channels();

        let stream = Box::new(
            device
                .build_output_stream(
                    &stream_config.into(),
                    stream_callback(
                        Arc::clone(&self.samples),
                        Arc::clone(&self.playhead),
                        loop_start,
                        self.end,
                        channels,
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
            PlayerState::Stopped => self.play(0, false)?,
            PlayerState::Paused(PlaybackState { playhead, looped }) => {
                self.play_from_playback_position(playhead, looped)?;
            }
        };

        Ok(())
    }

    pub fn playhead(&self) -> usize {
        let playback_position = self.playhead.load(Ordering::Relaxed);
        scale_index(self.playback_rate, self.input_rate, playback_position)
            .unwrap()
    }

    pub fn playback_rate(&self) -> u32 {
        self.playback_rate
    }

    pub fn samples_remaining(&self) -> usize {
        let playback_position = self.playhead.load(Ordering::Relaxed);
        let playback_samples =
            self.samples.len().saturating_sub(playback_position);
        scale_index(self.playback_rate, self.input_rate, playback_samples)
            .unwrap()
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

    interpolator.process(&[input_samples], None).unwrap()[0].clone()
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

    let mut configs = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .filter(|cfg| cfg.sample_format() == SampleFormat::F32)
        .collect::<Vec<_>>();

    let configs_1_ch = configs
        .iter()
        .filter(|cfg| cfg.channels() == 1)
        .copied()
        .collect::<Vec<_>>();
    let configs_2_ch = configs
        .iter()
        .filter(|cfg| cfg.channels() == 2)
        .copied()
        .collect::<Vec<_>>();

    configs = if !configs_1_ch.is_empty() {
        configs_1_ch
    } else if !configs_2_ch.is_empty() {
        configs_2_ch
    } else {
        configs
    };

    let config = configs
        .iter()
        .flat_map(|range| {
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
        .ok_or("Could not find appropriate stream configuration")?;

    Ok(config)
}

fn stream_callback<T>(
    samples: Arc<Vec<f32>>,
    playhead: Arc<AtomicUsize>,
    loop_start: Option<usize>,
    in_end: usize,
    channels: u16,
) -> impl FnMut(&mut [f32], &'_ T) {
    let mut offset = playhead.load(Ordering::Relaxed);
    let channels = usize::from(channels);

    move |buf: &mut [f32], _: &'_ _| {
        let sub_buf_len = buf.len() / channels;

        if let Some(loop_start) = loop_start {
            let loop_len = in_end - loop_start;

            let wrap = |off: usize| {
                if off >= in_end {
                    (off - loop_start) % loop_len + loop_start
                } else {
                    off
                }
            };

            let mut write_start = 0usize;

            loop {
                let write_count = (sub_buf_len - write_start)
                    .min(in_end.saturating_sub(offset));

                let write_end = write_start + write_count;
                let read_end = offset + write_count;

                buf[write_start..write_end]
                    .copy_from_slice(&samples[offset..read_end]);

                offset += write_count;
                offset = wrap(offset);
                write_start = write_end;

                if write_start >= sub_buf_len {
                    assert_eq!(write_end, sub_buf_len);
                    break;
                }
            }
        } else if offset < in_end {
            let sample_ct = in_end.saturating_sub(offset);
            let write_count =
                sample_ct.min(sub_buf_len).min(samples.len() - offset);

            let read_end = offset + write_count;

            buf[..write_count].copy_from_slice(&samples[offset..read_end]);
            buf[write_count..sub_buf_len].fill(f32::EQUILIBRIUM);
            offset += sub_buf_len;
        } else {
            buf[..sub_buf_len].fill(f32::EQUILIBRIUM);
        }

        // extend buffer by channel count
        if channels > 1 {
            let mut src_idx = sub_buf_len;
            let mut dst_idx = buf.len();
            while src_idx > 0 {
                src_idx -= 1;
                dst_idx -= channels;

                for ch in 0..channels {
                    buf[dst_idx + ch] = buf[src_idx];
                }
            }
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

impl std::fmt::Debug for PlayerState {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> Result<(), std::fmt::Error> {
        match self {
            PlayerState::Stopped => write!(formatter, "PlayerState::Stopped")?,
            PlayerState::Playing(_) => {
                write!(formatter, "PlayerState::Playing(<stream>)")?
            }
            PlayerState::PlayingLooped(_) => {
                write!(formatter, "PlayerState::PlayingLooped(<stream>)")?
            }
            PlayerState::Paused(state) => {
                write!(formatter, "PlayerState::Paused({:?})", state)?
            }
        };

        Ok(())
    }
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
