//! EBU R128 measurement and the gain needed to hit a target.

use ebur128::{Channel, EbuR128, Mode};

pub struct Measurement {
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
}

pub struct Meter {
    ebu: EbuR128,
    channels: usize,
}

impl Meter {
    pub fn new(channels: usize, rate: u32) -> Result<Self, ebur128::Error> {
        let mut ebu = EbuR128::new(channels as u32, rate, Mode::I | Mode::TRUE_PEAK)?;
        // A mono file plays on both speakers; measure it that way (+3 dB),
        // matching ffmpeg-normalize's --dual-mono.
        if channels == 1 {
            ebu.set_channel(0, Channel::DualMono)?;
        }
        Ok(Self { ebu, channels })
    }

    pub fn add(&mut self, interleaved: &[f32]) -> Result<(), ebur128::Error> {
        self.ebu.add_frames_f32(interleaved)
    }

    pub fn finish(&self) -> Result<Measurement, ebur128::Error> {
        let integrated_lufs = self.ebu.loudness_global()?;
        let mut peak: f64 = 0.0;
        for ch in 0..self.channels as u32 {
            peak = peak.max(self.ebu.true_peak(ch)?);
        }
        Ok(Measurement { integrated_lufs, true_peak_dbtp: 20.0 * peak.log10() })
    }
}

impl Measurement {
    /// Linear gain that brings the file to `target_lufs` without the true
    /// peak exceeding `ceiling_dbtp`. Silent files (-inf LUFS) get unity gain.
    pub fn gain_to(&self, target_lufs: f64, ceiling_dbtp: f64) -> Gain {
        if !self.integrated_lufs.is_finite() {
            return Gain { db: 0.0, limited: false };
        }
        let wanted = target_lufs - self.integrated_lufs;
        let max = ceiling_dbtp - self.true_peak_dbtp;
        if wanted > max {
            Gain { db: max, limited: true }
        } else {
            Gain { db: wanted, limited: false }
        }
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
