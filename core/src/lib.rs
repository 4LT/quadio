use hound::SampleFormat;
use std::io::{Read, Seek};

#[derive(Clone)]
pub struct Metadata {
    pub spec: hound::WavSpec,
    pub duration: u32,
    pub cue: Option<cuet::CuePoint>,
}

pub struct QWaveReader<R: Read> {
    pub reader: hound::WavReader<R>,
    pub cue: Option<cuet::CuePoint>,
}

impl<R: Read + Seek> QWaveReader<R> {
    pub fn new(reader: R) -> Result<Self, String> {
        let mut cursor =
            cuet::WaveCursor::new(reader).map_err(|e| e.to_string())?;

        let cue_bytes = cursor
            .read_next_chunk_body(*b"cue ")
            .map_err(|e| e.to_string())?;

        let cue_points = cue_bytes
            .map(|bytes| cuet::parse_cue_points(&bytes[..]))
            .unwrap_or(Vec::new());

        let reader = hound::WavReader::new(
            cursor.restore_cursor().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;

        Ok(QWaveReader {
            reader,
            cue: if cue_points.is_empty() {
                None
            } else {
                Some(cue_points[0])
            },
        })
    }
}

impl<R: Read> QWaveReader<R> {
    pub fn metadata(&self) -> Metadata {
        Metadata {
            spec: self.reader.spec(),
            duration: self.reader.duration(),
            cue: self.cue,
        }
    }
}

impl<R: Read> QWaveReader<R> {
    pub fn collect_samples(&mut self) -> Result<Vec<f32>, String> {
        let mut error = Option::<String>::None;
        let spec = self.metadata().spec;

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
