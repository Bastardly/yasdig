// ---- Which shots a clip already has ------------------------------------------
//
// The Prompts panel's rows against the clip on the stage. The Prompt is shot
// 1 and each row below a later shot; a row is *done* when the clip has that
// shot with that text — computed every redraw, never stored, so editing a
// done row is what makes it pending again. Pure, for `shots.test.js`.
//
//   texts   [Prompt, ...rows], as typed
//   clip    the clip on the stage, or null; `segments[k].text` is shot k as typed
//   chains  whether its family can go on from a last frame
//
// Answers:
//   done       how many leading rows the clip has, unchanged
//   whole      every shot of the clip is still in the rows — Extend's condition,
//              since a clip is only ever extended at its end
//   pending    the non-empty rows after the done ones: what Extend would draw
//   canExtend  there is a clip, it chains, it is whole, and something is pending
//   dropLast   the row index whose × cuts the clip's last shot, or -1 — never
//              shot 1, which is the clip
export function shotState(texts, clip, chains) {
  const t = texts.map((x) => String(x ?? '').trim());
  const segs = clip?.segments ?? [];
  let done = 0;
  while (done < segs.length && done < t.length && t[done] === String(segs[done].text ?? '').trim()) done++;
  const whole = !!clip && done === segs.length;
  const pending = t.slice(done).filter(Boolean);
  return {
    done,
    whole,
    pending,
    canExtend: whole && !!chains && pending.length > 0,
    dropLast: whole && segs.length >= 2 ? segs.length - 1 : -1,
  };
}
