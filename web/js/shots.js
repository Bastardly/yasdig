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
  (x.looks ?? []).map((m) => [m.name ?? '', m.look ?? '']),
]);

// What row `k` sends to a model that takes `take` ({references, sounds}, as
// `/video-models` says): H3 follows its references and sounds, and its image
// only with neither (sd.cpp refuses Ref2VA beside `-i`); Wan starts from its
// image. Only shot 1 and a cut start fresh, so only they take any. A row
// holds one or the other now (`castOnRow`, `session::shot_spec`); this is the
// guard for one saved before, which may hold both.
// Narrated stills (`take.narrated`): a shot is one picture, part 1's, and
// every part is read over it with at most one sound, the voice; nothing else
// is read, and a part's length is its speech's, so `frames` is null —
// never compared.
export const ALL = { references: true, sounds: true, looks: true };
export function sentRow(r, k, take = ALL) {
  if (take.narrated) {
    return {
      ...r, frames: null, cut: false, refs: [], audio: (r.audio ?? []).slice(0, 1), key: k === 0 ? r.key ?? null : null,
      subjects: [], voices: [], cast: [], lines: [], sound: '', music: '', looks: [],
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
    // The looks that lead the part's prompt (LTX 2.5), on any part that took
    // them: a continue's words name the shot's people as part 1's do.
    looks: take.looks ? r.looks ?? [] : [],
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
// it — a change to these asks first), or with other words than its
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

// What Enhance shows part `k` of shot `s` of what is around it, from the
// storyboard: `order` is every shot in storyboard order, `{scene, texts}`
// (each part's words, '' for an empty one). Its scene's opening part, this
// shot's first part, and the part before — inside the shot that one goes on
// from its last frame; a shot's part 1 cuts from the last words of the shot
// before, in this scene or the one before it. The fields of
// `agent::enhance::Request`.
export function partNeighbours(order, s, k) {
  const at = order[s];
  if (!at) return { first: '', scene: '', before: '', cut: false, then_continues: false, next: '' };
  const t = (x) => String(x ?? '').trim();
  const opener = order.find((o) => o.scene === at.scene) ?? at;
  const first = opener === at && k === 0 ? '' : t(opener.texts.find((x) => t(x)));
  let before = '';
  if (k > 0) before = t(at.texts[k - 1]);
  else {
    for (let j = s - 1; j >= 0 && !before; j--) before = t([...order[j].texts].reverse().find((x) => t(x)));
  }
  const thenContinues = !!t(at.texts[k + 1]);
  // The first words of the next shot in this scene, for a part that ends its
  // shot: a new scene is another place, which this one doesn't lead to.
  let next = '';
  for (let j = s + 1; !thenContinues && !next && j < order.length && order[j].scene === at.scene; j++) {
    next = t(order[j].texts.find((x) => t(x)));
  }
  return {
    first,
    scene: k > 0 ? t(at.texts[0]) : '',
    before,
    cut: k === 0 && !!before,
    then_continues: thenContinues,
    next,
  };
}

// The flow shift a shot is drawn with when it has its own: the draft's
// while one is open (`shift` beside its rows), else what the record kept.
export const ownShift = (c, draft) => (draft && 'shift' in draft ? draft.shift : c?.flow_shift_own) ?? null;

// A drawn shot whose own flow shift changed is drawn again whole: Extend
// and Redo run on the shot's recorded settings, so they can't change it.
// The Video's default moving changes nothing here — it isn't the lock.
export function withShift(st, own, c) {
  if (!c || c.rendered === false) return st;
  const n = (x) => (x == null || x === '' ? null : Number(x));
  if (n(own) === n(c.flow_shift_own)) return st;
  return { ...st, action: 'generate', shift: true };
}

// How long the prompt a part is sent is, in characters (code points, as
// Rust counts them): the scene's description and the part's words joined
// as `videos::shot_prompt` joins them. What `features.prompt_chars` limits.
export function promptChars(every, text, looks = []) {
  const e = (every ?? '').trim();
  const t = (text ?? '').trim();
  const body = [...(e && t ? `${e}. ${t}` : e || t)].length;
  // `cast::look_paragraph` + `compose`: each look a paragraph ahead, "Name: "
  // when it doesn't start with the name.
  return looks.filter((m) => (m.look ?? '').trim()).reduce((n, m) => {
    const look = m.look.trim();
    const named = look.toLowerCase().startsWith((m.name ?? '').trim().toLowerCase());
    return n + [...look].length + (named ? 0 : [...(m.name ?? '').trim()].length + 2) + 2;
  }, body);
}

// Pure: whether an edit of the locked things asks before it goes on — only
// one the person made (a trusted event, or `<prompt-box>`'s weighting), on
// something a generated shot was made with. Programmatic writes never ask.
export function asksFirst({ user, locked }) {
  return !!user && !!locked;
}
