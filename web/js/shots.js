// ---- Which shots a clip already has ------------------------------------------
//
// The Shots panel's rows against the clip on the stage. Row 1 is shot 1; a
// row is *done* when the clip has that shot with that text, seed, length,
// cut, references, sounds and starting image (shot 1's is the clip's
// `source`), and what is said, heard and scored in it — computed every
// redraw, never stored, so changing any of them on a done row is what makes
// it, and every row after it, pending. Pure, for `shots.test.js`.
//
//   rows    [{text, seed, frames, cut, refs, audio, key, cast, lines, sound,
//           music, subjects, voices}], as the panel holds them: seed -1 for
//           random (which never matches a drawn shot), frames on the
//           family's grid
//   clip    the clip on the stage, or null; `segments[k]` is shot k as drawn
//   chains  whether its family can go on from a last frame
//   take    what its family takes, {references, sounds, narrated} (`sentRow`)
//
// Answers:
//   done      how many leading rows the clip has, unchanged
//   whole     every shot of the clip is still in the rows
//   pending   the non-empty rows after the done ones: what a press would draw
//   action    'extend'   the clip is whole and rows follow: draw them onto it
//             'redo'     row `done` (≥ 1) changed: keep the shots before it,
//                        draw it and the rows after — *Redo from shot k*
//             'generate' row 1 changed, or there is no clip: a new clip
//             'none'     the rows are the clip
//   dropLast  the row index whose × cuts the clip's last shot, or -1 — never
//             shot 1, which is the clip
const same = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);

// The fields a segment records beyond its words, as one comparable string;
// absent and empty alike. A continue's cast is left out: it starts from the
// last frame, so it has none (clips before that recorded one).
const words = (x, fresh = true) => JSON.stringify([
  fresh ? x.cast ?? [] : [],
  (x.lines ?? []).map((l) => [l.who ?? '', l.words ?? '', l.language ?? '', !!l.voiceover]),
  String(x.sound ?? '').trim(),
  String(x.music ?? '').trim(),
  (x.subjects ?? []).map((m) => [m.name ?? '', m.look ?? '']),
  x.voices ?? [],
]);

// What row `k` sends to a model that takes `take` ({references, sounds}, as
// `/video-models` says): H3 follows its references and sounds, and its image
// only with neither (sd.cpp refuses Ref2VA beside `-i`); Wan starts from its
// image. Only shot 1 and a cut start fresh, so only they take any. A row
// holds one or the other now (`castOnRow`, `session::shot_spec`); this is the
// guard for one saved before, which may hold both.
// Narrated stills (`take.narrated`): every row is a cut from its own picture
// with at most one sound, the voice; nothing else is read, and a shot's
// length is its speech's, so `frames` is null — never compared.
export const ALL = { references: true, sounds: true };
export function sentRow(r, k, take = ALL) {
  if (take.narrated) {
    return {
      ...r, frames: null, cut: k > 0, refs: [], audio: (r.audio ?? []).slice(0, 1), key: r.key ?? null,
      subjects: [], voices: [], cast: [], lines: [], sound: '', music: '',
    };
  }
  const fresh = k === 0 || !!r.cut;
  const refs = fresh && take.references ? r.refs ?? [] : [];
  const audio = fresh && take.sounds ? r.audio ?? [] : [];
  const said = take.sounds;
  return {
    ...r,
    refs,
    audio,
    key: fresh && !refs.length && !audio.length ? r.key ?? null : null,
    subjects: (r.subjects ?? []).slice(0, refs.length),
    voices: (r.voices ?? []).slice(0, audio.length),
    cast: said && fresh ? r.cast ?? [] : [],
    lines: said ? r.lines ?? [] : [],
    sound: said ? r.sound ?? '' : '',
    music: said ? r.music ?? '' : '',
  };
}

// `source` is the clip's first frame, which row 1's image is. `row` is what
// was sent (`sentRow`).
export function rowMatches(row, seg, k, source = null) {
  return String(row.text ?? '').trim() === String(seg.text ?? '').trim()
    && Number(row.seed) === Number(seg.seed)
    && (row.frames == null || Number(row.frames) === Number(seg.frames))
    // Shot 1 starts fresh whatever its row says.
    && (k === 0 || !!row.cut === !!seg.cut)
    && same(row.refs ?? [], seg.references ?? [])
    && same(row.audio ?? [], seg.audio ?? [])
    && words(row, k === 0 || !!row.cut) === words(seg, k === 0 || !!row.cut)
    // A continue's picture is the last frame, whatever the row holds.
    && (k === 0 ? (row.key || null) === (source || null) : !row.cut || (row.key || null) === (seg.keyframe ?? null));
}

export function shotState(rows, clip, chains, take = ALL) {
  const segs = clip?.segments ?? [];
  let done = 0;
  while (done < segs.length && done < rows.length && rowMatches(sentRow(rows[done], done, take), segs[done], done, clip.source)) done++;
  const whole = !!clip && done === segs.length;
  const pending = rows.slice(done).filter((r) => String(r.text ?? '').trim());
  let action;
  if (!clip || done === 0) action = 'generate';
  else if (!whole) action = chains ? 'redo' : 'generate';
  else action = chains && pending.length ? 'extend' : 'none';
  return {
    done,
    whole,
    pending,
    action,
    dropLast: whole && segs.length >= 2 ? segs.length - 1 : -1,
  };
}

// Whether a drawn shot no longer fits its Video: made at another model, size,
// frame rate or Hi-res than the Video's (`want`, as the Generation panel has
// it — the lock holds these until unlocked), or with other words than its
// scene's description and negative. Computed, never stored, so putting a
// field back clears the mark. A plan is never outdated: it is drawn with what
// is there when it is drawn.
export function shotOutdated(c, want, scene) {
  if (!c || c.rendered === false) return false;
  const v = c.video ?? {};
  if (want && (c.model !== want.model || Number(v.width) !== Number(want.width)
    || Number(v.height) !== Number(want.height) || Number(c.fps) !== Number(want.fps)
    || !!v.hires !== !!want.hires)) return true;
  if (scene) {
    if (String(c.every ?? '').trim() !== String(scene.every ?? '').trim()) return true;
    if ((c.typed_negative ?? c.negative ?? '') !== (scene.negative ?? '')) return true;
  }
  return false;
}
