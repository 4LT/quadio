use hound::SampleFormat;
use std::io::{Read, Seek};
use std::num::TryFromIntError;

#[derive(Clone)]
pub struct Metadata {
    pub spec: hound::WavSpec,
    pub duration: u32,
    pub loop_start: Option<u32>,
}

pub struct QWaveReader<R: Read> {
    pub reader: hound::WavReader<R>,
    pub loop_start: Option<u32>,
    pub loop_length: Option<u32>,
}

impl<R: Read + Seek> QWaveReader<R> {
    pub fn new(reader: R) -> Result<Self, String> {
        let mut chunk_reader =
            cuet::ChunkReader::new(reader).map_err(|e| e.to_string())?;

        let cue_chunk = chunk_reader
            .read_next_chunk(Some(*b"cue "))
            .map_err(|e| e.to_string())?;

        let loop_start = cue_chunk.and_then(|(_, bytes)| {
            let pts = cuet::parse_cue_points(&bytes[..]);

            if pts.is_empty() {
                None
            } else {
                Some(pts[0].sample_offset)
            }
        });

        let loop_length = if loop_start.is_some() {
            let list_chunk = chunk_reader
                .read_next_chunk(Some(*b"LIST"))
                .map_err(|e| e.to_string())?;

            list_chunk.and_then(|(_, bytes)| {
                let labeled_texts = cuet::extract_labeled_text_from_list(&bytes); 
                labeled_texts.iter().next().map(|ltxt| ltxt.sample_length)
            })
        } else {
            None
        };

        let reader = hound::WavReader::new(
            chunk_reader.restore_cursor().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;

        Ok(QWaveReader {
            reader,
            loop_start,
            loop_length,
        })
    }
}

impl<R: Read> QWaveReader<R> {
    pub fn metadata(&self) -> Metadata {
        let wave_duration = self.reader.duration();

        let duration = if let (Some(start), Some(length)) =
            (self.loop_start, self.loop_length)
        {
            if let Some(d) = start.checked_add(length) {
                d
            } else {
                wave_duration
            }
        } else {
            wave_duration
        };

        Metadata {
            spec: self.reader.spec(),
            duration,
            loop_start: self.loop_start,
        }
    }

    pub fn collect_samples(&mut self) -> Result<Vec<f32>, String> {
        let mut error = Option::<String>::None;
        let spec = self.metadata().spec;
        let duration = self.metadata().duration
            .try_into()
            .map_err(|e: TryFromIntError| e.to_string())?;

        if spec.channels != 1 {
            return Err("Too many channels".into());
        }

        if spec.sample_format != SampleFormat::Int {
            return Err("Float samples are unsupported".into());
        }

        fn samp8_to_float(s: i16) -> f32 {
            (s - i16::from(u8::MAX / 2)) as f32
                / (i16::from(u8::MAX) * 2) as f32
        }

        fn samp16_to_float(s: i16) -> f32 {
            s as f32 / i16::MAX as f32
        }

        let samp_to_float = if spec.bits_per_sample == 8 {
            samp8_to_float
        } else if spec.bits_per_sample == 16 {
            samp16_to_float
        } else {
            return Err("Samples must be 8- or 16-bits".into());
        };

        let samples = self
            .reader
            .samples::<i16>()
            .take(duration)
            .map_while(|s| match s {
                Ok(s) => Some(samp_to_float(s)),
                Err(e) => {
                    error = Some(e.to_string());
                    None
                }
            })
            .collect();

        if let Some(e) = error {
            Err(e)
        } else {
            Ok(samples)
        }
    }
}
