import { mp4Muxer } from './mp4mux.js';
import { webmMuxer } from './webmmux.js';
import { gifEncoder, histogram, palette } from './gifenc.js';

// ---- A clip as a file to share -----------------------------------------------
//
// The clip dialog's Download panel. The server keeps a clip as frames (and
// an MJPEG AVI for *Save to output*); a file for Discord or a phone is
// encoded here, by the browser's own encoders (WebCodecs) — no codec in the
// program, and nothing new to license. So what is offered is what this
// browser can encode: `available()` asks it. Firefox on Linux may have no
// H.264 encoder; VP9 is everywhere WebCodecs is.
//
// Frames come from the server one at a time (`frameUrl(k)`), so a long clip
// is never all in memory; the sound from its `audio.wav`, as Opus.

export const FORMATS = [
  { id: 'mp4', label: 'MP4 (H.264)', ext: 'mp4', type: 'video/mp4',
    note: 'Plays everywhere: Discord, phones, every player and editor.' },
  { id: 'webm', label: 'WebM (VP9)', ext: 'webm', type: 'video/webm',
    note: 'An open format; Discord and every browser play it, some phones not.' },
  { id: 'av1', label: 'MP4 (AV1)', ext: 'mp4', type: 'video/mp4',
    note: 'The smallest file at the same quality; needs a newer player.' },
  { id: 'gif', label: 'GIF', ext: 'gif', type: 'image/gif',
    note: 'Loops on its own anywhere, a chat included — but no sound, 256 colours, and at most 480 px wide.' },
  { id: 'avi', label: 'AVI (original)', ext: 'avi', type: 'video/x-msvideo',
    note: 'The file yasdig keeps: MJPEG, large, sound included. Nothing is encoded.' },
];

// What Discord takes without Nitro, with room for the container.
export const DISCORD_BYTES = 10e6;
const AUDIO_BITRATE = 96000;
// A GIF wider than this is megabytes a second.
export const GIF_WIDTH = 480;

const even = (n) => Math.max(2, Math.floor(n / 2) * 2);

// The frame size a download is encoded at: scaled, even (4:2:0 halves the
// chroma), and for a GIF no wider than GIF_WIDTH.
export function outSize(fmt, width, height, scale) {
  let w = width * scale;
  let h = height * scale;
  if (fmt === 'gif' && w > GIF_WIDTH) { h = (h * GIF_WIDTH) / w; w = GIF_WIDTH; }
  return [even(Math.round(w)), even(Math.round(h))];
}

// Bits a second for a picture of this size: a share of a bit per pixel the
// codec needs for a clean result, which AV1 needs least of.
const BPP = { mp4: 0.12, webm: 0.08, av1: 0.06 };

export function bitrate(fmt, w, h, fps, seconds, size, sound) {
  const best = Math.max(400_000, Math.round(w * h * fps * BPP[fmt]));
  if (size !== 'discord') return { rate: best, fits: true };
  const budget = Math.floor((DISCORD_BYTES * 8 * 0.92) / Math.max(0.1, seconds)) - (sound ? AUDIO_BITRATE : 0);
  return { rate: Math.min(best, budget), fits: budget >= 150_000 };
}

// H.264's level by frame size in macroblocks, so a big frame isn't refused
// for a level that is too low.
function avcLevel(w, h) {
  const mbs = Math.ceil(w / 16) * Math.ceil(h / 16);
  return mbs <= 3600 ? '1F' : mbs <= 8192 ? '28' : mbs <= 22080 ? '32' : '33';
}

// The codec strings tried for a format, best first: Baseline H.264 never
// reorders frames; VP8 is the fallback where VP9 can't encode.
function candidates(fmt, w, h) {
  const big = w * h > 1280 * 720;
  if (fmt === 'mp4') {
    const l = avcLevel(w, h);
    return [`avc1.42E0${l}`, `avc1.4D00${l}`, `avc1.6400${l}`].map((codec) => ({ codec, mux: 'avc1' }));
  }
  if (fmt === 'webm') {
    return [{ codec: big ? 'vp09.00.50.08' : 'vp09.00.40.08', mux: 'V_VP9' }, { codec: 'vp8', mux: 'V_VP8' }];
  }
  if (fmt === 'av1') return [{ codec: big ? 'av01.0.12M.08' : 'av01.0.08M.08', mux: 'av01' }];
  return [];
}

// The first codec string this browser encodes, per format, or nothing.
async function videoCodec(fmt, w, h, fps) {
  if (typeof VideoEncoder === 'undefined') return null;
  for (const c of candidates(fmt, w, h)) {
    const config = { codec: c.codec, width: w, height: h, framerate: fps, bitrate: 2_000_000 };
    if (c.mux === 'avc1') config.avc = { format: 'avc' };
    try {
      if ((await VideoEncoder.isConfigSupported(config)).supported) return c;
    } catch { /* not this one */ }
  }
  return null;
}

// The formats this browser can make of a clip this size, by id.
export async function available(width, height, fps) {
  const out = new Set(['gif', 'avi']);
  for (const fmt of ['mp4', 'webm', 'av1']) {
    const [w, h] = outSize(fmt, width, height, 1);
    if (await videoCodec(fmt, w, h, fps)) out.add(fmt);
  }
  return out;
}

async function canOpus(channels) {
  if (typeof AudioEncoder === 'undefined') return false;
  try {
    return (await AudioEncoder.isConfigSupported({ codec: 'opus', sampleRate: 48000, numberOfChannels: channels, bitrate: AUDIO_BITRATE })).supported;
  } catch { return false; }
}

const bytesOf = (d) => (ArrayBuffer.isView(d) ? new Uint8Array(d.buffer, d.byteOffset, d.byteLength) : new Uint8Array(d)).slice();

async function bitmap(url, w, h, signal) {
  const r = await fetch(url, { signal });
  if (!r.ok) throw new Error(`a frame could not be read (${r.status})`);
  return createImageBitmap(await r.blob(), { resizeWidth: w, resizeHeight: h, resizeQuality: 'high' });
}

function aborted(signal) {
  if (signal?.aborted) throw new DOMException('Cancelled', 'AbortError');
}

// An av1C record from the codec string, for an encoder that hands none: the
// sequence header is in every keyframe anyway, and players read these four
// bytes for the profile, level and 8-bit 4:2:0.
function av1C(codec) {
  const [, profile, lvl] = codec.match(/^av01\.(\d)\.(\d\d)/);
  return Uint8Array.from([0x81, (Number(profile) << 5) | Number(lvl), 0x0c, 0]);
}

// The clip's sound as Opus chunks, cut to the picture's length.
async function encodeSound(url, seconds, signal, onChunk) {
  const r = await fetch(url, { signal });
  if (!r.ok) return null;
  const decoded = await new OfflineAudioContext(2, 1, 48000).decodeAudioData(await r.arrayBuffer());
  const channels = Math.min(2, decoded.numberOfChannels);
  if (!(await canOpus(channels))) return null;
  let preSkip = 312;
  let failed = null;
  const enc = new AudioEncoder({
    output: (chunk, meta) => {
      const d = meta?.decoderConfig?.description;
      // OpusHead, little-endian: its pre-skip at byte 10.
      if (d) { const b = bytesOf(d); if (b.length >= 12) preSkip = b[10] | (b[11] << 8); }
      const data = new Uint8Array(chunk.byteLength);
      chunk.copyTo(data);
      onChunk(data, chunk.timestamp);
    },
    error: (e) => { failed = e; },
  });
  enc.configure({ codec: 'opus', sampleRate: 48000, numberOfChannels: channels, bitrate: AUDIO_BITRATE });
  const total = Math.min(decoded.length, Math.round(seconds * 48000));
  const step = 4800;
  for (let at = 0; at < total; at += step) {
    aborted(signal);
    const n = Math.min(step, total - at);
    const data = new Float32Array(n * channels);
    for (let c = 0; c < channels; c++) data.set(decoded.getChannelData(c).subarray(at, at + n), c * n);
    enc.encode(new AudioData({ format: 'f32-planar', sampleRate: 48000, numberOfFrames: n, numberOfChannels: channels, timestamp: Math.round((at * 1e6) / 48000), data }));
  }
  await enc.flush();
  enc.close();
  if (failed) throw failed;
  return { sampleRate: 48000, channels, preSkip };
}

// Encode the clip. `frameUrl(k)` for k < `frames`; `audioUrl` or null.
// Answers `{ blob, note }`; `onProgress(done, total)` as frames go in.
export async function exportClip({ fmt, frameUrl, frames, fps, width, height, audioUrl, scale = 1, size = 'best', signal, onProgress = () => {} }) {
  const [w, h] = outSize(fmt, width, height, scale);
  const seconds = frames / fps;
  const notes = [];
  if (fmt === 'gif') return { blob: new Blob([await gif(frameUrl, frames, fps, w, h, signal, onProgress)], { type: 'image/gif' }), note: '' };

  const codec = await videoCodec(fmt, w, h, fps);
  if (!codec) throw new Error(`this browser can't encode ${FORMATS.find((f) => f.id === fmt).label} at ${w}×${h}`);
  const { rate, fits } = bitrate(fmt, w, h, fps, seconds, size, !!audioUrl);
  if (!fits) notes.push(`${seconds.toFixed(0)} s is long for 10 MB: it will look soft — half size helps.`);

  const video = [];
  let description = null;
  let failed = null;
  const enc = new VideoEncoder({
    output: (chunk, meta) => {
      if (meta?.decoderConfig?.description && !description) description = bytesOf(meta.decoderConfig.description);
      const data = new Uint8Array(chunk.byteLength);
      chunk.copyTo(data);
      video.push({ data, ts: chunk.timestamp, key: chunk.type === 'key' });
    },
    error: (e) => { failed = e; },
  });
  const config = {
    codec: codec.codec, width: w, height: h, framerate: fps, bitrate: rate,
    bitrateMode: size === 'discord' ? 'constant' : 'variable', latencyMode: 'quality',
  };
  if (codec.mux === 'avc1') config.avc = { format: 'avc' };
  enc.configure(config);
  try {
    // A keyframe every two seconds: a player can seek, and a cluster or a
    // chunk of the file stays small.
    const gop = Math.max(1, Math.round(fps * 2));
    for (let k = 0; k < frames; k++) {
      aborted(signal);
      if (failed) throw failed;
      const bmp = await bitmap(frameUrl(k), w, h, signal);
      const vf = new VideoFrame(bmp, { timestamp: Math.round((k * 1e6) / fps), duration: Math.round(1e6 / fps) });
      enc.encode(vf, { keyFrame: k % gop === 0 });
      vf.close();
      bmp.close();
      // Don't run ahead of the encoder: frames waiting in it are whole bitmaps.
      while (enc.encodeQueueSize > 4) await new Promise((r) => setTimeout(r, 5));
      onProgress(k + 1, frames);
    }
    await enc.flush();
  } finally {
    if (enc.state !== 'closed') enc.close();
  }
  if (failed) throw failed;

  const sound = [];
  let audio = null;
  if (audioUrl) {
    audio = await encodeSound(audioUrl, seconds, signal, (data, ts) => sound.push({ data, ts }));
    if (!audio) notes.push('This browser can’t encode Opus, so the file has no sound — the AVI keeps it.');
  }
  if (codec.mux === 'av01' && !description) description = av1C(codec.codec);
  const mux = fmt === 'webm'
    ? webmMuxer({ video: { codec: codec.mux, width: w, height: h, description }, audio })
    : mp4Muxer({ video: { codec: codec.mux, width: w, height: h, fps, description }, audio });
  for (const c of video) mux.add('video', c.data, c.ts, c.key);
  if (audio) for (const c of sound) mux.add('audio', c.data, c.ts);
  const bytes = mux.finish();
  if (size === 'discord' && bytes.length > DISCORD_BYTES) {
    notes.push(`It came out at ${(bytes.length / 1e6).toFixed(1)} MB, over Discord's 10 — try half size.`);
  }
  return { blob: new Blob([bytes], { type: FORMATS.find((f) => f.id === fmt).type }), note: notes.join(' ') };
}

// Two passes over the frames: a palette from all of them, then the frames.
async function gif(frameUrl, frames, fps, w, h, signal, onProgress) {
  const cv = new OffscreenCanvas(w, h);
  const ctx = cv.getContext('2d', { willReadFrequently: true });
  const pixels = async (k) => {
    const bmp = await bitmap(frameUrl(k), w, h, signal);
    ctx.drawImage(bmp, 0, 0);
    bmp.close();
    return ctx.getImageData(0, 0, w, h).data;
  };
  const sampled = [];
  const every = Math.max(1, Math.floor(frames / 24));
  for (let k = 0; k < frames; k += every) {
    aborted(signal);
    sampled.push(await pixels(k));
  }
  const g = gifEncoder(w, h, fps, palette(histogram(sampled, 2)));
  for (let k = 0; k < frames; k++) {
    aborted(signal);
    g.add(await pixels(k));
    onProgress(k + 1, frames);
  }
  return g.finish();
}
