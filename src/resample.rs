//! Fixed-ratio resampling to Opus' 48 kHz, streaming over interleaved f32.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, ResampleError, Resampler as _, ResamplerConstructionError};

const CHUNK: usize = 1024;

pub struct Resampler {
    fft: Fft<f32>,
    channels: usize,
    in_rate: u64,
    out_rate: u64,
    /// Interleaved input not yet consumed (less than one chunk).
    pending: Vec<f32>,
    out: Vec<f32>,
    /// Output frames still to drop; starts at the filter delay.
    skip: usize,
    frames_in: u64,
    frames_out: u64,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Result<Self, ResamplerConstructionError> {
        let fft = Fft::<f32>::new(in_rate as usize, out_rate as usize, CHUNK, channels, FixedSync::Input)?;
        let skip = fft.output_delay();
        Ok(Self {
            fft,
            channels,
            in_rate: in_rate.into(),
            out_rate: out_rate.into(),
            pending: Vec::new(),
            out: Vec::new(),
            skip,
            frames_in: 0,
            frames_out: 0,
        })
    }

    /// Feed interleaved input; returns whatever output is ready.
    pub fn push(&mut self, input: &[f32]) -> Result<&[f32], ResampleError> {
        debug_assert_eq!(input.len() % self.channels, 0, "input must hold whole frames");
        self.frames_in += (input.len() / self.channels) as u64;
        self.pending.extend_from_slice(input);
        self.out.clear();
        let need = self.fft.input_frames_next() * self.channels;
        let mut pending = std::mem::take(&mut self.pending);
        let mut consumed = 0;
        while pending.len() - consumed >= need {
            self.run(&pending[consumed..consumed + need], None)?;
            consumed += need;
        }
        pending.drain(..consumed);
        self.pending = pending;
        Ok(&self.out)
    }

    /// Drain the filter; the total output is exactly the input length scaled.
    pub fn flush(&mut self) -> Result<&[f32], ResampleError> {
        self.out.clear();
        let expected = (self.frames_in * self.out_rate).div_ceil(self.in_rate);
        let need = self.fft.input_frames_next() * self.channels;
        let partial = self.pending.len() / self.channels;
        self.pending.resize(need, 0.0);
        let mut pending = std::mem::take(&mut self.pending);
        self.run(&pending, Some(partial))?;
        pending.fill(0.0);
        while self.frames_out < expected {
            self.run(&pending, Some(0))?;
        }
        let extra = (self.frames_out - expected) as usize * self.channels;
        self.out.truncate(self.out.len() - extra);
        self.frames_out = expected;
        Ok(&self.out)
    }

    fn run(&mut self, chunk: &[f32], partial: Option<usize>) -> Result<(), ResampleError> {
        let frames = chunk.len() / self.channels;
        let input = InterleavedSlice::new(chunk, self.channels, frames).expect("chunk sized by channels");
        let max = self.fft.output_frames_max();
        let start = self.out.len();
        self.out.resize(start + max * self.channels, 0.0);
        let mut output = InterleavedSlice::new_mut(&mut self.out[start..], self.channels, max).expect("sized above");
        let indexing = partial.map(|n| Indexing::new().partial_len(n));
        let (_, produced) = self.fft.process_into_buffer(&input, &mut output, indexing.as_ref())?;
        self.out.truncate(start + produced * self.channels);
        let dropped = self.skip.min(produced);
        self.out.drain(start..start + dropped * self.channels);
        self.skip -= dropped;
        self.frames_out += (produced - dropped) as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 kHz stereo sine survives 44.1 -> 48 kHz with the right length,
    /// amplitude and no leading silence.
    #[test]
    fn sine_round_trip() {
        let (in_rate, out_rate, ch) = (44_100u32, 48_000u32, 2);
        let frames_in = 44_100 * 3 + 517;
        let input: Vec<f32> = (0..frames_in)
            .flat_map(|i| {
                let t = i as f32 / in_rate as f32;
                let v = (2.0 * std::f32::consts::PI * 1000.0 * t).sin();
                [v, 0.5 * v]
            })
            .collect();
        let mut rs = Resampler::new(in_rate, out_rate, ch).unwrap();
        let mut output = Vec::new();
        for piece in input.chunks(777 * ch) {
            output.extend_from_slice(rs.push(piece).unwrap());
        }
        output.extend_from_slice(rs.flush().unwrap());
        let frames_out = output.len() / ch;
        let expected = (frames_in as u64 * out_rate as u64).div_ceil(in_rate as u64) as usize;
        assert_eq!(frames_out, expected);
        for (i, frame) in output.chunks(ch).enumerate().skip(200).take(frames_out - 400) {
            let t = i as f32 / out_rate as f32;
            let v = (2.0 * std::f32::consts::PI * 1000.0 * t).sin();
            assert!((frame[0] - v).abs() < 0.02, "frame {i}: {} vs {v}", frame[0]);
            assert!((frame[1] - 0.5 * v).abs() < 0.02);
        }
        let peak = output.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak < 1.05, "peak {peak}");
    }
}
