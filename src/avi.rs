//! A clip as one file: an MJPEG AVI with an optional PCM soundtrack, and the
//! WAV beside a clip that has one.
//!
//! **Why MJPEG AVI.** It is the one container sd-cli always builds (`.webm`
//! and animated `.webp` need libraries it may be built without), it needs no
//! codec beyond a JPEG encoder, and every desktop player opens it — VLC, mpv,
//! GNOME Videos, the Windows player. The browser can't, which costs nothing:
//! the Video tab plays frames on a canvas. Laid out as sd.cpp's
//! `create_mjpg_avi_from_sd_images` writes it (`examples/common/media_io.cpp`),
//! so the file this writes and the one sd-cli writes for MiniMax H3 are read by
//! the same [`read`].
//!
//! **The frames stay the master copy** (`work/videos/{id}/frame_NNN.png`,
//! invariant 9): playback and *Enlarge* read them, and this file is made from
//! them for the person who wants a video.
//!
//! One deliberate difference from sd.cpp: `idx1` offsets are relative to the
//! `movi` list, the convention ffmpeg writes. sd.cpp writes them from the start
//! of the file; players accept both, and [`read`] needs neither.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// JPEG quality. Above sd-cli's 90 (which it caps at, for stb's encoder's
/// sake): the frames are the master copy and this is the copy people keep.
const QUALITY: u8 = 95;

/// 16-bit little-endian PCM, interleaved — what sd.cpp's audio VAE decodes to
/// once `audio_to_pcm16_bytes` has run, and what a WAV and an AVI carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audio {
    pub sample_rate: u32,
    pub channels: u16,
    pub pcm: Vec<u8>,
}

impl Audio {
    fn block_align(&self) -> u16 {
        self.channels * 2
    }

    /// How long it plays.
    pub fn seconds(&self) -> f32 {
        let per_second = u64::from(self.sample_rate) * u64::from(self.block_align());
        if per_second == 0 { 0.0 } else { self.pcm.len() as f32 / per_second as f32 }
    }

    fn samples(&self) -> u32 {
        (self.pcm.len() / usize::from(self.block_align().max(1))) as u32
    }
}

/// Encode `frames` (PNG files, all one size, in order) into an AVI at `fps`,
/// with `audio` as a second stream. Written through a `.part` and a rename, so
/// a half-written file is never what a player or the gallery finds.
pub fn write(path: &Path, frames: &[PathBuf], fps: u32, audio: Option<&Audio>) -> Result<()> {
    let Some(first) = frames.first() else {
        bail!("a clip with no frames has nothing to write");
    };
    let (width, height) = image::image_dimensions(first)
        .with_context(|| format!("reading {}", first.display()))?;
    let mut jpegs = Vec::with_capacity(frames.len());
    for f in frames {
        let img = image::open(f).with_context(|| format!("reading {}", f.display()))?.to_rgb8();
        if img.dimensions() != (width, height) {
            bail!("{} is {}×{}, not the clip's {width}×{height}", f.display(), img.width(), img.height());
        }
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, QUALITY)
            .encode_image(&img)
            .with_context(|| format!("encoding {} as JPEG", f.display()))?;
        jpegs.push(buf);
    }
    let bytes = build(width, height, fps.max(1), &jpegs, audio.filter(|a| !a.pcm.is_empty()));
    let part = path.with_extension("avi.part");
    std::fs::write(&part, bytes).with_context(|| format!("writing {}", part.display()))?;
    std::fs::rename(&part, path).with_context(|| format!("writing {}", path.display()))
}

fn build(width: u32, height: u32, fps: u32, jpegs: &[Vec<u8>], audio: Option<&Audio>) -> Vec<u8> {
    let n = jpegs.len() as u32;
    let mut b = Vec::with_capacity(jpegs.iter().map(Vec::len).sum::<usize>() + 4096);
    let u16le = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
    let u32le = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
    let cc = |b: &mut Vec<u8>, s: &[u8; 4]| b.extend_from_slice(s);

    cc(&mut b, b"RIFF");
    let riff = b.len();
    u32le(&mut b, 0);
    cc(&mut b, b"AVI ");

    // hdrl: the main header and one stream list per stream.
    let mut hdrl = 4 + 8 + 56 + 8 + 4 + 8 + 56 + 8 + 40;
    if audio.is_some() {
        hdrl += 8 + 4 + 8 + 56 + 8 + 16;
    }
    cc(&mut b, b"LIST");
    u32le(&mut b, hdrl);
    cc(&mut b, b"hdrl");
    cc(&mut b, b"avih");
    u32le(&mut b, 56);
    u32le(&mut b, 1_000_000 / fps); // µs per frame
    u32le(&mut b, 0);
    u32le(&mut b, 0);
    u32le(&mut b, 0x110); // AVIF_HASINDEX | AVIF_ISINTERLEAVED
    u32le(&mut b, n);
    u32le(&mut b, 0);
    u32le(&mut b, if audio.is_some() { 2 } else { 1 });
    u32le(&mut b, width * height * 3);
    u32le(&mut b, width);
    u32le(&mut b, height);
    for _ in 0..4 {
        u32le(&mut b, 0);
    }

    cc(&mut b, b"LIST");
    u32le(&mut b, 4 + 8 + 56 + 8 + 40);
    cc(&mut b, b"strl");
    cc(&mut b, b"strh");
    u32le(&mut b, 56);
    cc(&mut b, b"vids");
    cc(&mut b, b"MJPG");
    u32le(&mut b, 0);
    u16le(&mut b, 0);
    u16le(&mut b, 0);
    u32le(&mut b, 0);
    u32le(&mut b, 1); // scale
    u32le(&mut b, fps); // rate: rate / scale = fps
    u32le(&mut b, 0);
    u32le(&mut b, n);
    u32le(&mut b, width * height * 3);
    u32le(&mut b, u32::MAX);
    u32le(&mut b, 0);
    for _ in 0..4 {
        u16le(&mut b, 0);
    }
    cc(&mut b, b"strf");
    u32le(&mut b, 40);
    u32le(&mut b, 40);
    u32le(&mut b, width);
    u32le(&mut b, height);
    u16le(&mut b, 1);
    u16le(&mut b, 24);
    cc(&mut b, b"MJPG");
    u32le(&mut b, width * height * 3);
    for _ in 0..4 {
        u32le(&mut b, 0);
    }

    if let Some(a) = audio {
        let align = a.block_align();
        let rate = a.sample_rate * u32::from(align);
        cc(&mut b, b"LIST");
        u32le(&mut b, 4 + 8 + 56 + 8 + 16);
        cc(&mut b, b"strl");
        cc(&mut b, b"strh");
        u32le(&mut b, 56);
        cc(&mut b, b"auds");
        u32le(&mut b, 0);
        u32le(&mut b, 0);
        u16le(&mut b, 0);
        u16le(&mut b, 0);
        u32le(&mut b, 0);
        u32le(&mut b, u32::from(align)); // scale
        u32le(&mut b, rate); // rate: bytes per second
        u32le(&mut b, 0);
        u32le(&mut b, a.samples());
        u32le(&mut b, a.pcm.len() as u32);
        u32le(&mut b, u32::MAX);
        u32le(&mut b, u32::from(align));
        for _ in 0..4 {
            u16le(&mut b, 0);
        }
        cc(&mut b, b"strf");
        u32le(&mut b, 16);
        u16le(&mut b, 1); // WAVE_FORMAT_PCM
        u16le(&mut b, a.channels);
        u32le(&mut b, a.sample_rate);
        u32le(&mut b, rate);
        u16le(&mut b, align);
        u16le(&mut b, 16);
    }

    cc(&mut b, b"LIST");
    let movi_size = b.len();
    u32le(&mut b, 0);
    let movi = b.len();
    cc(&mut b, b"movi");
    let mut index: Vec<([u8; 4], u32, u32, u32)> = Vec::with_capacity(jpegs.len() + 1);
    let mut chunk = |b: &mut Vec<u8>, id: &[u8; 4], flags: u32, data: &[u8]| {
        index.push((*id, flags, (b.len() - movi) as u32, data.len() as u32));
        b.extend_from_slice(id);
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(data);
        if data.len() % 2 == 1 {
            b.push(0);
        }
    };
    for j in jpegs {
        chunk(&mut b, b"00dc", 0x10, j); // AVIIF_KEYFRAME: every MJPEG frame is one
    }
    if let Some(a) = audio {
        chunk(&mut b, b"01wb", 0, &a.pcm);
    }
    let size = (b.len() - movi_size - 4) as u32;
    b[movi_size..movi_size + 4].copy_from_slice(&size.to_le_bytes());

    cc(&mut b, b"idx1");
    u32le(&mut b, (index.len() * 16) as u32);
    for (id, flags, offset, len) in index {
        b.extend_from_slice(&id);
        u32le(&mut b, flags);
        u32le(&mut b, offset);
        u32le(&mut b, len);
    }
    let size = (b.len() - riff - 4) as u32;
    b[riff..riff + 4].copy_from_slice(&size.to_le_bytes());
    b
}

/// What an AVI holds: its JPEG frames in order, its frame rate, and its PCM
/// track if it has one. For the files above and sd-cli's own — the only AVIs
/// this ever opens, which is why anything but MJPEG video and 16-bit PCM audio
/// is refused rather than guessed at.
#[derive(Debug)]
pub struct Contents {
    pub fps: u32,
    pub frames: Vec<Vec<u8>>,
    pub audio: Option<Audio>,
}

pub fn read(path: &Path) -> Result<Contents> {
    let b = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&b).with_context(|| format!("reading {} as an AVI", path.display()))
}

fn parse(b: &[u8]) -> Result<Contents> {
    let u16at = |i: usize| -> Result<u16> {
        Ok(u16::from_le_bytes(b.get(i..i + 2).context("truncated")?.try_into()?))
    };
    let u32at = |i: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(b.get(i..i + 4).context("truncated")?.try_into()?))
    };
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"AVI ") {
        bail!("not an AVI");
    }
    let mut out = Contents { fps: 0, frames: Vec::new(), audio: None };
    // Stream headers in order: which stream number is audio, and its format.
    let mut streams: Vec<[u8; 4]> = Vec::new();
    let mut pcm_format: Option<(u16, u32)> = None;
    let mut pcm = Vec::new();
    // A flat walk: a LIST's body is entered (its four-byte type skipped),
    // anything else is a chunk to read or step over. Every chunk is padded to
    // an even length.
    let mut i = 12;
    while i + 8 <= b.len() {
        let id: [u8; 4] = b[i..i + 4].try_into()?;
        let len = u32at(i + 4)? as usize;
        let body = i + 8;
        if &id == b"LIST" {
            i = body + 4;
            continue;
        }
        let data = b.get(body..body + len).context("a chunk runs past the end of the file")?;
        match &id {
            b"strh" => {
                let kind: [u8; 4] = data.get(0..4).context("short strh")?.try_into()?;
                if &kind == b"vids" {
                    let scale = u32at(body + 20)?.max(1);
                    out.fps = (u32at(body + 24)? + scale / 2) / scale;
                }
                streams.push(kind);
            }
            b"strf" if streams.last() == Some(b"auds") => {
                let (format, channels, rate, bits) = (u16at(body)?, u16at(body + 2)?, u32at(body + 4)?, u16at(body + 14)?);
                if format != 1 || bits != 16 {
                    bail!("its audio is not 16-bit PCM");
                }
                pcm_format = Some((channels, rate));
            }
            b"strf" if streams.last() == Some(b"vids") && data.get(16..20) != Some(b"MJPG") => {
                bail!("its video is not MJPEG");
            }
            _ if id[2..4] == *b"dc" || id[2..4] == *b"db" => out.frames.push(data.to_vec()),
            _ if id[2..4] == *b"wb" => pcm.extend_from_slice(data),
            _ => {}
        }
        i = body + len + len % 2;
    }
    if out.frames.is_empty() {
        bail!("it holds no frames");
    }
    if let Some((channels, sample_rate)) = pcm_format.filter(|_| !pcm.is_empty()) {
        out.audio = Some(Audio { sample_rate, channels, pcm });
    }
    Ok(out)
}

/// Write `audio` as a WAV file.
pub fn write_wav(path: &Path, audio: &Audio) -> Result<()> {
    let mut b = Vec::with_capacity(44 + audio.pcm.len());
    let align = audio.block_align();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + audio.pcm.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&audio.channels.to_le_bytes());
    b.extend_from_slice(&audio.sample_rate.to_le_bytes());
    b.extend_from_slice(&(audio.sample_rate * u32::from(align)).to_le_bytes());
    b.extend_from_slice(&align.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(audio.pcm.len() as u32).to_le_bytes());
    b.extend_from_slice(&audio.pcm);
    std::fs::write(path, b).with_context(|| format!("writing {}", path.display()))
}

/// Read a WAV [`write_wav`] wrote: 16-bit PCM, its `fmt ` and `data` chunks
/// found by walking rather than assumed at fixed offsets.
pub fn read_wav(path: &Path) -> Result<Audio> {
    let b = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    parse_wav(&b).with_context(|| path.display().to_string())
}

/// [`read_wav`] over bytes in hand: an upload is checked before it is kept.
pub fn parse_wav(b: &[u8]) -> Result<Audio> {
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        bail!("not a WAV file");
    }
    let mut format = None;
    let mut i = 12;
    while i + 8 <= b.len() {
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into()?) as usize;
        let data = b.get(i + 8..i + 8 + len).context("a WAV chunk runs past the end")?;
        match &b[i..i + 4] {
            b"fmt " if data.len() >= 16 => {
                let bits = u16::from_le_bytes(data[14..16].try_into()?);
                if u16::from_le_bytes(data[0..2].try_into()?) != 1 || bits != 16 {
                    bail!("not 16-bit PCM");
                }
                format = Some((u16::from_le_bytes(data[2..4].try_into()?), u32::from_le_bytes(data[4..8].try_into()?)));
            }
            b"data" => {
                let (channels, sample_rate) = format.context("a WAV whose data comes before its format")?;
                return Ok(Audio { sample_rate, channels, pcm: data.to_vec() });
            }
            _ => {}
        }
        i += 8 + len + len % 2;
    }
    bail!("no audio data")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yasdig-avi-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Frames, rate and soundtrack all survive the trip, and the layout is one
    /// a player reads: the RIFF and `movi` sizes add up and the index points
    /// at each chunk.
    #[test]
    fn a_clip_round_trips() {
        let dir = tmp("rt");
        let frames: Vec<PathBuf> = (0..3u8)
            .map(|i| {
                let p = dir.join(format!("frame_{i:03}.png"));
                image::RgbImage::from_pixel(64, 48, image::Rgb([i * 80, 40, 200])).save(&p).unwrap();
                p
            })
            .collect();
        let audio = Audio { sample_rate: 32_000, channels: 2, pcm: (0..401u32).map(|x| x as u8).collect() };
        let out = dir.join("clip.avi");
        write(&out, &frames, 24, Some(&audio)).unwrap();

        let b = std::fs::read(&out).unwrap();
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize, b.len() - 8);
        let c = read(&out).unwrap();
        assert_eq!(c.fps, 24);
        assert_eq!(c.frames.len(), 3);
        let first = image::load_from_memory(&c.frames[0]).unwrap();
        assert_eq!((first.width(), first.height()), (64, 48));
        assert_eq!(c.audio.as_ref(), Some(&audio));
        // The odd-length PCM chunk is padded, and the index still finds it.
        let movi = b.windows(4).position(|w| w == b"movi").unwrap();
        let idx = b.windows(4).rposition(|w| w == b"idx1").unwrap();
        let last = &b[idx + 8 + 3 * 16..idx + 8 + 4 * 16];
        assert_eq!(&last[0..4], b"01wb");
        let off = u32::from_le_bytes(last[8..12].try_into().unwrap()) as usize;
        assert_eq!(&b[movi + off..movi + off + 4], b"01wb");

        let wav = dir.join("audio.wav");
        write_wav(&wav, &audio).unwrap();
        assert_eq!(read_wav(&wav).unwrap(), audio);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_silent_clip_has_one_stream_and_frames_of_another_size_are_refused() {
        let dir = tmp("one");
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        image::RgbImage::new(32, 32).save(&a).unwrap();
        image::RgbImage::new(32, 16).save(&b).unwrap();
        let out = dir.join("clip.avi");
        write(&out, std::slice::from_ref(&a), 16, None).unwrap();
        let c = read(&out).unwrap();
        assert_eq!((c.fps, c.frames.len()), (16, 1));
        assert!(c.audio.is_none());
        assert!(write(&out, &[a, b], 16, None).is_err());
        assert!(parse(b"RIFF\0\0\0\0WAVE").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
