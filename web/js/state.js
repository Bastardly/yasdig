import { $ } from './dom.js';
import { showError } from './memory.js';

// ---------------------------------------------------------------------------
// Two tabs put an image on screen, and everything around it — the action row,
// Save, the viewer — is the same machinery pointed at a different pair of
// elements. `S.pane` says which.
//
// The state stays single on purpose. Only one tab is visible at a time, so
// there is only ever one live `imageId` to be acting on; two copies would mean
// two ways to be stale. (The server keeps one per tab — `pipeline::Slot` — and
// the Image tab's comes back with it: see `restoreImage`.) What that costs is that switching
// tabs has to *clear* the pane being left (see showTab), or a picture would sit
// there looking live while `imageId` addressed a different one.
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
  // Which of the image panes is showing: `image` or `outputs`.
  pane: 'image',
  // The agent's note on the work item last reopened (`{ id, chat, title, round,
  // reason }`), shown in Info while that item's picture is up.
  agentTrace: null,
};

export const PANES = {
  // `result` is the stage the picture sits on, `info` the Info panel.
  image:   { result: 'result',   info: 'resultinfo',   err: 'err',
             refmodel: 'refmodel',   hiresmodel: 'hiresmodel' },
  // The second row has none of the first row's other three keys. It has no
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
  const info = $(PANES[which].info);
  if (info) info.innerHTML = '<p class="sub">Nothing generated yet.</p>';
  if ($('lightbox').open) $('lightbox').close();
}

// Where a failure goes: the pane's own error line (the Image tab's status
// card). In this module rather than in image.js because it is pane logic — the
// viewer's sliders report through it too.
// `oom` and `opts` are an out-of-memory failure's, for `memory.js`'s box.
export const escErr = (msg, oom = null, opts = {}) =>
  showError(P('err'), msg, oom, opts);
