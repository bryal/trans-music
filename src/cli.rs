//! Command line grammar; `xflags!` generates `TransMusic` from it.

use std::process::exit;

mod flags {
    use std::num::NonZeroUsize;
    use std::path::PathBuf;

    xflags::xflags! {
        /// Mirror a music library as loudness-normalized Opus.
        ///
        /// Every audio file under <src> is transcoded to <dst>/<same path>.opus,
        /// unless the output already exists and is newer than the source.
        cmd trans-music {
            /// Library to read
            required src: PathBuf
            /// Mirror to write
            required dst: PathBuf
            /// Opus bitrate in kbit/s
            required -b,--bitrate kbps: u32
            /// Integrated loudness target in LUFS [default: -16]
            optional -l,--loudness lufs: f64
            /// True-peak ceiling in dBTP; the gain is reduced to respect it [default: -2]
            optional -p,--true-peak dbtp: f64
            /// Skip loudness normalization
            optional -N,--no-normalize
            /// Files transcoded in parallel [default: CPU count]
            optional -j,--jobs n: NonZeroUsize
            /// Re-transcode even if the output is up to date
            optional -f,--force
            /// List what would be done, change nothing
            optional -n,--dry-run
            /// Delete files in <dst> with no counterpart in <src>
            optional --prune
            /// Do not copy embedded cover art
            optional --strip-art
            /// Only report errors and warnings
            optional -q,--quiet
            /// Report every file, including skipped ones
            optional -v,--verbose
        }
    }
}

pub use flags::TransMusic;

/// Parses the command line. Exits with a message on bad usage.
pub fn parse() -> TransMusic {
    // Checked before parsing so it works without the required arguments.
    if std::env::args_os().skip(1).any(|a| a == "-V" || a == "--version") {
        println!("trans-music {}", env!("CARGO_PKG_VERSION"));
        exit(0)
    }
    let flags = TransMusic::from_env_or_exit();
    if !(6..=510).contains(&flags.bitrate) {
        eprintln!("error: --bitrate must be between 6 and 510 kbit/s");
        exit(2)
    }
    flags
}
