//! EBU R128 measurement and the gain needed to hit a target.

use ebur128::{Channel, EbuR128, Mode};

pub fn meter(channels: usize, rate: u32) -> Result<EbuR128, ebur128::Error> {
    let mut ebu = EbuR128::new(channels as u32, rate, Mode::I | Mode::TRUE_PEAK)?;
    // A mono file plays on both speakers; measure it that way (+3 dB),
    // matching ffmpeg-normalize's --dual-mono.
    if channels == 1 {
        ebu.set_channel(0, Channel::DualMono)?;
    }
    Ok(ebu)
}

/// What loudness normalization aims for.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub integrated_lufs: f64,
    /// Ceiling for the true peak; it wins over `integrated_lufs`.
    pub true_peak_dbtp: f64,
}

/// The gain that brings the audio measured by `ebu` to `target`. Silence
/// gets unity gain.
pub fn gain_to(ebu: &EbuR128, target: &Target) -> Result<Gain, ebur128::Error> {
    let lufs = ebu.loudness_global()?;
    if !lufs.is_finite() {
        return Ok(Gain { db: 0.0, limited: false });
    }
    let mut peak: f64 = 0.0;
    for ch in 0..ebu.channels() {
        peak = peak.max(ebu.true_peak(ch)?);
    }
    let wanted = target.integrated_lufs - lufs;
    let max = target.true_peak_dbtp - 20.0 * peak.log10();
    Ok(Gain { db: wanted.min(max), limited: wanted > max })
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
