use cuet::{ChunkWriter, LabeledText, CuePoint};
use hound::{WavSpec, WavWriter};
use std::fs::OpenOptions;
use std::io::{BufWriter, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SampleFmt {
    Unsigned8,
    Signed16,
}

pub struct Project {
    samples: Vec<i16>,
    sample_rate: u32,
    sample_loop: Option<Range<u32>>,
    render_format: SampleFmt,
}

impl Project {
    pub fn from_reader<R: Read + Seek>(
        mut reader: crate::QWaveReader<R>,
    ) -> Result<Self, String> {
        let (samples, metadata) =
            { (reader.collect_samples()?, reader.metadata()) };

        let sample_loop = metadata
            .loop_start
            .map(|start| -> Result<_, std::num::TryFromIntError> {
                if let Some(end) = metadata.end {
                    Ok(start..end)
                } else {
                    Ok(start..samples.len().try_into()?)
                }
            })
            .transpose()
            .map_err(|e| e.to_string())?;

        let sample_fmt = if metadata.bits_per_sample == 8 {
            SampleFmt::Unsigned8
        } else if metadata.bits_per_sample == 16 {
            SampleFmt::Signed16
        } else {
            return Err(String::from("beans"));
        };

        Ok(Project {
            samples,
            sample_rate: metadata.sample_rate,
            sample_loop,
            render_format: sample_fmt,
        })
    }

    pub fn set_loop(&mut self, sample_loop: Option<Range<u32>>) {
        self.sample_loop = sample_loop;
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn sample_count(&self) -> u32 {
        self.samples.len().try_into().unwrap()
    }

    pub fn write_to(&self, outpath: &impl AsRef<Path>) -> Result<(), String> {
        let outfile = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(outpath)
            .map_err(|e| e.to_string())?;
        let mut writer = BufWriter::new(outfile);

        let wave_spec = WavSpec {
            channels: 1,
            sample_format: hound::SampleFormat::Int,
            sample_rate: self.sample_rate,
            bits_per_sample: match self.render_format {
                SampleFmt::Unsigned8 => 8,
                SampleFmt::Signed16 => 16,
            },
        };

        {
            let mut wav_writer = WavWriter::new(&mut writer, wave_spec)
                .map_err(|e| e.to_string())?;

            let samples = self.samples.iter().map(match self.render_format {
                SampleFmt::Unsigned8 => |&s| s >> 8,
                SampleFmt::Signed16 => |&s| s,
            });

            for s in samples {
                wav_writer.write_sample(s).map_err(|e| e.to_string())?;
            }

            wav_writer.finalize().map_err(|e| e.to_string())?;
        }

        let mut outfile = writer.into_inner().map_err(|e| e.to_string())?;


        if let Some(sample_loop) = &self.sample_loop {
            outfile
                .seek(SeekFrom::Start(0))
                .map_err(|e| e.to_string())?;

            let mut chunk_writer =
                ChunkWriter::new(outfile).map_err(|e| e.to_string())?;

            let cue = [CuePoint::from_sample_offset(0, sample_loop.start)];
            chunk_writer.append_cue_chunk(&cue).map_err(|e| e.to_string())?;

            if self.samples.len().try_into().map(
                |len: u32| len != sample_loop.end
            ).unwrap_or(true) {
                let length = sample_loop.end.checked_sub(sample_loop.start)
                    .ok_or("Loop ends before it begins")?;

                let labeled_text = [LabeledText::from_cue_length(0, length)];
                chunk_writer.append_label_chunk(&labeled_text).map_err(
                    |e| e.to_string()
                )?;
            }
        }

        Ok(())
    }
}
