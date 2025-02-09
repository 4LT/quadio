use hound::SampleFormat;
use std::io::{Read, Seek};
use std::num::TryFromIntError;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Metadata {
    pub sample_rate: u32,
    pub sample_count: u32,
    pub loop_start: Option<u32>,
    pub end: Option<u32>,
    pub bits_per_sample: u16,
}

pub struct QWaveReader<R: Read> {
    reader: hound::WavReader<R>,
    loop_start: Option<u32>,
    loop_length: Option<u32>,
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
                let labeled_texts =
                    cuet::extract_labeled_text_from_list(&bytes);
                labeled_texts
                    .first()
                    .filter(|ltxt| ltxt.purpose_id == *b"mark")
                    .map(|ltxt| ltxt.sample_length)
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
        let sample_count = self.reader.duration();

        let end = if let (Some(start), Some(length)) =
            (self.loop_start, self.loop_length)
        {
            start.checked_add(length)
        } else {
            None
        };

        Metadata {
            sample_rate: self.reader.spec().sample_rate,
            sample_count,
            loop_start: self.loop_start,
            end,
            bits_per_sample: self.reader.spec().bits_per_sample,
        }
    }

    pub fn collect_samples(&mut self) -> Result<Vec<i16>, String> {
        let mut error = Option::<String>::None;
        let spec = self.reader.spec();
        let duration = self
            .reader
            .duration()
            .try_into()
            .map_err(|e: TryFromIntError| e.to_string())?;

        if spec.channels != 1 {
            return Err("Too many channels".into());
        }

        if spec.sample_format != SampleFormat::Int {
            return Err("Float samples are unsupported".into());
        }

        let samp_to_i16 = if spec.bits_per_sample == 8 {
            |s| s << 8
        } else if spec.bits_per_sample == 16 {
            |s| s
        } else {
            return Err("Samples must be 8- or 16-bits".into());
        };

        let samples = self
            .reader
            .samples::<i16>()
            .take(duration)
            .map_while(|s| match s {
                Ok(s) => Some(samp_to_i16(s)),
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
