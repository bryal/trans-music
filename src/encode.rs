//! Opus encoding into an Ogg container (RFC 7845), and the tags to put in it.

use std::fs::File;
use std::hash::BuildHasher;
use std::io::Write;
use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::ogg::tag::VorbisComments;
use lofty::ogg::{OggPictureStorage, OpusFile, VorbisFile};
use lofty::probe::Probe;
use lofty::tag::TagExt;
use ogg::{PacketWriteEndInfo, PacketWriter};
use opusic_c::{Application, Bitrate, Channels, Encoder, SampleRate, Signal};

pub const RATE: u32 = 48_000;
/// 20 ms, the frame size libopus is tuned for.
const FRAME: usize = 960;
const MAX_PACKET: usize = 1276;
const VENDOR: &str = concat!("trans-music ", env!("CARGO_PKG_VERSION"));

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// libopus error codes don't implement `Error`.
fn opus<T>(r: std::result::Result<T, opusic_c::ErrorCode>) -> Result<T> {
    r.map_err(|e| format!("opus: {e:?}").into())
}

fn new_encoder(channels: Channels, bitrate_kbps: u32) -> Result<Encoder> {
    let mut enc = opus(Encoder::new(channels, SampleRate::Hz48000, Application::Audio))?;
    opus(enc.set_bitrate(Bitrate::Value(bitrate_kbps * 1000)))?;
    opus(enc.set_vbr(true))?;
    opus(enc.set_complexity(10))?;
    opus(enc.set_signal(Signal::Music))?;
    Ok(enc)
}

/// Streams 48 kHz interleaved f32 into an Ogg Opus file.
pub struct OggOpusWriter<W: Write> {
    enc: Encoder,
    ogg: PacketWriter<'static, W>,
    serial: u32,
    channels: usize,
    /// Encoder delay in samples; decoders drop this many at the start.
    pre_skip: u16,
    pending: Vec<f32>,
    packet: Vec<u8>,
    /// Real (unpadded) frames fed so far.
    frames: u64,
    /// Frames handed to the encoder so far, padding included.
    encoded: u64,
}

impl<W: Write> OggOpusWriter<W> {
    pub fn new(out: W, channels: Channels, input_rate: u32, bitrate_kbps: u32, mut tags: VorbisComments) -> Result<Self> {
        let mut enc = new_encoder(channels, bitrate_kbps)?;
        let pre_skip = u16::try_from(opus(enc.get_look_ahead())?)?;
        let serial = std::hash::RandomState::new().hash_one(0u8) as u32;
        let mut ogg = PacketWriter::new(out);
        ogg.write_packet(opus_head(channels as u8, pre_skip, input_rate), serial, PacketWriteEndInfo::EndPage, 0)?;
        tags.set_vendor(VENDOR.to_owned());
        let mut opus_tags = b"OpusTags".to_vec();
        tags.dump_to(&mut opus_tags, WriteOptions::default())?;
        ogg.write_packet(opus_tags, serial, PacketWriteEndInfo::EndPage, 0)?;
        Ok(Self {
            enc,
            ogg,
            serial,
            channels: channels as usize,
            pre_skip,
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
        let min_len = self.pending.len() + self.pre_skip as usize * self.channels;
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
        let len = opus(self.enc.encode_float_to_slice(input, &mut self.packet))?;
        self.encoded += FRAME as u64;
        // Granule positions count decoded samples, pre-skip included; the
        // last one is clamped so the padding is trimmed on playback.
        let granule = match end {
            PacketWriteEndInfo::EndStream => self.frames + self.pre_skip as u64,
            _ => self.encoded,
        };
        self.ogg.write_packet(self.packet[..len].to_vec(), self.serial, end, granule)?;
        Ok(())
    }
}

fn opus_head(channels: u8, pre_skip: u16, input_rate: u32) -> Vec<u8> {
    let mapping_family_and_gain = [0, 0, 0];
    [b"OpusHead", &[1, channels][..], &pre_skip.to_le_bytes(), &input_rate.to_le_bytes(), &mapping_family_and_gain].concat()
}

const GAIN_TAGS: [&str; 6] = [
    "REPLAYGAIN_TRACK_GAIN",
    "REPLAYGAIN_TRACK_PEAK",
    "REPLAYGAIN_ALBUM_GAIN",
    "REPLAYGAIN_ALBUM_PEAK",
    "R128_TRACK_GAIN",
    "R128_ALBUM_GAIN",
];

/// The tags of `src` as Vorbis comments, minus ReplayGain and R128 gains.
/// FLAC and Ogg sources are copied field for field; other formats go
/// through lofty's generic mapping, which drops fields it has no name for.
pub fn read_tags(src: &Path, keep_art: bool) -> Result<VorbisComments> {
    let probe = Probe::open(src)?.guess_file_type()?;
    let mut tags = match probe.file_type() {
        Some(FileType::Flac) => {
            let flac = FlacFile::read_from(&mut File::open(src)?, ParseOptions::default())?;
            let mut tags = flac.vorbis_comments().cloned().unwrap_or_default();
            for (picture, info) in flac.pictures() {
                tags.insert_picture(picture.clone(), Some(*info))?;
            }
            tags
        }
        Some(FileType::Opus) => OpusFile::read_from(&mut File::open(src)?, ParseOptions::default())?.vorbis_comments().clone(),
        Some(FileType::Vorbis) => {
            VorbisFile::read_from(&mut File::open(src)?, ParseOptions::default())?.vorbis_comments().clone()
        }
        _ => {
            let tagged = probe.read()?;
            tagged.primary_tag().or(tagged.first_tag()).cloned().map(VorbisComments::from).unwrap_or_default()
        }
    };
    for key in GAIN_TAGS {
        tags.remove(key).for_each(drop);
    }
    if !keep_art {
        tags.remove_pictures();
    }
    Ok(tags)
}
