import { $ } from './dom.js';

// ---------------------------------------------------------------------------
// Two tabs put an image on screen, and everything around it — the action row,
// Save, the viewer — is the same machinery pointed at a different pair of
// elements. `S.pane` says which.
//
// The state stays single on purpose. Only one tab is visible at a time and the
// GPU worker is serialized, so there is only ever one live `imageId` to have;
// two copies would mean two ways to be stale. What that costs is that switching
// tabs has to *clear* the pane being left (see showTab), or a picture would sit
// there looking live while `imageId` addressed a different one.
//
// It also means the ids render() writes — refine, hires, saveout, upfactor —
// exist in at most one pane at a time, which is why IDLE_LABELS and setUpFactor
// need no second set of them.
//
// The sliders and their timing readout are *not* in here, and that is what moving
// them into the viewer bought: there is one dialog, so there is one pair of
// sliders, addressed by plain id. A pane is now only the three things that really
// are per-tab — where the result block goes, where an error goes, and which
// escalation-model select to read.
//
// One object rather than six exported `let`s, because an imported binding is
// read-only at the importer: `imageId = null` in another module is a TypeError,
// not an assignment. Nothing about the single-state rule changes — this is only
// how it is spelled now there is more than one file to spell it in.
// ---------------------------------------------------------------------------
export const S = {
  // Server-side handle for the image currently on screen, used to escalate its
  // quality; null before anything is generated, or once the handle goes stale.
  imageId: null,
  // The data: URL on screen. Kept out of render()'s closure so the post-process
  // sliders can swap the image without rebuilding the result block.
  currentSrc: null,
  // The archive entry the image on screen came from, so the grid can mark it.
  workId: null,
  // The hi-res upscale factor. Module state rather than whatever the select
  // happens to say, because render() rebuilds the whole .actions row on every
  // generate, refine and hi-res — so a value living only in the DOM is reset by
  // the very actions you'd want it to survive.
  upFactor: 2,
  // A run in flight — a batch, or the loop — else null. `total` is Infinity in
  // loop mode, `stop` is set by the Stop button and read between images. It is
  // also the flag everything else reads to know the GPU is spoken for.
  run: null,
  // Which of the three image tabs is showing.
  pane: 'image',
};

export const PANES = {
  image:   { result: 'result',   err: 'err',   escmodel: 'escmodel' },
  img2img: { result: 'i2result', err: 'i2err', escmodel: 'i2escmodel' },
  // Three tabs now, and the third has neither of the other two keys. It has no
  // result block because its image only ever exists in the viewer, and no
  // escalation select because it deliberately offers no escalation — an output
  // is already-filtered pixels, and diffusing over those is invariant 1. The two
  // places that read `result` reach it with `?.` for exactly this row.
  outputs: { err: 'oerr' },
};
export const P = (k) => $(PANES[S.pane][k]);

// Empty a pane and forget the image it was showing.
//
// Called when a tab is left, and it is not housekeeping: `imageId` is single, so
// the picture in the pane you walked away from would otherwise keep sitting
// there beside Refine and Hi-res buttons that now address a different image. The
// viewer goes with it for the same reason — it shows the image just forgotten.
export function clearPane(which) {
  const result = $(PANES[which].result);
  if (result) result.innerHTML = '';
  if ($('lightbox').open) $('lightbox').close();
}

// Where a failure goes. `#err` lives in the left control column, which on a wide
// screen is nowhere near the result, so an escalation reports beside the buttons
// that caused it: `#rerr` is part of render()'s output, and render() only runs on
// success, so the slot from the last successful render is still there to write
// into. It is in this module rather than in image.js because it is pane logic —
// the viewer's sliders report through it too.
export const escErr = (msg) => { ($('rerr') || P('err')).textContent = msg; };
