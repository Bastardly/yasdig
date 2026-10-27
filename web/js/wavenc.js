// A sound as the server keeps it: 16-bit PCM WAV, the one format
// `avi::parse_wav` reads — the page decodes whatever the browser can play
// and writes this, as it re-encodes pictures to PNG. Pure, so
// `wavenc.test.js` reads one back.
//
// The limits are MiniMax H3's for a reference sound (its model card): each
// clip 2 to 15 s, which `Store::import_audio` checks again.

export const SOUND_RATE = 32000;
export const MIN_SOUND_S = 2;
export const MAX_SOUND_S = 15;

// What becomes of a decoded sound `seconds` long: refused when too short,
// else kept up to the limit.
export function fitSound(seconds) {
  if (!(seconds >= MIN_SOUND_S)) {
    return { error: `a sound for H3 is at least ${MIN_SOUND_S} seconds; this one is ${(seconds || 0).toFixed(1)} s` };
  }
  return { seconds: Math.min(seconds, MAX_SOUND_S), trimmed: seconds > MAX_SOUND_S };
}

// `channels`: one Float32Array per channel, -1..1, all the same length.
export function encodeWav(channels, rate) {
  const n = channels[0]?.length ?? 0;
  const ch = channels.length;
  const data = n * ch * 2;
  const b = new DataView(new ArrayBuffer(44 + data));
  const tag = (at, s) => { for (let i = 0; i < 4; i++) b.setUint8(at + i, s.charCodeAt(i)); };
  tag(0, 'RIFF');
  b.setUint32(4, 36 + data, true);
  tag(8, 'WAVE');
  tag(12, 'fmt ');
  b.setUint32(16, 16, true);
  b.setUint16(20, 1, true); // PCM
  b.setUint16(22, ch, true);
  b.setUint32(24, rate, true);
  b.setUint32(28, rate * ch * 2, true);
  b.setUint16(32, ch * 2, true);
  b.setUint16(34, 16, true);
  tag(36, 'data');
  b.setUint32(40, data, true);
  let at = 44;
  for (let i = 0; i < n; i++) {
    for (let c = 0; c < ch; c++) {
      const v = Math.max(-1, Math.min(1, channels[c][i]));
      b.setInt16(at, v < 0 ? Math.round(v * 0x8000) : Math.round(v * 0x7fff), true);
      at += 2;
    }
  }
  return new Uint8Array(b.buffer);
}
