//! Decode any supported audio file to interleaved f32 via symphonia.
//! Opus input is handled by registering `opuscule` as a symphonia decoder.

use std::fs::File;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use symphonia::core::audio::{Audio, AudioBuffer, AudioMut, AudioSpec};
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult};
use symphonia::core::codecs::registry::{CodecRegistry, RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia::core::codecs::CodecInfo;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::{Hint, Probe};
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::PacketRef;
use symphonia_core::audio::{AsGenericAudioBufferRef, GenericAudioBufferRef};
use symphonia_core::support_audio_codec;

pub type Result<T> = std::result::Result<T, Error>;

static CODECS: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut r = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut r);
    r.register_audio_decoder::<OpusDecoder>();
    r
});

static PROBE: LazyLock<Probe> = LazyLock::new(|| {
    let mut p = Probe::default();
    symphonia::default::register_enabled_formats(&mut p);
    p
});

/// A decoding session over one file, yielding interleaved f32 chunks.
pub struct AudioReader {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    pub rate: u32,
    pub channels: usize,
    chunk: Vec<f32>,
}

impl AudioReader {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = PROBE.probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or(Error::Unsupported("no audio track"))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(Error::Unsupported("no audio codec parameters"))?;
        let rate = params.sample_rate.ok_or(Error::Unsupported("unknown sample rate"))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .ok_or(Error::Unsupported("unknown channel count"))?;
        let decoder = CODECS.make_audio_decoder(params, &AudioDecoderOptions::default())?;
        Ok(Self { track_id: track.id, format, decoder, rate, channels, chunk: Vec::new() })
    }


    /// The next decoded chunk, or `None` at end of stream.
    pub fn next_chunk(&mut self) -> Result<Option<&[f32]>> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None),
                // A clean end of stream is `Ok(None)`; hitting EOF inside a
                // packet means the file is truncated.
                Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Err(Error::DecodeError("file is truncated"));
                }
                Err(e) => return Err(e),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    self.chunk.clear();
                    buf.copy_to_vec_interleaved(&mut self.chunk);
                    return Ok(Some(&self.chunk));
                }
                // A corrupt packet is skipped, like most players do.
                Err(Error::DecodeError(_)) => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

/// libopus wrapped as a symphonia decoder. Only channel mapping family 0
/// (mono and stereo) is supported, which covers music libraries.
struct OpusDecoder {
    /// Behind a mutex only because symphonia wants decoders to be `Sync`.
    inner: Mutex<opusic_c::Decoder>,
    params: AudioCodecParameters,
    pcm: Vec<f32>,
    buf: AudioBuffer<f32>,
    /// Encoder delay from OpusHead. The Ogg demuxer trims end padding but
    /// leaves pre-skip to the decoder.
    pre_skip: usize,
    skip: usize,
}

/// Opus allows up to 120 ms per packet.
const MAX_FRAMES: usize = 5760;

impl OpusDecoder {
    fn new_inner(n: usize) -> Result<opusic_c::Decoder> {
        let layout = match n {
            1 => opusic_c::Channels::Mono,
            2 => opusic_c::Channels::Stereo,
            _ => return Err(Error::Unsupported("opus: only mono and stereo are supported")),
        };
        opusic_c::Decoder::new(layout, opusic_c::SampleRate::Hz48000).map_err(opus_error)
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Box<dyn AudioDecoder>> {
        let channels = params.channels.clone().ok_or(Error::Unsupported("opus: unknown channels"))?;
        let n = channels.count();
        let inner = Mutex::new(Self::new_inner(n)?);
        let buf = AudioBuffer::new(AudioSpec::new(48_000, channels), MAX_FRAMES);
        let pre_skip = match params.extra_data.as_deref() {
            Some(head) if head.len() >= 12 && head.starts_with(b"OpusHead") => {
                u16::from_le_bytes([head[10], head[11]]) as usize
            }
            _ => 0,
        };
        Ok(Box::new(Self {
            inner,
            params: params.clone(),
            pcm: vec![0.0; MAX_FRAMES * n],
            buf,
            pre_skip,
            skip: pre_skip,
        }))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_OPUS, "opus", "Opus (libopus)")]
    }
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        let _ = self.inner.get_mut().unwrap().reset();
        self.skip = self.pre_skip;
    }

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()[0].info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        let frames = self
            .inner
            .get_mut()
            .unwrap()
            .decode_float_to_slice(packet.data, &mut self.pcm, false)
            .map_err(|_| Error::DecodeError("opus: invalid packet"))?;
        self.buf.clear();
        self.buf.render_uninit(Some(frames));
        let n = self.buf.spec().channels().count();
        self.buf.copy_from_slice_interleaved(&&self.pcm[..frames * n]);
        let start = packet.trim_start.get() as usize + self.skip.min(frames);
        self.skip -= self.skip.min(frames);
        self.buf.trim(start.min(frames), packet.trim_end.get() as usize);
        Ok(self.buf.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

fn opus_error(e: opusic_c::ErrorCode) -> Error {
    Error::Unsupported(match e {
        opusic_c::ErrorCode::AllocFail => "opus: allocation failed",
        _ => "opus: bad argument",
    })
}
