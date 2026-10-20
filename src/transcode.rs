//! The per-file pipeline: measure, then decode again, normalize, resample, encode, tag.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use crate::decode::AudioReader;
use crate::encode::{self, OggOpusWriter, RATE};
use crate::loudness::{Gain, Meter};
use crate::resample::Resampler;
use opusic_c::Channels;

pub type Result<T> = encode::Result<T>;

pub struct Settings {
    pub bitrate_kbps: u32,
    /// `None` disables normalization.
    pub target: Option<Target>,
    pub keep_art: bool,
}

#[derive(Clone, Copy)]
pub struct Target {
    pub loudness_lufs: f64,
    pub true_peak_dbtp: f64,
}

pub struct Report {
    pub gain: Option<Gain>,
    pub warning: Option<String>,
}

pub fn transcode(src: &Path, dst: &Path, settings: &Settings) -> Result<Report> {
    let mut reader = AudioReader::open(src)?;
    let channels = encode::channels(reader.channels)?;
    let gain = settings.target.map(|t| measure(src, t)).transpose()?;
    let linear = gain.map_or(1.0, |g| g.linear());

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write next to the destination, rename when complete, so an interrupted
    // run never leaves a truncated file that looks up to date.
    let tmp = dst.with_extension("opus.part");
    let result = encode_into(&tmp, &mut reader, channels, linear, settings)
        .and_then(|()| encode::copy_tags(src, &tmp, settings.keep_art))
        .and_then(|warning| fs::rename(&tmp, dst).map(|()| warning).map_err(Into::into));
    match result {
        Ok(warning) => Ok(Report { gain, warning }),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn measure(src: &Path, target: Target) -> Result<Gain> {
    let mut reader = AudioReader::open(src)?;
    let mut meter = Meter::new(reader.channels, reader.rate)?;
    while let Some(chunk) = reader.next_chunk()? {
        meter.add(chunk)?;
    }
    Ok(meter.finish()?.gain_to(target.loudness_lufs, target.true_peak_dbtp))
}

fn encode_into(tmp: &Path, reader: &mut AudioReader, channels: Channels, linear: f32, settings: &Settings) -> Result<()> {
    let rate = reader.rate;
    let mut writer = OggOpusWriter::new(BufWriter::new(File::create(tmp)?), channels, rate, settings.bitrate_kbps)?;
    let mut resampler = (rate != RATE).then(|| Resampler::new(rate, RATE, channels as usize)).transpose()?;
    let mut scaled = Vec::new();
    while let Some(chunk) = reader.next_chunk()? {
        scaled.clear();
        scaled.extend(chunk.iter().map(|s| s * linear));
        match &mut resampler {
            Some(r) => writer.push(r.push(&scaled)?)?,
            None => writer.push(&scaled)?,
        }
    }
    if let Some(r) = &mut resampler {
        writer.push(r.flush()?)?;
    }
    writer.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loudness::Meter;

    fn write_wav(path: &Path, rate: u32, channels: u16, frames: usize) {
        let data_len = frames * channels as usize * 2;
        let mut w = Vec::with_capacity(44 + data_len);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        w.extend_from_slice(&(channels * 2).to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data_len as u32).to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            // -20 dBFS tone; silence as the first second to check for drift.
            let v = if t < 1.0 { 0.0 } else { 0.1 * (2.0 * std::f32::consts::PI * 440.0 * t).sin() };
            for _ in 0..channels {
                w.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
            }
        }
        fs::write(path, w).unwrap();
    }

    /// Transcoding a 44.1 kHz tone keeps its length, lands on the target
    /// loudness, and keeps its first second silent.
    #[test]
    fn wav_round_trip() {
        let dir = std::env::temp_dir().join(format!("trans-music-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("tone.wav");
        let dst = dir.join("out").join("tone.opus");
        let frames_in = 44_100 * 3 + 123;
        write_wav(&src, 44_100, 2, frames_in);
        let settings = Settings {
            bitrate_kbps: 96,
            target: Some(Target { loudness_lufs: -16.0, true_peak_dbtp: -2.0 }),
            keep_art: true,
        };
        let report = transcode(&src, &dst, &settings).unwrap();
        assert!(report.gain.unwrap().db > 0.0);
        assert_eq!(report.warning, None);

        let mut reader = AudioReader::open(&dst).unwrap();
        assert_eq!((reader.rate, reader.channels), (RATE, 2));
        let mut meter = Meter::new(2, RATE).unwrap();
        let mut out = Vec::new();
        while let Some(chunk) = reader.next_chunk().unwrap() {
            meter.add(chunk).unwrap();
            out.extend_from_slice(chunk);
        }
        let expected = (frames_in as u64 * RATE as u64).div_ceil(44_100) as usize;
        assert_eq!(out.len() / 2, expected);
        let lead_peak = out[..2 * 47_000].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(lead_peak < 0.01, "leading silence peak {lead_peak}");
        let lufs = meter.finish().unwrap().integrated_lufs;
        assert!((lufs + 16.0).abs() < 0.5, "got {lufs} LUFS");
        fs::remove_dir_all(&dir).unwrap();
    }
}
