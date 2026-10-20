//! The per-file pipeline: measure loudness, then decode again, apply the gain, resample and encode.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::decode::AudioReader;
use crate::encode::{self, OggOpusWriter, RATE};
use crate::loudness::{self, Gain};
use crate::resample::Resampler;
use lofty::ogg::tag::VorbisComments;

pub type Result<T> = encode::Result<T>;

pub struct Settings {
    /// Library root; jobs are paths relative to it.
    pub src: PathBuf,
    /// Mirror root, see [`Settings::output`].
    pub dst: PathBuf,
    pub bitrate_kbps: u32,
    /// `None` leaves the level alone.
    pub normalize: Option<loudness::Target>,
    pub keep_art: bool,
}

impl Settings {
    /// Where the transcode of `job` goes.
    pub fn output(&self, job: &Path) -> PathBuf {
        self.dst.join(job.with_extension("opus"))
    }
}

/// Transcodes `job`, printing warnings to stderr.
pub fn transcode(job: &Path, settings: &Settings) -> Result<()> {
    let src = settings.src.join(job);
    let dst = settings.output(job);
    let mut reader = AudioReader::open(&src)?;
    let tags = encode::read_tags(&src, settings.keep_art).unwrap_or_else(|e| {
        eprintln!("warning: {job:?}: tags not copied: {e}");
        Default::default()
    });
    let linear = match &settings.normalize {
        None => 1.0,
        Some(target) => {
            let gain = measure(&src, target)?;
            if gain.limited {
                eprintln!("warning: {job:?}: gain capped at {:+.1} dB by the true-peak ceiling", gain.db);
            }
            gain.linear()
        }
    };

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write next to the destination, rename when complete, so an interrupted
    // run never leaves a truncated file that looks up to date.
    let tmp = dst.with_extension("opus.part");
    let result = encode_into(&tmp, &mut reader, linear, tags, settings).and_then(|()| Ok(fs::rename(&tmp, &dst)?));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn measure(src: &Path, target: &loudness::Target) -> Result<Gain> {
    let mut reader = AudioReader::open(src)?;
    let mut meter = loudness::meter(reader.channels as usize, reader.rate)?;
    while let Some(chunk) = reader.next_chunk()? {
        meter.add_frames_f32(chunk)?;
    }
    Ok(loudness::gain_to(&meter, target)?)
}

fn encode_into(tmp: &Path, reader: &mut AudioReader, linear: f32, tags: VorbisComments, settings: &Settings) -> Result<()> {
    let (rate, channels) = (reader.rate, reader.channels);
    let out = BufWriter::new(File::create(tmp)?);
    let mut writer = OggOpusWriter::new(out, channels, rate, settings.bitrate_kbps, tags)?;
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
        let settings = Settings {
            src: dir.clone(),
            dst: dir.join("out"),
            bitrate_kbps: 96,
            normalize: Some(loudness::Target { integrated_lufs: -16.0, true_peak_dbtp: -2.0 }),
            keep_art: true,
        };
        let job = Path::new("tone.wav");
        let frames_in = 44_100 * 3 + 123;
        write_wav(&dir.join(job), 44_100, 2, frames_in);
        transcode(job, &settings).unwrap();

        let mut reader = AudioReader::open(&settings.output(job)).unwrap();
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
        let unlimited = loudness::Target { integrated_lufs: -16.0, true_peak_dbtp: f64::INFINITY };
        let off_by = loudness::gain_to(&meter, &unlimited).unwrap().db;
        assert!(off_by.abs() < 0.5, "{off_by} dB off -16 LUFS");
        fs::remove_dir_all(&dir).unwrap();
    }
}
