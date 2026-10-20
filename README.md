# trans-music

Mirror a music library as loudness-normalized Opus. Everything is built
into the binary: decoding, EBU R128 measurement, resampling, tagging and a
statically linked libopus. It needs nothing but libc at runtime, so a
system upgrade cannot break it.

```
trans-music [OPTIONS] <SRC> <DST>
```

Every audio file under `SRC` becomes `DST/<same path>.opus`. Files whose
output already exists and is newer than the source are skipped, so reruns
only pick up new or changed tracks. See `trans-music --help` for options:
bitrate (required), loudness target, true-peak ceiling, job count, `--prune`,
`--strip-art`, `--dry-run`, `--force`.

## What it does to each file

1. Decode and measure integrated loudness and true peak (`ebur128`). Mono
   files are measured as dual mono, like `ffmpeg-normalize --dual-mono`.
2. Decode again, apply one constant gain that lands on the target
   (default -16 LUFS). If that would push the true peak above the ceiling
   (default -2 dBTP) the gain is reduced instead, and a warning is printed.
   There is no dynamic processing.
3. Resample to 48 kHz if needed (`rubato`), encode with libopus
   (VBR, complexity 10, music signal hint), mux into Ogg.
4. Copy tags. FLAC and Ogg sources keep their Vorbis comments verbatim;
   ID3 and MP4 tags go through `lofty`'s mapping. ReplayGain and R128 gain
   tags are dropped because the audio is already normalized.

Input: flac, mp3, aac/alac in m4a/mp4, vorbis/opus in ogg/oga/opus, wav,
mka. Mono and stereo only.

Output is written to `<name>.opus.part` and renamed when complete, so an
interrupted run never leaves a truncated file that looks up to date.

## Building

```
cargo build --release
```

Needs a C compiler and cmake for the bundled libopus (`opusic-sys`).
Everything else is Rust.
