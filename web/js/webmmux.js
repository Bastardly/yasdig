// ---- WebM (Matroska), written whole ------------------------------------------
//
// The clip dialog's WebM download: VP9 or AV1 video, Opus sound, from the
// browser's own encoder (`export.js`). One EBML header and one Segment of
// known size — SeekHead, Info, Tracks, a Cluster per video keyframe, Cues —
// since the whole clip is in memory anyway. Times are milliseconds
// (TimecodeScale 1 000 000 ns). Pure, for `webmmux.test.js`.
//
//   video  { codec: 'V_VP9' | 'V_AV1', width, height, description? }
//          AV1 needs its av1C record as CodecPrivate; VP9 has none.
//   audio  { sampleRate, channels, preSkip } or null — Opus.
//
// `add(track, data, ptsUs, key)` takes chunks in any order per track;
// `finish()` interleaves them by time and answers the file's bytes.

const enc = new TextEncoder();

function cat(parts) {
  let n = 0;
  for (const p of parts) n += p.length;
  const out = new Uint8Array(n);
  let at = 0;
  for (const p of parts) { out.set(p, at); at += p.length; }
  return out;
}

// An element size: the shortest vint that holds it (all ones is "unknown").
function vsize(n) {
  for (let len = 1; len <= 8; len++) {
    if (n < 2 ** (7 * len) - 1) {
      const out = new Uint8Array(len);
      let v = n;
      for (let i = len - 1; i >= 0; i--) { out[i] = v % 256; v = Math.floor(v / 256); }
      out[0] |= 1 << (8 - len);
      return out;
    }
  }
  throw new Error('element too large');
}

function uint(v, width = 0) {
  const bytes = [];
  let x = v;
  do { bytes.unshift(x % 256); x = Math.floor(x / 256); } while (x > 0);
  while (bytes.length < width) bytes.unshift(0);
  return Uint8Array.from(bytes);
}

function float(v) {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setFloat64(0, v);
  return b;
}

// An element: its id's own bytes (they carry their length), size, data.
function el(id, ...parts) {
  const body = cat(parts.map((p) => (typeof p === 'string' ? enc.encode(p) : p)));
  const idb = uint(id);
  return cat([idb, vsize(body.length), body]);
}

const ID = {
  EBML: 0x1a45dfa3, EBMLVersion: 0x4286, EBMLReadVersion: 0x42f7, EBMLMaxIDLength: 0x42f2,
  EBMLMaxSizeLength: 0x42f3, DocType: 0x4282, DocTypeVersion: 0x4287, DocTypeReadVersion: 0x4285,
  Segment: 0x18538067, SeekHead: 0x114d9b74, Seek: 0x4dbb, SeekID: 0x53ab, SeekPosition: 0x53ac,
  Info: 0x1549a966, TimecodeScale: 0x2ad7b1, Duration: 0x4489, MuxingApp: 0x4d80, WritingApp: 0x5741,
  Tracks: 0x1654ae6b, TrackEntry: 0xae, TrackNumber: 0xd7, TrackUID: 0x73c5, TrackType: 0x83,
  CodecID: 0x86, CodecPrivate: 0x63a2, CodecDelay: 0x56aa, SeekPreRoll: 0x56bb,
  Video: 0xe0, PixelWidth: 0xb0, PixelHeight: 0xba, Audio: 0xe1, SamplingFrequency: 0xb5, Channels: 0x9f,
  Cluster: 0x1f43b675, Timecode: 0xe7, SimpleBlock: 0xa3,
  Cues: 0x1c53bb6b, CuePoint: 0xbb, CueTime: 0xb3, CueTrackPositions: 0xb7, CueTrack: 0xf7,
  CueClusterPosition: 0xf1,
};

// Opus's own header, as Matroska's CodecPrivate carries it (little-endian).
export function opusHead(channels, sampleRate, preSkip = 312) {
  const b = new Uint8Array(19);
  b.set(enc.encode('OpusHead'));
  const v = new DataView(b.buffer);
  v.setUint8(8, 1);
  v.setUint8(9, channels);
  v.setUint16(10, preSkip, true);
  v.setUint32(12, sampleRate, true);
  return b;
}

export function webmMuxer({ video, audio = null }) {
  const blocks = []; // { track, ms, key, data }

  function add(kind, data, ptsUs, key = true) {
    blocks.push({ track: kind === 'video' ? 1 : 2, ms: Math.round(ptsUs / 1000), key: kind === 'audio' || key, data });
  }

  function simpleBlock(b, clusterMs) {
    const head = new Uint8Array(4);
    head[0] = 0x80 | b.track;
    new DataView(head.buffer).setInt16(1, b.ms - clusterMs);
    head[3] = b.key ? 0x80 : 0;
    return el(ID.SimpleBlock, head, b.data);
  }

  function finish() {
    blocks.sort((a, b) => a.ms - b.ms || a.track - b.track);
    const durationMs = blocks.at(-1)?.ms ?? 0;
    const header = el(ID.EBML, el(ID.EBMLVersion, uint(1)), el(ID.EBMLReadVersion, uint(1)),
      el(ID.EBMLMaxIDLength, uint(4)), el(ID.EBMLMaxSizeLength, uint(8)), el(ID.DocType, 'webm'),
      el(ID.DocTypeVersion, uint(4)), el(ID.DocTypeReadVersion, uint(2)));
    const info = el(ID.Info, el(ID.TimecodeScale, uint(1e6)), el(ID.MuxingApp, 'yasdig'),
      el(ID.WritingApp, 'yasdig'), el(ID.Duration, float(durationMs + frameMs())));
    const vtrack = el(ID.TrackEntry, el(ID.TrackNumber, uint(1)), el(ID.TrackUID, uint(1)),
      el(ID.TrackType, uint(1)), el(ID.CodecID, video.codec),
      ...(video.description ? [el(ID.CodecPrivate, video.description)] : []),
      el(ID.Video, el(ID.PixelWidth, uint(video.width)), el(ID.PixelHeight, uint(video.height))));
    const tracks = [vtrack];
    if (audio) {
      const preSkip = audio.preSkip ?? 312;
      tracks.push(el(ID.TrackEntry, el(ID.TrackNumber, uint(2)), el(ID.TrackUID, uint(2)),
        el(ID.TrackType, uint(2)), el(ID.CodecID, 'A_OPUS'),
        el(ID.CodecPrivate, opusHead(audio.channels, audio.sampleRate, preSkip)),
        el(ID.CodecDelay, uint(Math.round((preSkip / 48000) * 1e9))), el(ID.SeekPreRoll, uint(80e6)),
        el(ID.Audio, el(ID.SamplingFrequency, float(48000)), el(ID.Channels, uint(audio.channels)))));
    }
    const tracksEl = el(ID.Tracks, ...tracks);

    // A cluster starts at every video keyframe, and before a block's offset
    // from its cluster would outgrow the signed 16 bits it is written in.
    const clusters = [];
    let cur = null;
    for (const b of blocks) {
      if (!cur || (b.track === 1 && b.key) || b.ms - cur.ms > 30000) {
        cur = { ms: b.ms, blocks: [], key: b.track === 1 && b.key };
        clusters.push(cur);
      }
      cur.blocks.push(b);
    }
    const clusterEls = clusters.map((c) => el(ID.Cluster, el(ID.Timecode, uint(c.ms)), ...c.blocks.map((b) => simpleBlock(b, c.ms))));

    // Positions are from the Segment's data. The SeekHead's own size is
    // fixed — every position is written in 8 bytes — so it can be laid out
    // before the positions it holds are known.
    const seek = (id, pos) => el(ID.Seek, el(ID.SeekID, uint(id)), el(ID.SeekPosition, uint(pos, 8)));
    const seekHead = (p) => el(ID.SeekHead, seek(ID.Info, p.info), seek(ID.Tracks, p.tracks), seek(ID.Cues, p.cues));
    const headLen = seekHead({ info: 0, tracks: 0, cues: 0 }).length;
    const pos = { info: headLen };
    pos.tracks = pos.info + info.length;
    let at = pos.tracks + tracksEl.length;
    const clusterAt = clusterEls.map((c) => { const p = at; at += c.length; return p; });
    pos.cues = at;
    const cues = el(ID.Cues, ...clusters.map((c, i) => (c.key ? el(ID.CuePoint, el(ID.CueTime, uint(c.ms)),
      el(ID.CueTrackPositions, el(ID.CueTrack, uint(1)), el(ID.CueClusterPosition, uint(clusterAt[i])))) : null))
      .filter(Boolean));
    const segment = el(ID.Segment, seekHead(pos), info, tracksEl, ...clusterEls, cues);
    return cat([header, segment]);
  }

  // One video frame's time, so Duration covers the last frame shown.
  function frameMs() {
    const v = blocks.filter((b) => b.track === 1);
    return v.length > 1 ? (v.at(-1).ms - v[0].ms) / (v.length - 1) : 0;
  }

  return { add, finish };
}
