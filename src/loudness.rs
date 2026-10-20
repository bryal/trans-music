//! EBU R128 measurement and the gain needed to hit a target.

use ebur128::{Channel, EbuR128, Mode};

pub struct Meter(EbuR128);

impl Meter {
    pub fn new(channels: usize, rate: u32) -> Result<Self, ebur128::Error> {
        let mut ebu = EbuR128::new(channels as u32, rate, Mode::I | Mode::TRUE_PEAK)?;
        // A mono file plays on both speakers; measure it that way (+3 dB),
        // matching ffmpeg-normalize's --dual-mono.
        if channels == 1 {
            ebu.set_channel(0, Channel::DualMono)?;
        }
        Ok(Self(ebu))
    }

    pub fn add(&mut self, interleaved: &[f32]) -> Result<(), ebur128::Error> {
        self.0.add_frames_f32(interleaved)
    }

    /// The gain that brings the audio added so far to `target_lufs` without
    /// its true peak exceeding `ceiling_dbtp`. Silence gets unity gain.
    pub fn gain_to(&self, target_lufs: f64, ceiling_dbtp: f64) -> Result<Gain, ebur128::Error> {
        let lufs = self.0.loudness_global()?;
        if !lufs.is_finite() {
            return Ok(Gain { db: 0.0, limited: false });
        }
        let mut peak: f64 = 0.0;
        for ch in 0..self.0.channels() {
            peak = peak.max(self.0.true_peak(ch)?);
        }
        let wanted = target_lufs - lufs;
        let max = ceiling_dbtp - 20.0 * peak.log10();
        Ok(Gain { db: wanted.min(max), limited: wanted > max })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Gain {
    pub db: f64,
    /// True when the gain was reduced to respect the true-peak ceiling.
    pub limited: bool,
}

impl Gain {
    pub fn linear(&self) -> f32 {
        10f32.powf(self.db as f32 / 20.0)
    }
}
