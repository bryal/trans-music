//! Opus encoding into an Ogg container (RFC 7845), plus tag copying.

use std::fs::File;
use std::hash::BuildHasher;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::ogg::tag::VorbisComments;
use lofty::ogg::{OggPictureStorage, OpusFile, VorbisFile};
use lofty::probe::Probe;
use lofty::tag::{Tag, TagExt, TagType};
use ogg::{PacketWriteEndInfo, PacketWriter};
use rusty_opus::{Application, OpusEncoder, SignalType};

pub const RATE: u32 = 48_000;
/// 20 ms, the frame size libopus-style encoders are tuned for.
pub const FRAME: usize = 960;
/// rusty-opus' encoder delay at 48 kHz (the CELT overlap; it has no extra
/// delay compensation like libopus), verified by the test below.
pub const PRE_SKIP: u16 = 120;
const MAX_PACKET: usize = 1276;
const VENDOR: &str = concat!("trans-music ", env!("CARGO_PKG_VERSION"));

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn new_encoder(channels: usize, bitrate_kbps: u32) -> Result<OpusEncoder> {
    let mut enc = OpusEncoder::new(RATE as i32, channels, Application::Audio)?;
    enc.bitrate_bps = (bitrate_kbps * 1000) as i32;
    enc.complexity = 10;
    enc.signal_type = Some(SignalType::Music);
    Ok(enc)
}

/// Streams 48 kHz interleaved f32 into an Ogg Opus file.
pub struct OggOpusWriter<W: Write> {
    enc: OpusEncoder,
    ogg: PacketWriter<'static, W>,
    serial: u32,
    channels: usize,
    pending: Vec<f32>,
    packet: Vec<u8>,
    /// Real (unpadded) frames fed so far.
    frames: u64,
    /// Frames handed to the encoder so far, padding included.
    encoded: u64,
}

impl<W: Write> OggOpusWriter<W> {
    pub fn new(out: W, channels: usize, input_rate: u32, bitrate_kbps: u32) -> Result<Self> {
        let serial = std::hash::RandomState::new().hash_one(0u8) as u32;
        let mut ogg = PacketWriter::new(out);
        ogg.write_packet(opus_head(channels as u8, input_rate), serial, PacketWriteEndInfo::EndPage, 0)?;
        ogg.write_packet(opus_tags(), serial, PacketWriteEndInfo::EndPage, 0)?;
        Ok(Self {
            enc: new_encoder(channels, bitrate_kbps)?,
            ogg,
            serial,
            channels,
            pending: Vec::new(),
            packet: vec![0; MAX_PACKET],
            frames: 0,
            encoded: 0,
        })
    }

    pub fn push(&mut self, interleaved: &[f32]) -> Result<()> {
        self.frames += (interleaved.len() / self.channels) as u64;
        self.pending.extend_from_slice(interleaved);
        let need = FRAME * self.channels;
        let mut consumed = 0;
        while self.pending.len() - consumed >= need {
            self.encode_frame(consumed, PacketWriteEndInfo::NormalPacket)?;
            consumed += need;
        }
        self.pending.drain(..consumed);
        Ok(())
    }

    /// Pads so the decoder can flush the encoder delay, closes the stream.
    pub fn finish(mut self) -> Result<W> {
        let need = FRAME * self.channels;
        let min_len = self.pending.len() + PRE_SKIP as usize * self.channels;
        self.pending.resize(min_len.div_ceil(need) * need, 0.0);
        let frames = self.pending.len() / need;
        for i in 0..frames {
            let end = if i + 1 == frames { PacketWriteEndInfo::EndStream } else { PacketWriteEndInfo::NormalPacket };
            self.encode_frame(i * need, end)?;
        }
        Ok(self.ogg.into_inner())
    }

    fn encode_frame(&mut self, offset: usize, end: PacketWriteEndInfo) -> Result<()> {
        let input = &self.pending[offset..offset + FRAME * self.channels];
        let len = self.enc.encode(input, FRAME, &mut self.packet)?;
        self.encoded += FRAME as u64;
        // Granule positions count decoded samples, pre-skip included; the
        // last one is clamped so the padding is trimmed on playback.
        let granule = match end {
            PacketWriteEndInfo::EndStream => self.frames + PRE_SKIP as u64,
            _ => self.encoded,
        };
        self.ogg.write_packet(self.packet[..len].to_vec(), self.serial, end, granule)?;
        Ok(())
    }
}

fn opus_head(channels: u8, input_rate: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(19);
    h.extend_from_slice(b"OpusHead");
    h.push(1);
    h.push(channels);
    h.extend_from_slice(&PRE_SKIP.to_le_bytes());
    h.extend_from_slice(&input_rate.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // channel mapping family: mono/stereo
    h
}

/// Vendor-only comment header; real tags are written by `copy_tags` afterwards.
fn opus_tags() -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    t.extend_from_slice(VENDOR.as_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

/// Copy `src`'s tags onto the finished Opus file `dst`. ReplayGain tags are
/// dropped since the audio is already normalized. Unreadable source tags
/// are reported as a warning rather than an error: the audio is still good.
pub fn copy_tags(src: &Path, dst: &Path, keep_art: bool) -> Result<Option<String>> {
    let comments = match read_comments(src, keep_art) {
        Ok(c) => c,
        Err(e) => return Ok(Some(format!("tags not copied: {e}"))),
    };
    if comments.items().len() == 0 && comments.pictures().is_empty() {
        return Ok(None);
    }
    let mut file = File::options().read(true).write(true).open(dst)?;
    comments.save_to(&mut file, WriteOptions::default())?;
    Ok(None)
}

const GAIN_TAGS: [&str; 6] = [
    "REPLAYGAIN_TRACK_GAIN",
    "REPLAYGAIN_TRACK_PEAK",
    "REPLAYGAIN_ALBUM_GAIN",
    "REPLAYGAIN_ALBUM_PEAK",
    "R128_TRACK_GAIN",
    "R128_ALBUM_GAIN",
];

/// Sources that already carry Vorbis comments are copied field for field;
/// everything else goes through lofty's generic mapping, which drops
/// fields it has no name for.
fn read_comments(src: &Path, keep_art: bool) -> std::result::Result<VorbisComments, lofty::error::FileParseError> {
    let probe = Probe::open(src)?.guess_file_type()?;
    let mut comments = match probe.file_type() {
        Some(FileType::Flac) => FlacFile::read_from(&mut File::open(src)?, ParseOptions::default())?
            .vorbis_comments()
            .cloned()
            .unwrap_or_default(),
        Some(FileType::Opus) => OpusFile::read_from(&mut File::open(src)?, ParseOptions::default())?
            .vorbis_comments()
            .clone(),
        Some(FileType::Vorbis) => VorbisFile::read_from(&mut File::open(src)?, ParseOptions::default())?
            .vorbis_comments()
            .clone(),
        _ => {
            let tagged = probe.read()?;
            let mut tag = Tag::new(TagType::VorbisComments);
            // Merge every tag found (ID3v2 + ID3v1 + APE, say); earlier ones win.
            for t in tagged.tags() {
                for item in t.items() {
                    if tag.get(item.key()).is_none() {
                        tag.insert(item.clone());
                    }
                }
                for p in t.pictures() {
                    tag.push_picture(p.clone());
                }
            }
            VorbisComments::from(tag)
        }
    };
    for key in GAIN_TAGS {
        comments.remove(key).for_each(drop);
    }
    if !keep_art {
        comments.remove_pictures();
    }
    comments.set_vendor(VENDOR.to_owned());
    Ok(comments)
}

pub fn buffered(path: &Path) -> io::Result<BufWriter<File>> {
    Ok(BufWriter::new(File::create(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An impulse encoded and decoded comes back `PRE_SKIP` frames late.
    #[test]
    fn encoder_delay_is_pre_skip() {
        for channels in [1, 2] {
            let mut enc = new_encoder(channels, 120).unwrap();
            let layout = if channels == 1 { opuscule::Channels::Mono } else { opuscule::Channels::Stereo };
            let mut dec = opuscule::Decoder::new(opuscule::SampleRate::Hz48000, layout);
            let mut input = vec![0.0f32; FRAME * 8 * channels];
            input[2000 * channels] = 0.9;
            let mut packet = vec![0u8; MAX_PACKET];
            let mut output = Vec::new();
            let mut pcm = vec![0.0f32; FRAME * channels];
            for frame in input.chunks(FRAME * channels) {
                let len = enc.encode(frame, FRAME, &mut packet).unwrap();
                let n = dec.decode(Some(&packet[..len]), &mut pcm, false).unwrap();
                output.extend_from_slice(&pcm[..n * channels]);
            }
            let (peak_at, _) = output
                .iter()
                .enumerate()
                .fold((0, 0f32), |(bi, bv), (i, &v)| if v.abs() > bv { (i, v.abs()) } else { (bi, bv) });
            assert_eq!(peak_at / channels, 2000 + PRE_SKIP as usize, "{channels} channels");
        }
    }
}
