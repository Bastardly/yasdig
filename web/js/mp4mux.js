// ---- MP4 (ISO BMFF), written whole -------------------------------------------
//
// The clip dialog's MP4 download: encoded chunks from the browser's own
// encoder (WebCodecs, `export.js`) laid into one plain, non-fragmented file —
// `ftyp`, `mdat` with every sample, then `moov` describing them, which is
// what Discord, phones and every player read. One sample per chunk, so the
// sample tables stay one line each. Pure, for `mp4mux.test.js`.
//
//   video  { codec: 'avc1' | 'av01', width, height, fps, description }
//          `description` is the encoder's decoder config: an avcC record for
//          H.264, an av1C record for AV1.
//   audio  { sampleRate, channels, preSkip } or null — Opus, in a `dOps`.
//
// `add(track, data, ptsUs, key)` takes chunks in decode order; `finish()`
// answers the file's bytes.

const enc = new TextEncoder();
const u8 = (...b) => Uint8Array.from(b);
const u16 = (v) => u8((v >> 8) & 255, v & 255);
const u32 = (v) => u8((v >>> 24) & 255, (v >>> 16) & 255, (v >>> 8) & 255, v & 255);
const i32 = (v) => u32(v >>> 0);
const zeros = (n) => new Uint8Array(n);
const fourcc = (s) => enc.encode(s);

function cat(parts) {
  let n = 0;
  for (const p of parts) n += p.length;
  const out = new Uint8Array(n);
  let at = 0;
  for (const p of parts) { out.set(p, at); at += p.length; }
  return out;
}

function box(type, ...parts) {
  const body = cat(parts);
  return cat([u32(body.length + 8), fourcc(type), body]);
}

// A full box: version and 24 bits of flags first.
const full = (type, version, flags, ...parts) => box(type, u8(version), u8((flags >> 16) & 255, (flags >> 8) & 255, flags & 255), ...parts);

// The identity matrix, 16.16 and 2.30 fixed point.
const MATRIX = cat([u32(0x10000), u32(0), u32(0), u32(0), u32(0x10000), u32(0), u32(0), u32(0), u32(0x40000000)]);

// Equal neighbours folded into [count, value] runs: `stts` and `ctts`.
function runs(values) {
  const out = [];
  for (const v of values) {
    if (out.length && out.at(-1)[1] === v) out.at(-1)[0]++;
    else out.push([1, v]);
  }
  return out;
}

export function mp4Muxer({ video, audio = null }) {
  const tracks = [{ kind: 'video', samples: [] }];
  if (audio) tracks.push({ kind: 'audio', samples: [] });
  // Video counts in frames, audio in samples: every duration is exact.
  tracks[0].timescale = video.fps;
  if (audio) tracks[1].timescale = 48000;

  function add(kind, data, ptsUs, key = true) {
    const t = tracks.find((x) => x.kind === kind);
    t.samples.push({ data, pts: Math.round((ptsUs * t.timescale) / 1e6), key });
  }

  function sampleEntry(t) {
    if (t.kind === 'video') {
      const config = video.codec === 'av01' ? box('av1C', video.description) : box('avcC', video.description);
      const name = zeros(32);
      return box(video.codec, zeros(6), u16(1), zeros(16), u16(video.width), u16(video.height),
        u32(0x00480000), u32(0x00480000), zeros(4), u16(1), name, u16(0x18), u16(0xffff), config);
    }
    // `dOps` is big-endian, unlike the OpusHead it is made from.
    const dops = box('dOps', u8(0, audio.channels), u16(audio.preSkip ?? 312), u32(audio.sampleRate), u16(0), u8(0));
    return box('Opus', zeros(6), u16(1), zeros(8), u16(audio.channels), u16(16), zeros(4),
      u32(48000 * 0x10000), dops);
  }

  // Decode times are the presentation times in order; an encoder that
  // reorders (B-frames) leaves an offset, written signed (`ctts` v1).
  function timing(t) {
    const n = t.samples.length;
    const sorted = t.samples.map((s) => s.pts).sort((a, b) => a - b);
    const dts = sorted;
    const dur = [];
    for (let k = 0; k < n; k++) {
      const next = k + 1 < n ? dts[k + 1] : null;
      dur.push(next == null ? (dur.at(-1) ?? 1) : Math.max(0, next - dts[k]));
    }
    const offsets = t.samples.map((s, k) => s.pts - dts[k]);
    return { dts, dur, offsets, end: n ? dts[n - 1] + dur[n - 1] - dts[0] : 0 };
  }

  function trak(t, id, offsets, movieDuration) {
    const { dur, offsets: cts, end } = timing(t);
    const isVideo = t.kind === 'video';
    const tkhd = full('tkhd', 0, 3, u32(0), u32(0), u32(id), u32(0), u32(movieDuration), zeros(8),
      u16(0), u16(0), u16(isVideo ? 0 : 0x0100), u16(0), MATRIX,
      u32(isVideo ? video.width * 0x10000 : 0), u32(isVideo ? video.height * 0x10000 : 0));
    // Language 'und', packed as three 5-bit letters.
    const mdhd = full('mdhd', 0, 0, u32(0), u32(0), u32(t.timescale), u32(end), u16(0x55c4), u16(0));
    const hdlr = full('hdlr', 0, 0, u32(0), fourcc(isVideo ? 'vide' : 'soun'), zeros(12),
      enc.encode(isVideo ? 'yasdig video\0' : 'yasdig sound\0'));
    const head = isVideo ? full('vmhd', 0, 1, zeros(8)) : full('smhd', 0, 0, zeros(4));
    const dinf = box('dinf', full('dref', 0, 0, u32(1), full('url ', 0, 1)));
    const n = t.samples.length;
    const stts = runs(dur);
    const parts = [
      full('stsd', 0, 0, u32(1), sampleEntry(t)),
      full('stts', 0, 0, u32(stts.length), ...stts.flatMap(([c, d]) => [u32(c), u32(d)])),
    ];
    if (cts.some((o) => o !== 0)) {
      const r = runs(cts);
      parts.push(full('ctts', 1, 0, u32(r.length), ...r.flatMap(([c, o]) => [u32(c), i32(o)])));
    }
    if (isVideo) {
      const keys = t.samples.map((s, k) => (s.key ? k + 1 : 0)).filter(Boolean);
      parts.push(full('stss', 0, 0, u32(keys.length), ...keys.map(u32)));
    }
    parts.push(
      full('stsc', 0, 0, u32(1), u32(1), u32(1), u32(1)),
      full('stsz', 0, 0, u32(0), u32(n), ...t.samples.map((s) => u32(s.data.length))),
      full('stco', 0, 0, u32(n), ...offsets.map(u32)),
    );
    return box('trak', tkhd, box('mdia', mdhd, hdlr, box('minf', head, dinf, box('stbl', ...parts))));
  }

  function finish() {
    const ftyp = box('ftyp', fourcc('isom'), u32(0x200), fourcc('isom'), fourcc('iso2'),
      fourcc(video.codec === 'av01' ? 'av01' : 'avc1'), fourcc('mp41'));
    // Samples in `mdat`, video first: one chunk each, so a chunk offset per
    // sample — known once `ftyp` and the `mdat` header are.
    let at = ftyp.length + 8;
    const offsets = tracks.map((t) => t.samples.map((s) => { const o = at; at += s.data.length; return o; }));
    const mdat = cat([u32(at - ftyp.length), fourcc('mdat'), ...tracks.flatMap((t) => t.samples.map((s) => s.data))]);
    // The movie in milliseconds: the longest track.
    const ms = Math.max(...tracks.map((t) => Math.round((timing(t).end * 1000) / t.timescale)));
    const mvhd = full('mvhd', 0, 0, u32(0), u32(0), u32(1000), u32(ms), u32(0x10000), u16(0x0100),
      zeros(10), MATRIX, zeros(24), u32(tracks.length + 1));
    const moov = box('moov', mvhd, ...tracks.map((t, i) => trak(t, i + 1, offsets[i], ms)));
    return cat([ftyp, mdat, moov]);
  }

  return { add, finish };
}
