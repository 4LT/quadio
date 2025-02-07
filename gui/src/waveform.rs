use std::ops::DerefMut;
use std::rc::Rc;
use std::cell::RefCell;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Theme {
    pub background: u32,
    pub in_range: u32,
    pub rms: u32,
}

pub trait MutSlice {
    type Output<'a>: DerefMut<Target=[u8]> where Self: 'a;
    
    fn mut_slice<'a>(&'a mut self) -> Self::Output<'a>;
}

pub struct Waveform<Img: MutSlice> {
    samples: Vec<i16>,
    bin_mips: Vec<Vec<Bin>>,
    zoom_cutoff: f64,
    image: Rc<RefCell<Img>>,
    buffer_width: i32,
    buffer_height: i32,
    buffer_stride: i32,
    theme: Theme,
}

impl<Img: MutSlice> Waveform<Img> {
    pub fn new<'n>(
        samples: Vec<i16>,
        zoom_cutoff: f64,
        buffer_width: i32,
        buffer_height: i32,
        buffer_stride: i32,
        theme: Theme,
        to_image: impl Fn(Vec<u8>) -> Img + 'n,
    ) -> Self {
        if !zoom_cutoff.is_finite()
            || zoom_cutoff <= 0.0
            || zoom_cutoff > 1.0
        {
            panic!("Target zoom cutoff outside range (0.0, 1.0]");
        }

        let max_bin_count = (samples.len() as f64 * zoom_cutoff) as usize;

        if max_bin_count == 0 {
            panic!("floor(Sample count * Target zoom cutoff) cannot be zero");
        }

        let mut max_bin_count = max_bin_count.next_power_of_two();

        if max_bin_count > samples.len() {
            max_bin_count = max_bin_count >> 1;
        }

        let zoom_cutoff = max_bin_count as f64 / samples.len() as f64;

        let mut bin_count = max_bin_count;
        let mut bin_mips = Vec::<Vec<Bin>>::new();

        while bin_count > 0 {
            bin_mips.push(Bin::bin_samples(&samples[..], bin_count));
            bin_count = bin_count >> 1;
        }

        let pixbuf_sz = usize::try_from(buffer_stride).unwrap() *
            usize::try_from(buffer_height).unwrap();

        let pixbuf = vec![0u8; pixbuf_sz];
        let image = Rc::new(RefCell::new(to_image(pixbuf)));

        Waveform {
            samples,
            bin_mips,
            zoom_cutoff,
            image,
            buffer_width,
            buffer_height,
            buffer_stride,
            theme,
        }
    }

    pub fn render(&mut self, window: &Window) -> DrawInfo<Img> {
        // Reminder:
        // * zooms are in pixels/sample

        if window.zoom <= 0.0 {
            panic!("Zoom must be > 0");
        } else if window.zoom > self.zoom_cutoff {
            // todo: stroke path
            DrawInfo::Blank
        } else {
            let col_count = window.width_px.try_into().unwrap();
            let row_count = self.buffer_height.try_into().unwrap();

            // left side of window in pixels and samples
            let left_px = -window.offset_px;
            let left_sample = left_px as f64 / window.zoom;
            // padding between left of window and waveform start
            let left_pad = window.offset_px.max(0);

            if left_pad >= window.width_px {
                return DrawInfo::Blank;
            }

            let left_pad = left_pad as usize;
            let right_px = window.offset_px + window.width_px;
            let right_sample = right_px as f64 / window.zoom;
            let zoom_ratio = self.zoom_cutoff/window.zoom;
            let zoom_ratio_log = zoom_ratio.log2();
            let mip_level = zoom_ratio_log as usize;

            if mip_level >= self.bin_mips.len() {
                return DrawInfo::Blank;
            }

            let scale = 1.0/(zoom_ratio_log - zoom_ratio_log.floor() + 1.0);
            let mip = &self.bin_mips[mip_level];
            let bins_per_sample = mip.len() as f64 / self.samples.len() as f64;

            let left_bin = ((left_sample * bins_per_sample).floor() as isize)
                .max(0) as usize;

            let right_bin = ((right_sample * bins_per_sample).floor() as isize)
                .min(mip.len().try_into().unwrap());

            if right_bin < 0 {
                return DrawInfo::Blank;
            }

            let right_bin = right_bin as usize;
            let mip_slice = &mip[left_bin..right_bin];

            let bins = rebin_ranges(
                    mip_slice.len(),
                    ((mip_slice.len() as f64 * scale) as usize)
                        .min(window.width_px as usize)
                )
                    .map(|range| Bin::from_others(&mip_slice[range]))
                    .collect::<Vec<_>>();

            println!("{} + {} <= {}", bins.len(), left_pad, window.width_px);
            assert!(bins.len() + left_pad <= window.width_px as usize);

            let stride = self.buffer_stride as usize;

            let sample_to_row = {
                let row_max = (row_count - 1) as f64;

                move |sample: f64| {
                    ((1.0 - sample)/2.0 * row_max) as usize
                }
            };

            {
                let color_coord = {
                    let image = Rc::clone(&self.image);

                    move |row, col, color: u32| {
                        let mut borrowed_image = image.borrow_mut();
                        let mut pixbuf = borrowed_image.mut_slice();
                        let idx = row * stride + col*4;
                        pixbuf[idx..idx+4].copy_from_slice(
                            &color.to_ne_bytes()
                        );
                    }
                };

                for row in 0..row_count {
                    for col in 0..left_pad {
                        color_coord(row, col, self.theme.background);
                    }
                }

                let col_stop = bins.len() + left_pad;
                println!("col_stop {}", col_stop);

                for (col, bin) in std::iter::zip(left_pad..col_stop, bins) {
                    let start = 0;
                    let stop_max = sample_to_row(bin.max());
                    let stop_min = sample_to_row(bin.min());
                    let stop_pos_rms = sample_to_row(bin.rms()).max(stop_max);
                    let stop_neg_rms = sample_to_row(-bin.rms()).min(stop_min);

                    for row in start..stop_max {
                        color_coord(row, col, self.theme.background);
                    }

                    for row in stop_max..stop_pos_rms {
                        color_coord(row, col, self.theme.in_range);
                    }

                    for row in stop_pos_rms..stop_neg_rms {
                        color_coord(row, col, self.theme.rms);
                    }

                    for row in stop_neg_rms..stop_min {
                        color_coord(row, col, self.theme.in_range);
                    }

                    for row in stop_min..row_count {
                        color_coord(row, col, self.theme.background);
                    }
                }

                for row in 0..row_count {
                    for col in col_stop..col_count {
                        color_coord(row, col, self.theme.background);
                    }
                }

                println!("RENDERED 0..{}..{}..{}",
                    left_pad,
                    col_stop,
                    col_count,
                );
            }

            DrawInfo::Image(Rc::clone(&self.image))
        }
    }
}

pub enum DrawInfo<Img> {
    Blank,
    Samples(Vec<(f64, f64)>),
    Image(Rc<RefCell<Img>>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub offset_px: i32,

    // zoom as ratio pixels/sample
    pub zoom: f64,
    pub width_px: i32,
}

// Adapted from Besenham's line-drawing algorithm
fn rebin_ranges(old_size: usize, new_size: usize)
    -> impl Iterator<Item=std::ops::Range<usize>>
{
    if new_size > old_size {
        panic!("New size must be less than old size");
    }

    if old_size == 0 {
        panic!("Old size must be greater than zero");
    }

    let small_slice_size = old_size / new_size;
    let large_slice_size = small_slice_size + 1;
    let large_bin_count = old_size % new_size;
    let small_bin_count = new_size - large_bin_count;
    let old_size_64 = u64::try_from(old_size).unwrap();
    let new_size_64 = u64::try_from(new_size).unwrap();
    let small_incr = u64::try_from(small_slice_size).unwrap() * new_size_64;
    let large_incr = u64::try_from(large_slice_size).unwrap() * new_size_64;
    let exact_max = old_size_64 * new_size_64;

    let mut exact_accum = 0;
    let mut start = 0;
    let mut end;
    let mut approx_accum;

    if small_bin_count > large_bin_count {
        approx_accum = small_incr;
        end = small_slice_size;
    } else {
        approx_accum = large_incr;
        end = large_slice_size;
    }

    std::iter::from_fn(move || {
        if exact_accum >= exact_max {
            return None
        }

        let range = start..end;

        start = end;
        exact_accum+= old_size_64;

        if approx_accum >= exact_accum {
            approx_accum+= small_incr;
            end+= small_slice_size;
        } else {
            approx_accum+= large_incr;
            end+= large_slice_size;
        }

        Some(range)
    })
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Bin {
    min: f64,
    max: f64,
    mean_square: f64,
    sample_count: usize,
}

impl Bin {
    pub fn bin_samples(samples: &[i16], bin_count: usize) -> Vec<Bin> {
        rebin_ranges(samples.len(), bin_count).map(
            |range| Bin::from_samples(&samples[range])
        ).collect()
    }

    pub fn from_samples(samples: &[i16]) -> Self {
        if samples.len() < 1 {
            panic!("Too few samples, must have at least 1");
        }

        let mut min = 1f64;
        let mut max = -min;
        let mut sum_squares = 0f64;

        for sample in samples {
            let sample = f64::from(*sample) / -f64::from(i16::MIN);
            min = min.min(sample);
            max = max.max(sample);
            sum_squares+= sample * sample;
        }

        Bin {
            min,
            max,
            mean_square: sum_squares / samples.len() as f64,
            sample_count: samples.len(),
        }
    }

    pub fn from_others<'a>(bins: impl IntoIterator<Item=&'a Bin>) -> Self {
        let mut iter = bins.into_iter().peekable();

        if iter.peek() == None {
            panic!("Too few bins, must have at least 1");
        }

        let mut min = 1f64;
        let mut max = -min;
        let mut weighted_sum = 0f64;
        let mut sample_count = 0usize;

        for bin in iter {
            min = min.min(bin.min);
            max = max.max(bin.max);
            sample_count+= bin.sample_count;
            weighted_sum+= bin.mean_square * bin.sample_count as f64;
        }

        Bin {
            min,
            max,
            mean_square: weighted_sum / sample_count as f64,
            sample_count,
        }
    }

    pub fn rms(&self) -> f64 {
        self.mean_square.sqrt()
    }

    pub fn min(&self) -> f64 {
        self.min
    }

    pub fn max(&self) -> f64 {
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_25_samples_in_7_bins() {
        let samples = [0i16; 25];
        let bins = Bin::bin_samples(&samples, 7);
        assert_eq!(bins.len(), 7);
        assert_eq!(bins[0].sample_count, 4);
        assert_eq!(bins[1].sample_count, 3);
        assert_eq!(bins[2].sample_count, 4);
        assert_eq!(bins[3].sample_count, 3);
        assert_eq!(bins[4].sample_count, 4);
        assert_eq!(bins[5].sample_count, 3);
        assert_eq!(bins[6].sample_count, 4);
    }

    #[test]
    fn bin_7_samples_in_3_bins() {
        let samples = [0i16; 7];
        let bins = Bin::bin_samples(&samples, 3);
        assert_eq!(bins.len(), 3);
        assert_eq!(bins[0].sample_count, 2);
        assert_eq!(bins[1].sample_count, 3);
        assert_eq!(bins[2].sample_count, 2);
    }

    #[test]
    fn bin_4_samples_in_4_bins() {
        let samples = [0i16; 4];
        let bins = Bin::bin_samples(&samples, 4);
        assert_eq!(bins.len(), 4);
        assert_eq!(bins[0].sample_count, 1);
        assert_eq!(bins[1].sample_count, 1);
        assert_eq!(bins[2].sample_count, 1);
        assert_eq!(bins[3].sample_count, 1);
    }

    #[test]
    fn bin_1017_samples_in_97_bins() {
        let samples = [0i16; 1017];
        let bins = Bin::bin_samples(&samples, 97);
        assert_eq!(bins.len(), 97);
        assert_eq!(
            bins.into_iter().map(|b| b.sample_count).sum::<usize>(),
            1017
        );
    }

    #[test]
    fn waveform_1_sample_zoom_cutoff_1() {
        let samples = vec![0i16];
        let zoom_cutoff = 1.0;

        let waveform = Waveform::new(
            samples,
            1,
            zoom_cutoff,
            |i| { i },
            |img, _, _| { img.len() },
        );
        
        assert_eq!(waveform.mips.len(), 1);
        assert_eq!(waveform.mips[0], 4);
        assert_eq!(
            waveform.draw_info(&Window {
                offset: 0.0,
                zoom: 0.3,
                width_px: 137,
            }),
            DrawInfo::Image(&4, 0.0, 0.3)
        );
    }

    #[test]
    fn waveform_3033_samples_zoom_cutoff_1() {
        let samples = vec![0i16; 3033];
        let zoom_cutoff = 1.0;

        let waveform = Waveform::new(
            samples,
            1,
            zoom_cutoff,
            |i| { i },
            |img, _width, _stride| { img.len() },
        );
        
        // floor(log2(3033)) + 1 == 12
        assert_eq!(waveform.mips.len(), 12);
        // 2^(12 - 1) * 4 = 8192
        assert_eq!(waveform.mips[0], 8192);

        assert_eq!(
            waveform.draw_info(&Window {
                offset: 2.0,
                zoom: 1.0,
                width_px: 137,
            }),
            DrawInfo::Image(&8192, 2.0, 1.0)
        );

        let DrawInfo::Image(sz, _, scale) = waveform.draw_info(&Window {
            offset: 0.0,
            zoom: 0.3,
            width_px: 12,
        }) else {
            panic!("Unexpected DrawInfo variant");
        };

        assert_eq!(scale, 0.3 * 4.0);
        assert_eq!(sz, &2048);

        let DrawInfo::Image(sz, _, scale) = waveform.draw_info(&Window {
            offset: 0.0,
            zoom: 0.2,
            width_px: 12,
        }) else {
            panic!("Unexpected DrawInfo variant");
        };

        assert_eq!(sz, &1024);
        assert_eq!(scale, 0.2 * 8.0);
    }

    #[test]
    fn waveform_137_samples_zoom_cutoff_0_3() {
        let samples = vec![0i16; 137];
        let zoom_cutoff = 0.3;

        let waveform = Waveform::new(
            samples,
            1,
            zoom_cutoff,
            |i| { i },
            |img, _width, _stride| { img.len() },
        );

        // floor(log2(137)) + 1 == 8
        // floor(log2(0.3)) == -2
        // 8 - 2 == 6
        assert_eq!(waveform.mips.len(), 6);
        // 2^(6 - 1) * 4 == 128
        assert_eq!(waveform.mips[0], 128);
        assert_eq!(waveform.mips[1], 64);
        assert_eq!(waveform.mips[2], 32);
        assert_eq!(waveform.mips[3], 16);
        assert_eq!(waveform.mips[4], 8);
        assert_eq!(waveform.mips[5], 4);

        let DrawInfo::Image(sz, _, scale) = waveform.draw_info(&Window {
            offset: 0.0,
            zoom: 0.1,
            width_px: 12,
        }) else {
            panic!("Unexpected DrawInfo variant");
        };

        assert_eq!(sz, &32);
        assert_eq!(scale, 0.1 * 16.0);
    }
}
