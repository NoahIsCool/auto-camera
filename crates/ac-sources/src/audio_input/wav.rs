//! The fake signal written as a WAV or RF64 file, for tests of file inputs.
//!
//! A test of an audio-only file input needs a file whose every sample it
//! knows. [`write_fake_wav`] writes [`fake_sample`] of each channel and
//! frame, the signal [`crate::audio_input::fake`] delivers, so what a file
//! input reads back is checked against the same function, bit for bit:
//!
//! * [`WavEncoding::Pcm16`] stores each sample as a 16-bit integer, which
//!   the fake signal fills exactly, so the `f32` ffmpeg decodes is the
//!   fake sample itself;
//! * [`WavEncoding::Float32`] stores the `f32` as it is.
//!
//! A plain file is RIFF/WAVE with a `fmt ` and a `data` chunk; an RF64 one
//! (EBU Tech 3306, what recorders write past 4 GiB) puts `RF64` and the
//! placeholder size `0xFFFFFFFF` in the header and the real sizes in a
//! `ds64` chunk first, so files of either layout are made without writing
//! gigabytes. Nothing large is kept in the repository: tests make their
//! files when they run.

use std::io::{self, Write};
use std::path::Path;

use crate::audio_input::fake_sample;

/// How samples are stored in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavEncoding {
    /// 16-bit little-endian integers (format tag 1).
    Pcm16,
    /// 32-bit little-endian floats (format tag 3).
    Float32,
}

impl WavEncoding {
    /// The `fmt ` chunk's format tag.
    fn tag(self) -> u16 {
        match self {
            WavEncoding::Pcm16 => 1,
            WavEncoding::Float32 => 3,
        }
    }

    /// Bytes per sample.
    fn bytes(self) -> u16 {
        match self {
            WavEncoding::Pcm16 => 2,
            WavEncoding::Float32 => 4,
        }
    }
}

/// Writes `frames` sample frames of the fake signal on `channels` channels
/// at `sample_rate` to `path`, as RF64 when `rf64`; see the module docs.
pub fn write_fake_wav(
    path: &Path,
    sample_rate: u32,
    channels: u16,
    frames: u64,
    encoding: WavEncoding,
    rf64: bool,
) -> io::Result<()> {
    let block_align = channels * encoding.bytes();
    let mut fmt = Vec::with_capacity(16);
    fmt.extend(encoding.tag().to_le_bytes());
    fmt.extend(channels.to_le_bytes());
    fmt.extend(sample_rate.to_le_bytes());
    fmt.extend((sample_rate * u32::from(block_align)).to_le_bytes());
    fmt.extend(block_align.to_le_bytes());
    fmt.extend((encoding.bytes() * 8).to_le_bytes());
    let mut data = Vec::with_capacity(frames as usize * usize::from(block_align));
    for n in 0..frames {
        for c in 0..channels {
            let v = fake_sample(c, n);
            match encoding {
                WavEncoding::Pcm16 => data.extend(((v * 32_768.0) as i16).to_le_bytes()),
                WavEncoding::Float32 => data.extend(v.to_le_bytes()),
            }
        }
    }
    let data_len = data.len() as u64;
    let mut out = Vec::with_capacity(data.len() + 80);
    let chunk = |out: &mut Vec<u8>, id: &[u8], body: &[u8], size: u32| {
        out.extend(id);
        out.extend(size.to_le_bytes());
        out.extend(body);
    };
    if rf64 {
        // RIFF size, data size, sample count, table length.
        let riff = 4 + (8 + 28) + (8 + fmt.len() as u64) + 8 + data_len;
        let mut ds64 = Vec::with_capacity(28);
        ds64.extend(riff.to_le_bytes());
        ds64.extend(data_len.to_le_bytes());
        ds64.extend(frames.to_le_bytes());
        ds64.extend(0u32.to_le_bytes());
        out.extend(b"RF64");
        out.extend(u32::MAX.to_le_bytes());
        out.extend(b"WAVE");
        chunk(&mut out, b"ds64", &ds64, 28);
        chunk(&mut out, b"fmt ", &fmt, fmt.len() as u32);
        chunk(&mut out, b"data", &data, u32::MAX);
    } else {
        let size = |n: u64| {
            u32::try_from(n).map_err(|_| io::Error::other("over 4 GiB of samples needs RF64"))
        };
        let riff = size(4 + (8 + fmt.len() as u64) + 8 + data_len)?;
        out.extend(b"RIFF");
        out.extend(riff.to_le_bytes());
        out.extend(b"WAVE");
        chunk(&mut out, b"fmt ", &fmt, fmt.len() as u32);
        chunk(&mut out, b"data", &data, size(data_len)?);
    }
    std::fs::File::create(path)?.write_all(&out)
}
