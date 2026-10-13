import { $, post } from './dom.js';
import { S, P, escErr } from './state.js';

// The viewer's panel, top to bottom. This list is the only place a filter is
// declared: the markup, the labels, setPost, the /adjust body and the
// input listeners all read it, so adding one is a row here and a field on the
// server's `Filters`. The names are the wire names — that is what lets the body
// and the response be built by looping rather than by naming ten keys twice.
//
// Only the last two are per-model; the eight grading knobs always start at 0,
// because a checkpoint has a house sharpness and no opinion about your hue.
// `help` is the tooltip text, and it lives here rather than in `HELP` below for
// the reason the rest of the row does: a slider is declared in one place, and
// number eleven should be one line in one list.
export const FILTERS = [
  { group: 'Tone', id: 'lightness', label: 'Lightness',
    help: 'Overall brightness, applied as an exposure change in stops rather than '
        + 'as a flat addition — so it lifts the picture without flattening the '
        + 'darkest and brightest ends into each other.' },
  { id: 'contrast', label: 'Contrast',
    help: 'Pushes tones away from middle grey, or towards it going negative. It '
        + 'pivots around mid-grey, so the picture does not get brighter or darker '
        + 'overall, only more or less separated.' },
  { id: 'highlights', label: 'Highlights',
    help: 'The bright end only. Negative recovers a blown sky; positive lifts it '
        + 'further. Mid-tones and shadows are left alone.' },
  { id: 'shadows', label: 'Shadows',
    help: 'The dark end only. Positive opens up what is hidden down there without '
        + 'touching the highlights — which is the difference between this and '
        + 'Lightness.' },
  { group: 'Colour', id: 'saturation', label: 'Saturation',
    help: 'How colourful everything is. −1 is exactly greyscale. Small positive '
        + 'amounts go a long way, since every colour moves at once.' },
  { id: 'temperature', label: 'Temperature',
    help: 'Warm (yellow) against cool (blue) — the usual white-balance knob. The '
        + 'shift is scaled by how bright a pixel is, so black stays black instead '
        + 'of picking up a cast.' },
  { id: 'tint', label: 'Tint',
    help: 'Green against magenta, the other white-balance axis. It is what '
        + 'neutralises a cast that Temperature alone cannot, which is most of the '
        + 'casts that come off a screen or a fluorescent tube.' },
  // Degrees, because 0.25 is not a number anyone has an opinion about and 45°
  // is. The wire value stays -1..1 like every other knob.
  { id: 'hue', label: 'Hue', fmt: (v) => `${Math.round(v * 180)}°`,
    help: 'Rotates every colour around the wheel. A few degrees is a correction; '
        + 'large angles are an effect, and ±180° arrives at the same place from '
        + 'both directions.' },
  { group: 'Detail', id: 'clarity', label: 'Clarity',
    help: 'Local contrast over a wide band: it adds depth and punch rather than '
        + 'edge sharpness, and negative softens skin and noise. The band is sized '
        + 'from the image, so a setting survives a hi-res pass.' },
  // The one unsigned knob: RCAS has no direction to run backwards in.
  { id: 'sharpness', label: 'Sharpness', min: 0,
    help: 'FSR contrast-adaptive sharpening, which sharpens edges without ringing '
        + 'them. 1.0 is the ceiling rather than a soft limit — past it the filter '
        + 'would invert instead of sharpening harder. It starts at the model’s own '
        + 'value, not at zero.' },
];

// Where the sliders *start*: pre-filled from the model when one is selected, then
// re-set from the server's reply, which is authoritative since it says what the
// image on screen actually has. Called whether or not the viewer is open — the
// controls exist the whole time, they are just off-screen until it is.
//
// A missing key is 0, not "leave it": the two callers both describe a *whole*
// state — a model's start points, or what the server has applied — and a knob
// either of them omits is a knob that is off.
export function setPost(filters) {
  for (const f of FILTERS) $(f.id).value = filters[f.id] ?? 0;
  syncPostLabels();
}

export function syncPostLabels() {
  for (const f of FILTERS) {
    const v = Number($(f.id).value);
    $(f.id + 'val').textContent = f.fmt ? f.fmt(v) : v.toFixed(2);
  }
}

// The same image is on screen in two places whenever the viewer is open, and the
// one the eye is on is the big one. Swapping `src` in place rather than
// rebuilding the block keeps the result column's layout height, so a drag doesn't
// jump the page — and in the viewer it keeps the scroll offset, since a
// re-filtered image is the same size as the one it replaces. That matters: you
// pan to a detail, then drag Sharpness, and the detail stays under the cursor.
export function setResultImage(src) {
  // This path is a re-filter, so it never re-centres — and it has to say so
  // rather than rely on nothing being pending. Re-opening the viewer on the src
  // already loaded fires no load event, which leaves a centring pending for the
  // next decode to consume, and the next decode is this one.
  lb?.keepView();
  S.currentSrc = src;
  // `?.`, because the Outputs tab is a pane with no result block at all — its
  // image only ever exists in the viewer. The other two always have one.
  const thumb = P('result')?.querySelector('img');
  if (thumb) thumb.src = src;
  $('lbimg').src = src;
}

// Fit, 1:1 and drag-to-pan for an image in a scrolling pane: the viewer's and
// the Redraw dialog's. A factory, gridmark.js's shape, because two copies of the
// centring rules below are two chances for one to be fixed and the other not.
// Builds closures and nothing else, so calling it from an init keeps the
// declare-only rule.
//
// `pans(e)` says whether a pointerdown is a pan: any left drag in the viewer,
// Space+drag or a middle drag in the dialog, where a left drag paints.
export function zoomPane({ view, img, fit, full, pans }) {
  // Fitted, or true 1:1 and pannable. The fit/1:1 choice survives from the last
  // time the pane was open, because it is a preference about how you work.
  let isFit = true;
  // Set when the next decode is a newly opened or freshly rendered image rather
  // than a re-filter of the one on screen, and consumed by the image's load
  // handler. Without it a slider drag would yank the view back to the middle on
  // every step.
  let centreNext = false;

  // Landing in the top-left corner of a 3072² image shows you its corner. The
  // middle is what was on screen a moment ago in the fitted view, so that is
  // where 1:1 should start.
  const centre = () => {
    view.scrollLeft = (view.scrollWidth - view.clientWidth) / 2;
    view.scrollTop = (view.scrollHeight - view.clientHeight) / 2;
  };
  const setFit = (f) => {
    isFit = f;
    view.classList.toggle('fit', f);
    fit.classList.toggle('active', f);
    full.classList.toggle('active', !f);
    if (!f) centre();
  };

  fit.addEventListener('click', () => setFit(true));
  full.addEventListener('click', () => setFit(false));
  // The centring has to be re-run once the decode has given the pane something
  // to measure: a fresh data: URL has no dimensions yet when `show` returns, and
  // re-opening on the src already loaded fires no load event at all — so both
  // halves are needed.
  img.addEventListener('load', () => {
    if (centreNext) { centreNext = false; if (!isFit) centre(); }
  });

  // Drag to pan. Scroll offsets rather than a transform, so the wheel, a trackpad
  // and the scrollbars all keep working for free; `setPointerCapture` is what
  // makes a drag survive the pointer leaving the pane, and preventDefault on the
  // down is what stops the browser dragging a ghost of the image around instead.
  view.addEventListener('pointerdown', (e) => {
    if (isFit || !pans(e)) return;
    const x0 = e.clientX, y0 = e.clientY, sx = view.scrollLeft, sy = view.scrollTop;
    const move = (ev) => {
      view.scrollLeft = sx - (ev.clientX - x0);
      view.scrollTop = sy - (ev.clientY - y0);
    };
    const up = () => {
      view.classList.remove('drag');
      view.removeEventListener('pointermove', move);
      view.removeEventListener('pointerup', up);
      view.removeEventListener('pointercancel', up);
    };
    view.setPointerCapture(e.pointerId);
    view.classList.add('drag');
    view.addEventListener('pointermove', move);
    view.addEventListener('pointerup', up);
    view.addEventListener('pointercancel', up);
    e.preventDefault();
    e.stopPropagation();
  });

  return {
    // A different image is arriving: centre on it once it has decoded.
    expectNew: () => { centreNext = true; },
    // The next decode is a re-filter of the image on screen: leave the view.
    keepView: () => { centreNext = false; },
    // The pane was just shown: re-apply the remembered choice, now that there
    // is a size to centre against.
    shown: () => setFit(isFit),
    isFit: () => isFit,
  };
}

// The viewer's pane, made in initViewer.
let lb = null;

// The image is already a data: URL on the page, so this re-uses it rather than
// fetching anything.
//
// The scroll position does not survive a re-open, because it belonged to an
// image that has since been replaced.
export function showFullSize(src) {
  lb.expectNew();
  $('lbimg').src = src;
  $('lberr').textContent = '';
  $('lightbox').showModal();
  // After showModal, so the pane has a size to centre against.
  lb.shown();
}

// Post-process is the free rung of the ladder: no diffusion, just the display
// filters re-run on the image the server already has. That's cheap enough to
// drive from the slider itself rather than from a button.
let postBusy = false;   // a request is in flight
let postAgain = false;  // ...and the slider moved while it was
let postTimer = null;
// Where Reset goes: the model's sharpness and clarity with the grade at neutral,
// as the server reports it (`filters_start`). Not all zeros — resetting a fresh
// image to sharpness 0 would leave it softer than it arrived — and not the
// filters currently applied either, since after a Refine those carry the grade
// from the previous edit.
let postStart = {};
// Bumped by `cancelPost`, and compared across `runPost`'s await. A re-filter of
// the image you just navigated away from is already in flight by the time the
// next one is asked for, and it would otherwise land on top of the new picture —
// which reads as the arrow key having done nothing.
let postEpoch = 0;

// Debounce, then *coalesce*: while a request is out, later moves collapse into
// one follow-up with the latest values. The display worker is serialized, so a
// queue of drag positions would play back as a visible lag after letting go.
export function schedulePost() {
  syncPostLabels();
  if (S.imageId === null) return;
  clearTimeout(postTimer);
  postTimer = setTimeout(runPost, 120);
}

async function runPost() {
  // The sliders stay live while a pass runs: they are on the display lane, and
  // if a refine replaces the image mid-drag the server refuses the drag rather
  // than writing it over the new one (`Retained::update`).
  if (S.imageId === null) return;
  if (postBusy) { postAgain = true; return; }
  postBusy = true;
  try {
    const t0 = performance.now();
    // Every knob, every time. The route takes a patch and would accept just the
    // one that moved, but the panel is the state of the image and sending it
    // whole is what makes a coalesced drag — where two sliders moved and only
    // the last request survives — land both of them.
    const epoch = postEpoch;
    const data = await post('/adjust', {
      image_id: S.imageId,
      ...Object.fromEntries(FILTERS.map((f) => [f.id, parseFloat($(f.id).value)])),
    });
    // The image this was filtering is no longer the one on screen.
    if (epoch !== postEpoch) return;
    setResultImage(`data:image/png;base64,${data.image_base64}`);
    $('postms').textContent =
      `${Math.round(performance.now() - t0)} ms · gpu ${data.timings.fsr_ms} ms`;
    $('lberr').textContent = '';
  } catch (e) {
    // The image changed under the request: nothing to report about it.
    if (epoch !== postEpoch) return;
    // 409: a Refine or Hi-res landed while this drag was in flight. The image
    // is still live — newer, under the same id — and its result is on its way
    // in; nulling the id here used to leave Refine, Hi-res and Save dead.
    if (e.status === 409) return;
    // Into the panel, because that is what is on screen while a slider is being
    // dragged. A 400 means the retained entry is gone, which outlives the dialog
    // — so that one is also reported on the page behind it.
    $('lberr').textContent = String(e.message || e);
    if (e.status === 400) { S.imageId = null; escErr(String(e.message || e)); }
  } finally {
    postBusy = false;
    if (postAgain) { postAgain = false; runPost(); }
  }
}

// Read-only imported bindings again: only this module's own wiring reads
// `postStart`, so only the write crosses a file boundary.
export const setPostStart = (v) => { postStart = v; };

// Drop a debounced re-filter and disown one already in flight. Called when the
// image under the sliders is being replaced by a different one — the Outputs
// tab's arrow keys — rather than re-filtered.
export function cancelPost() {
  clearTimeout(postTimer);
  postAgain = false;
  postEpoch++;
}

// The viewer can already be open when render() runs — a modal dialog stops
// clicks, not a batch that was started before it was opened. So the big copy is
// kept in step here too, and re-centred, because what arrived is a different
// image rather than a re-filter of the one on screen.
export function setViewerImage(src) {
  lb?.expectNew();
  $('lbimg').src = src;
}

// Declarations only above this line: everything that touches the DOM or starts a
// fetch happens here, called from app.js, so an import cycle can never be read
// half-initialised. Same shape in every module.
export function initViewer() {
  // Built once, at load, so the panel exists before the first image does —
  // `syncAdvanced` writes the model's start points into it while the viewer is
  // still closed.
  $('lbsliders').innerHTML = FILTERS.map((f) => `
    ${f.group ? `<div class="lbgroup">${f.group}</div>` : ''}
    <div class="slider">
      <div class="head">
        <label for="${f.id}">${f.label}</label><span class="val" id="${f.id}val"></span>
      </div>
      <input id="${f.id}" type="range" min="${f.min ?? -1}" max="1" step="0.01" value="0">
    </div>`).join('');

  // 'input', not 'change': the point is to see the filter while dragging. Double
  // click sends one knob home, which is the only way back to an exact 0 on a
  // pointer — a drag can always land a hundredth off.
  for (const f of FILTERS) {
    $(f.id).addEventListener('input', schedulePost);
    $(f.id).addEventListener('dblclick', () => {
      $(f.id).value = postStart[f.id] ?? 0;
      schedulePost();
    });
  }

  // The viewer. Closing is the x and Escape (which <dialog> handles itself) and
  // nothing else: a click-anywhere-to-dismiss is fine for something you only look
  // at and hostile for something you edit in.
  $('lbclose').addEventListener('click', () => $('lightbox').close());
  $('lbreset').addEventListener('click', () => { setPost(postStart); schedulePost(); });
  lb = zoomPane({
    view: $('lbview'), img: $('lbimg'), fit: $('lbfit'), full: $('lbfull'),
    pans: (e) => e.button === 0,
  });
}
