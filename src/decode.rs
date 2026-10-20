//! Decode any supported audio file to interleaved f32: symphonia for
//! everything but Opus, which it lacks, and libopus for Opus.

use std::fs::File;
use std::path::Path;

use opusic_c::Channels;
use symphonia::core::codecs::audio::AudioDecoder;
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;

pub type Result<T> = std::result::Result<T, Error>;

/// Opus allows up to 120 ms per packet.
const MAX_OPUS_FRAMES: usize = 5760;

enum Decoder {
    Symphonia(Box<dyn AudioDecoder>),
    /// `skip` counts the OpusHead pre-skip frames still to drop; the
    /// demuxer trims only the end padding.
    Opus {
        dec: opusic_c::Decoder,
        skip: usize,
    },
}

/// A decoding session over one file, yielding interleaved f32 chunks.
pub struct AudioReader {
    format: Box<dyn FormatReader>,
    decoder: Decoder,
    track_id: u32,
    pub rate: u32,
    pub channels: Channels,
    chunk: Vec<f32>,
}

impl AudioReader {
    /// Opens `path` for decoding. Only mono and stereo are supported.
    pub fn open(path: &Path) -> Result<Self> {
        let mss = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
        let format = symphonia::default::get_probe().probe(&Hint::new(), mss, Default::default(), Default::default())?;
        let track = format.default_track(TrackType::Audio).ok_or(Error::Unsupported("no audio track"))?;
        let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or(Error::Unsupported("no audio track"))?;
        let rate = params.sample_rate.ok_or(Error::Unsupported("unknown sample rate"))?;
        let channels = match params.channels.as_ref().map(|c| c.count()) {
            Some(1) => Channels::Mono,
            Some(2) => Channels::Stereo,
            _ => return Err(Error::Unsupported("only mono and stereo are supported")),
        };
        let decoder = if params.codec == CODEC_ID_OPUS {
            // OpusHead is little-endian. MP4's dOps, which symphonia passes on
            // behind an "OpusHead" prefix, is big-endian with version 0.
            let head = params.extra_data.as_deref().unwrap_or_default();
            let field = |at: usize| {
                let bytes = [*head.get(at)?, *head.get(at + 1)?];
                Some(if head.get(8) == Some(&0) { u16::from_be_bytes(bytes) } else { u16::from_le_bytes(bytes) })
            };
            let (skip, output_gain) = (field(10).unwrap_or(0), field(16).unwrap_or(0) as i16);
            let mut dec =
                opusic_c::Decoder::new(channels, opusic_c::SampleRate::Hz48000).map_err(|e| Error::Unsupported(e.message()))?;
            dec.set_gain(output_gain.into()).map_err(|e| Error::Unsupported(e.message()))?;
            Decoder::Opus { dec, skip: skip.into() }
        } else {
            Decoder::Symphonia(symphonia::default::get_codecs().make_audio_decoder(params, &Default::default())?)
        };
        Ok(Self { track_id: track.id, format, decoder, rate, channels, chunk: Vec::new() })
    }

    /// The next decoded chunk, or `None` at end of stream.
    pub fn next_chunk(&mut self) -> Result<Option<&[f32]>> {
        let ch = self.channels as usize;
        loop {
            let Some(packet) = self.format.next_packet()? else { return Ok(None) };
            if packet.track_id != self.track_id {
                continue;
            }
            match &mut self.decoder {
                Decoder::Symphonia(dec) => dec.decode(&packet)?.copy_to_vec_interleaved(&mut self.chunk),
                Decoder::Opus { dec, skip } => {
                    self.chunk.resize(MAX_OPUS_FRAMES * ch, 0.0);
                    let frames = dec
                        .decode_float_to_slice(&packet.data, &mut self.chunk, false)
                        .map_err(|e| Error::DecodeError(e.message()))?;
                    let head = (*skip).min(frames);
                    *skip -= head;
                    let tail = frames.saturating_sub(packet.trim_end.get() as usize).max(head);
                    self.chunk.truncate(tail * ch);
                    self.chunk.drain(..head * ch);
                }
            }
            return Ok(Some(&self.chunk));
        }
    }
}
