//! The per-file pipeline: measure, then decode again, normalize, resample, encode, tag.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use crate::decode::AudioReader;
use crate::encode::{self, OggOpusWriter, RATE};
use crate::loudness::{self, Gain};
use crate::resample::Resampler;
use crate::scan::Job;

pub type Result<T> = encode::Result<T>;

pub struct Settings {
    pub bitrate_kbps: u32,
    pub normalize: bool,
    pub loudness_lufs: f64,
    pub true_peak_dbtp: f64,
    pub keep_art: bool,
}

/// Transcodes one file, printing warnings to stderr.
pub fn transcode(job: &Job, settings: &Settings) -> Result<()> {
    let (src, dst) = (&job.src, &job.dst);
    let mut reader = AudioReader::open(src)?;
    let mut linear = 1.0;
    if settings.normalize {
        let gain = measure(src, settings)?;
        if gain.limited {
            eprintln!("warning: {}: gain capped at {:+.1} dB by the true-peak ceiling", job.rel.display(), gain.db);
        }
        linear = gain.linear();
    }

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write next to the destination, rename when complete, so an interrupted
    // run never leaves a truncated file that looks up to date.
    let tmp = dst.with_extension("opus.part");
    let result = encode_into(&tmp, &mut reader, linear, settings)
        .and_then(|()| encode::copy_tags(src, &tmp, settings.keep_art))
        .and_then(|warning| {
            if let Some(w) = warning {
                eprintln!("warning: {}: {w}", job.rel.display());
            }
            Ok(fs::rename(&tmp, dst)?)
        });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn measure(src: &Path, settings: &Settings) -> Result<Gain> {
    let mut reader = AudioReader::open(src)?;
    let mut meter = loudness::meter(reader.channels as usize, reader.rate)?;
    while let Some(chunk) = reader.next_chunk()? {
        meter.add_frames_f32(chunk)?;
    }
    Ok(loudness::gain_to(&meter, settings.loudness_lufs, settings.true_peak_dbtp)?)
}

fn encode_into(tmp: &Path, reader: &mut AudioReader, linear: f32, settings: &Settings) -> Result<()> {
    let (rate, channels) = (reader.rate, reader.channels);
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
        let job = Job { rel: "tone.wav".into(), src: dir.join("tone.wav"), dst: dir.join("out").join("tone.opus") };
        let frames_in = 44_100 * 3 + 123;
        write_wav(&job.src, 44_100, 2, frames_in);
        let settings =
            Settings { bitrate_kbps: 96, normalize: true, loudness_lufs: -16.0, true_peak_dbtp: -2.0, keep_art: true };
        transcode(&job, &settings).unwrap();

        let mut reader = AudioReader::open(&job.dst).unwrap();
        assert_eq!((reader.rate, reader.channels as usize), (RATE, 2));
        let mut meter = loudness::meter(2, RATE).unwrap();
        let mut out = Vec::new();
        while let Some(chunk) = reader.next_chunk().unwrap() {
            meter.add_frames_f32(chunk).unwrap();
            out.extend_from_slice(chunk);
        }
        let expected = (frames_in as u64 * RATE as u64).div_ceil(44_100) as usize;
        assert_eq!(out.len() / 2, expected);
        let lead_peak = out[..2 * 47_000].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(lead_peak < 0.01, "leading silence peak {lead_peak}");
        let off_by = loudness::gain_to(&meter, -16.0, f64::INFINITY).unwrap().db;
        assert!(off_by.abs() < 0.5, "{off_by} dB off -16 LUFS");
        fs::remove_dir_all(&dir).unwrap();
    }
}
