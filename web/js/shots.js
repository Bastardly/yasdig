// ---- Which shots a clip already has ------------------------------------------
//
// The Shots panel's rows against the clip on the stage. Row 1 is shot 1; a
// row is *done* when the clip has that shot with that text, seed, length, and
// cut, references and starting picture — computed every redraw, never stored, so changing any
// of them on a done row is what makes it, and every row after it, pending.
// Pure, for `shots.test.js`.
//
//   rows    [{text, seed, frames, cut, refs, key}], as the panel holds them: seed
//           -1 for random (which never matches a drawn shot), frames on the
//           family's grid
//   clip    the clip on the stage, or null; `segments[k]` is shot k as drawn
//   chains  whether its family can go on from a last frame
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

export function rowMatches(row, seg, k) {
  return String(row.text ?? '').trim() === String(seg.text ?? '').trim()
    && Number(row.seed) === Number(seg.seed)
    && Number(row.frames) === Number(seg.frames)
    // Shot 1 starts fresh whatever its row says.
    && (k === 0 || !!row.cut === !!seg.cut)
    && same(row.refs ?? [], seg.references ?? [])
    // A continue's picture is the last frame, whatever the row holds.
    && (k === 0 || !row.cut || (row.key || null) === (seg.keyframe ?? null));
}

export function shotState(rows, clip, chains) {
  const segs = clip?.segments ?? [];
  let done = 0;
  while (done < segs.length && done < rows.length && rowMatches(rows[done], segs[done], done)) done++;
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
