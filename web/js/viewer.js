import { $, esc, post, rememberFolds } from './dom.js';
import { S, P, escErr } from './state.js';
import { onJobs, passRunning } from './jobs.js';
import { regionTool } from './regiontool.js';
import { brushPaint } from './brushpaint.js';

// The viewer's panel, top to bottom. This list is the only place a filter is
// declared: the markup, the labels, setPost, the /adjust body and the
// input listeners all read it, so adding one is a row here and a field on the
// server's `Filters`. The names are the wire names — that is what lets the body
// and the response be built by looping rather than by naming ten keys twice.
//
// Only the last two are per-model; the nine grading knobs always start at 0,
// because a checkpoint has a house sharpness and no opinion about your hue.
// `help` is the tooltip text, and it lives here rather than in `HELP` below for
// the reason the rest of the row does: a slider is declared in one place, and
// number twelve should be one line in one list.
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
  { id: 'vibrance', label: 'Vibrance',
    help: 'Saturation for the dull colours only: muted ones gain the most, and '
        + 'colours that are already vivid barely move, so nothing clips first. '
        + 'Gentler on skin than Saturation. Negative mutes the dull ones first.' },
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
//
// These are the *whole picture's* filters. While the sliders point at the area
// (`target`), they are parked and set there instead.
export function setPost(filters) {
  if (target === 'all') writeSliders(filters);
  else parked = { ...filters };
  syncPostLabels();
}

export function syncPostLabels() {
  for (const f of FILTERS) {
    const v = Number($(f.id).value);
    $(f.id + 'val').textContent = f.fmt ? f.fmt(v) : v.toFixed(2);
  }
  viewerSummaries();
}

const readSliders = () => Object.fromEntries(FILTERS.map((f) => [f.id, parseFloat($(f.id).value)]));
function writeSliders(v) {
  for (const f of FILTERS) $(f.id).value = v[f.id] ?? 0;
}

// ---- Two looks: the whole picture's, and the area's -------------------------
//
// The eleven sliders are one set of controls for two sets of values: the whole
// picture's filters, and the area's (`crate::area` on the server). `target`
// says which the sliders show; the other is held here. Sent whole, both, on
// every re-filter.
let target = 'all';
// The whole picture's filters while the sliders show the area's.
let parked = {};
// The area: {shape, x0, y0, x1, y1 (0..1 of the frame), feather, invert,
// filters}, or null. Its `filters` are stale while the sliders show them.
let area = null;
// The last result the viewer was handed: its size and history, for Crop.
let shown = null;
// What a crop or an undo's answer goes through, per pane: the Image tab's
// `render`, the Outputs viewer's own. Registered by those modules.
const resultHandlers = {};
export const onViewerResult = (pane, fn) => { resultHandlers[pane] = fn; };

const globalFilters = () => (target === 'all' ? readSliders() : parked);
const areaState = () => area && { ...area, filters: target === 'area' ? readSliders() : area.filters };

function setTarget(t) {
  if (t === target || (t === 'area' && !area)) return;
  if (target === 'all') parked = readSliders(); else area.filters = readSliders();
  target = t;
  writeSliders(t === 'all' ? parked : area.filters);
  $('lbtargetall').classList.toggle('active', t === 'all');
  $('lbtargetarea').classList.toggle('active', t === 'area');
  syncPostLabels();
}

// A new picture, or a new state of this one: the server's word on both looks,
// where Reset goes, and the history Crop's undo reads.
export function setLook(data) {
  // A different picture: the page's masks were of the last one.
  if (shown && data.image_id !== shown.image_id) paint?.reset();
  shown = data;
  postStart = data.filters_start ?? data.filters;
  target = 'all';
  $('lbtargetall').classList.add('active');
  $('lbtargetarea').classList.remove('active');
  area = data.area ?? null;
  writeSliders(data.filters);
  syncArea();
  syncBrushes();
  drawHistory();
  syncPostLabels();
  // The box describes the picture that was there; put it on this one once
  // it has decoded and has a size.
  if (box?.shown()) $('lbimg').addEventListener('load', reshowTool, { once: true });
}

// ---- Summaries --------------------------------------------------------------
//
// Every fold says what is inside it when folded, as the Image tab's do.
const signed = (f, v) => (f.fmt ? f.fmt(v) : `${v > 0 ? '+' : ''}${v.toFixed(2)}`);
function say(id, text, on) {
  $(id).textContent = text ? '— ' + text : '';
  $(id).title = text;
  $(id).classList.toggle('on', !!on);
}
function viewerSummaries() {
  // Before initViewer has built the slider folds there is nothing to say.
  if (!$('sum-lb-tone')) return;
  const v = readSliders();
  const base = target === 'all' ? postStart : {};
  for (const [group, rows] of groups()) {
    const moved = rows.filter((f) => Math.abs((v[f.id] ?? 0) - (base[f.id] ?? 0)) > 0.004);
    const text = moved.map((f) => `${f.label} ${signed(f, v[f.id])}`).join(' · ') || 'neutral';
    say(`sum-lb-${group.toLowerCase()}`, (target === 'area' ? 'Area: ' : '') + text, moved.length);
  }
  const a = areaState();
  const changes = a ? Object.values(a.filters).filter((x) => Math.abs(x) > 0.004).length : 0;
  say('sum-lb-area', a
    ? [a.shape === 'ellipse' ? 'ellipse' : 'rectangle',
      `${changes} change${changes === 1 ? '' : 's'}`, a.invert ? 'inverted' : null]
      .filter(Boolean).join(' · ')
    : 'none', !!a);
  const cropped = isCrop();
  say('sum-lb-crop', cropped ? `${shown.width}×${shown.height}` : 'whole picture', cropped);
  const painted = Object.entries(strokes());
  say('sum-lb-brush', painted.map(([t, a]) => `${TOOL_LABELS[t] ?? t} ${a.toFixed(2)}${
    t === 'restore' && shown.restore_from != null ? ` from ${shown.restore_from + 1}` : ''}`).join(' · ') || 'none',
  painted.length > 0);
  syncApply();
}

// FILTERS' rows by their `group`, which only the first row of each names.
function groups() {
  const out = [];
  for (const f of FILTERS) {
    if (f.group) out.push([f.group, []]);
    out[out.length - 1][1].push(f);
  }
  return out;
}

// ---- Crop and Area: one box tool, one use at a time --------------------------
//
// Opening the Crop fold puts a crop box up and closing it drops an unapplied
// one; opening Area shows the area's shape for dragging. Only one of the two
// is open at once, so the box under the pointer is never ambiguous.
let box = null; // the box tool, made in initViewer
let toolMode = null; // 'crop' | 'area' | null

// Whether the picture on screen has been cropped: any crop step up to here.
const isCrop = () => !!shown && (shown.history ?? []).slice(0, shown.at + 1).some((l) => /^Crop /.test(l));

// The list holds each ratio once, landscape; `turned` stands it on end. For
// Original it swaps the picture's own sides, so a landscape picture can be cut
// to a portrait of the same proportions.
let turned = false;
function aspectValue() {
  const v = $('lbaspect').value;
  if (!v) return null;
  const r = v === 'orig' ? (shown ? shown.width / shown.height : null) : Number(v);
  return r && turned ? 1 / r : r;
}

// Turning means nothing for Free or 1:1, so the button says so.
function syncTurn() {
  const v = $('lbaspect').value;
  const b = $('lbaspectturn');
  b.disabled = !v || v === '1';
  b.classList.toggle('active', turned && !b.disabled);
  b.title = b.disabled ? 'Pick an aspect to turn it'
    : turned ? 'Portrait — turn back to landscape' : 'Landscape — turn to portrait';
}

function showCropTool() {
  toolMode = 'crop';
  box.show(null, { dim: true, shape: 'rect', feather: 0, aspect: aspectValue() });
}

function showAreaTool() {
  if (!area) { box.hide(); toolMode = null; return; }
  toolMode = 'area';
  const w = $('lbimg').naturalWidth || 1, h = $('lbimg').naturalHeight || 1;
  box.show(
    { x: area.x0 * w, y: area.y0 * h, w: (area.x1 - area.x0) * w, h: (area.y1 - area.y0) * h },
    { dim: false, shape: area.shape, feather: area.feather, aspect: null },
  );
}

function reshowTool() {
  if (toolMode === 'crop') box.reset();
  else if (toolMode === 'area') showAreaTool();
}

function onBox(b) {
  const w = $('lbimg').naturalWidth || 1, h = $('lbimg').naturalHeight || 1;
  if (toolMode === 'crop') {
    const snap = (v) => Math.floor(v / 8) * 8;
    $('lbcropsize').textContent = `${snap(b.w)} × ${snap(b.h)} px`;
  } else if (toolMode === 'area' && area) {
    Object.assign(area, { x0: b.x / w, y0: b.y / h, x1: (b.x + b.w) / w, y1: (b.y + b.h) / h });
    schedulePost();
  }
}

// Crop, Apply and History all make or move a history step, which a pass
// landing afterwards would silently overwrite: locked while one runs.
function syncCrop() {
  const busy = passRunning();
  $('lbcropapply').disabled = busy || stepping;
  $('lbcropapply').title = busy ? 'A pass is running on the image — crop when it lands' : '';
  for (const b of $('lbhistory').querySelectorAll('button')) b.disabled = busy || stepping;
  syncApply();
}

// Apply is for a look that isn't neutral: a whole-picture slider off 0, or an
// area that changes something.
function syncApply() {
  const g = globalFilters();
  const a = areaState();
  const moved = Object.values(g).some((v) => v !== 0)
    || (!!a && Object.values(a.filters).some((v) => v !== 0))
    || Object.keys(strokes()).length > 0;
  $('lbapply').disabled = !moved || passRunning() || stepping || S.imageId === null;
}

// ---- Brushes -------------------------------------------------------------------
//
// One mask and one amount per tool, held by the server (`crate::brush`) and
// painted here (`brushpaint.js`). A stroke ends in `/brush` with the tool's
// whole mask; the amount slider re-sends only the amount. Both answer with the
// re-filtered picture, which lands like a slider's (`setResultImage`), so a
// stroke never re-centres the view.
const TOOL_LABELS = {
  dodge: 'Dodge', burn: 'Burn', saturate: 'Saturate', desaturate: 'Desaturate', blur: 'Blur', sharpen: 'Sharpen',
  restore: 'Restore',
};
let paint = null;
let brushTool = 'dodge';
// The tools whose strokes the server holds, and their amounts.
const strokes = () => shown?.brushes ?? {};
// One `/brush` at a time, in order; a stroke already queued for a tool reads
// that tool's mask when it goes, so a burst of strokes is one request.
let brushChain = Promise.resolve();
const strokeQueued = new Set();
let amountTimer = null;

const amountFor = (t) => (t === brushTool ? Number($('lbbamount').value) : strokes()[t] ?? 0.5);

function brushSend(make) {
  brushChain = brushChain.then(async () => {
    if (S.imageId === null) return;
    const epoch = postEpoch;
    try {
      await settlePost();
      const t0 = performance.now();
      const data = await post('/brush', { image_id: S.imageId, ...make() });
      if (epoch !== postEpoch) return;
      setResultImage(`data:image/png;base64,${data.image_base64}`);
      shown = { ...shown, brushes: data.brushes, restore_from: data.restore_from };
      $('postms').textContent =
        `${Math.round(performance.now() - t0)} ms · gpu ${data.timings.fsr_ms} ms`;
      $('lberr').textContent = '';
      viewerSummaries();
    } catch (e) {
      // A pass landed first: its result is on its way, and these strokes go
      // with the picture they were painted on.
      if (epoch !== postEpoch || e.status === 409) return;
      $('lberr').textContent = String(e.message || e);
    }
  });
}

function queueStroke(t) {
  if (strokeQueued.has(t)) return;
  strokeQueued.add(t);
  brushSend(() => {
    strokeQueued.delete(t);
    return { tool: t, amount: amountFor(t), mask: paint.dataUrl(t), ...fromFor(t) };
  });
}

function clearTool(t) {
  paint.clear(t);
  if (t in strokes()) brushSend(() => ({ tool: t, mask: null }));
}

function chooseTool(t) {
  brushTool = t;
  paint.setTool(t);
  for (const b of document.querySelectorAll('.lbtools [data-tool]')) {
    b.classList.toggle('active', b.dataset.tool === t);
  }
  // A fresh Restore starts at full strength: half a restore is rarely what
  // "take that part back" means.
  if (t in strokes()) $('lbbamount').value = strokes()[t];
  else if (t === 'restore') $('lbbamount').value = 1;
  $('lbbamountval').textContent = Number($('lbbamount').value).toFixed(2);
  $('lbbfromrow').hidden = t !== 'restore';
}

// ---- Restore -------------------------------------------------------------------
//
// The Restore brush paints a History step back in: its source is any step but
// the one on screen, by default the one before (the one after, on the first).
// The server keys the strokes to that step and drops them on a new one.
const fromFor = (t) => (t === 'restore' ? { from: Number($('lbbfrom').value) } : {});

function syncRestoreFrom() {
  const steps = shown?.history ?? [];
  const at = shown?.at ?? 0;
  const btn = document.querySelector('.lbtools [data-tool="restore"]');
  const none = steps.length < 2;
  btn.disabled = none;
  btn.title = none ? 'Restore needs an earlier step: refine, upscale or crop first'
    : 'Paint an earlier step back in — refine, then take back the parts you liked better before';
  if (none && brushTool === 'restore') chooseTool('dodge');
  const keep = shown?.restore_from ?? (at > 0 ? at - 1 : 1);
  $('lbbfrom').innerHTML = steps.map((label, i) => (i === at ? ''
    : `<option value="${i}"${i === keep ? ' selected' : ''}>${i + 1}. ${esc(label)}</option>`)).join('');
}

// After a result: drop the page's masks for tools the picture no longer has,
// and fetch the server's for those the page lacks at this size — only while
// the fold is open, since nothing needs them otherwise.
function syncBrushes() {
  if (!paint) return;
  for (const t of Object.keys(TOOL_LABELS)) if (!(t in strokes())) paint.clear(t);
  syncRestoreFrom();
  if ($('fold-lb-brush').open) loadMasks();
  if (brushTool in strokes()) $('lbbamount').value = strokes()[brushTool];
  $('lbbamountval').textContent = Number($('lbbamount').value).toFixed(2);
}

function loadMasks() {
  for (const t of Object.keys(strokes())) {
    // The query only defeats the cache; the server routes on the path.
    if (!paint.has(t)) paint.load(t, `/brush/${shown.image_id}/${t}.png?v=${Date.now()}`).catch(() => {});
  }
}

// ---- History -----------------------------------------------------------------
//
// The image's states, newest first, as the Refine dialog lists them — one
// markup for both lists, so they can't come to disagree about a step.
export const historyMarkup = (data) => (data?.history ?? []).map((label, i) => `
    <li><button type="button" data-step="${i}" class="${i === data.at ? 'active' : ''}">
      <img src="/history/${data.image_id}/${i}.png" alt="" loading="lazy">
      <span>${i + 1}. ${esc(label)}</span>
    </button></li>`).reverse().join('');

function drawHistory() {
  $('lbhistory').innerHTML = historyMarkup(shown);
  const n = shown?.history?.length ?? 0;
  say('sum-lb-history', n ? `${n} step${n === 1 ? '' : 's'} · at ${shown.at + 1}` : '',
    n > 0 && shown.at < n - 1);
  syncCrop();
}

function syncArea() {
  $('lbareabox').hidden = !area;
  if (area) {
    $('lbshaperect').classList.toggle('active', area.shape === 'rect');
    $('lbshapeellipse').classList.toggle('active', area.shape === 'ellipse');
    $('lbfeather').value = area.feather;
    $('lbfeatherval').textContent = area.feather.toFixed(2);
    $('lbinvert').checked = area.invert;
  }
  if (toolMode === 'area' && !area) { box.hide(); toolMode = null; }
}

// The crop or the undo answered with a new state of the image: the pane that
// owns the viewer draws it, the same way it draws any other result.
function deliver(data) {
  resultHandlers[S.pane]?.(data);
}

// A crop, an Apply or a jump through History: each answers with a new state
// of the image, and says whether it landed. One at a time, and after any re-filter still on its way — so
// what is applied or cropped is what is on screen.
let stepping = false;
async function stepRequest(route, body) {
  if (S.imageId === null || stepping) return false;
  stepping = true;
  syncCrop();
  $('lberr').textContent = '';
  try {
    await settlePost();
    // And any stroke still on its way: an Apply must bake what was painted.
    await brushChain;
    const data = await post(route, { image_id: S.imageId, ...body });
    deliver(data);
    return true;
  } catch (e) {
    // A pass landed first: the image is newer, and its result is on its way.
    if (e.status !== 409) $('lberr').textContent = String(e.message || e);
    return false;
  } finally {
    stepping = false;
    syncCrop();
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

// Let a debounced or in-flight re-filter land before a step is taken from
// the picture (Apply, Crop, a History jump), so the step is of what is shown.
async function settlePost() {
  if (postTimer !== null) {
    clearTimeout(postTimer);
    await runPost();
  }
  while (postBusy) await new Promise((r) => { setTimeout(r, 30); });
}

async function runPost() {
  postTimer = null;
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
      ...globalFilters(),
      // Always sent, `null` included: the panel is the whole state, and `null`
      // is how the server hears that the area was removed.
      area: areaState(),
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
  // A fold per group, Tone and Colour open by default; `rememberFolds` below
  // keeps whatever the user leaves them as.
  $('lbsliders').innerHTML = groups().map(([group, rows]) => `
    <details id="fold-lb-${group.toLowerCase()}" class="fold"${group === 'Detail' ? '' : ' open'}>
      <summary>${group} <span class="foldstate" id="sum-lb-${group.toLowerCase()}"></span></summary>
      ${rows.map((f) => `
      <div class="slider">
        <div class="head">
          <label for="${f.id}">${f.label}</label><span class="val" id="${f.id}val"></span>
        </div>
        <input id="${f.id}" type="range" min="${f.min ?? -1}" max="1" step="0.01" value="0">
      </div>`).join('')}
    </details>`).join('');

  // 'input', not 'change': the point is to see the filter while dragging. Double
  // click sends one knob home, which is the only way back to an exact 0 on a
  // pointer — a drag can always land a hundredth off.
  for (const f of FILTERS) {
    $(f.id).addEventListener('input', schedulePost);
    // Home is the image's start point for the whole picture, and 0 — "as the
    // rest of the picture" — for the area.
    $(f.id).addEventListener('dblclick', () => {
      $(f.id).value = target === 'all' ? (postStart[f.id] ?? 0) : 0;
      schedulePost();
    });
  }

  // The viewer. Closing is the x and Escape (which <dialog> handles itself) and
  // nothing else: a click-anywhere-to-dismiss is fine for something you only look
  // at and hostile for something you edit in.
  $('lbclose').addEventListener('click', () => $('lightbox').close());
  // Reset is the whole look: the picture's sliders home, and the area gone.
  $('lbreset').addEventListener('click', () => {
    setTarget('all');
    area = null;
    syncArea();
    setPost(postStart);
    schedulePost();
    // The strokes are part of the look too.
    for (const t of Object.keys(TOOL_LABELS)) clearTool(t);
  });
  // Left drag pans at 1:1; the middle button too, which is the pan left while
  // a crop or area box has the left one.
  lb = zoomPane({
    view: $('lbview'), img: $('lbimg'), fit: $('lbfit'), full: $('lbfull'),
    pans: (e) => e.button === 0 || e.button === 1,
  });

  box = regionTool({ view: $('lbview'), img: $('lbimg'), onChange: onBox });
  box.init();
  const crop = $('fold-lb-crop'), areaFold = $('fold-lb-area');
  const brushFold = $('fold-lb-brush');
  crop.addEventListener('toggle', () => {
    if (crop.open) {
      areaFold.open = false;
      brushFold.open = false;
      showCropTool();
    } else if (toolMode === 'crop') {
      box.hide();
      toolMode = null;
    }
  });
  areaFold.addEventListener('toggle', () => {
    if (areaFold.open) {
      crop.open = false;
      brushFold.open = false;
      showAreaTool();
    } else if (toolMode === 'area') {
      box.hide();
      toolMode = null;
    }
  });
  $('lbaspect').addEventListener('change', () => { syncTurn(); box.set({ aspect: aspectValue() }); });
  $('lbaspectturn').addEventListener('click', () => {
    turned = !turned;
    syncTurn();
    box.set({ aspect: aspectValue() });
  });
  syncTurn();
  $('lbcropcancel').addEventListener('click', () => { crop.open = false; });
  // A crop is done once it has landed: the fold closes, which takes the box
  // down, so the next drag on the picture is a pan again.
  $('lbcropapply').addEventListener('click', async () => {
    const b = box.box();
    if (toolMode !== 'crop' || !b) return;
    if (await stepRequest('/crop', { x: b.x, y: b.y, width: b.w, height: b.h })) crop.open = false;
  });
  $('lbapply').addEventListener('click', () => stepRequest('/apply', {}));
  $('lbhistory').addEventListener('click', (e) => {
    const b = e.target.closest('[data-step]');
    if (!b || b.disabled || !shown) return;
    const index = Number(b.dataset.step);
    if (index === shown.at) return;
    $('lbimg').addEventListener('load', reshowTool, { once: true });
    stepRequest('/history/goto', { index });
  });

  // The area: made on first choosing it, a centred ellipse.
  $('lbtargetarea').addEventListener('click', () => {
    if (!area) {
      area = { shape: 'ellipse', x0: 0.3, y0: 0.3, x1: 0.7, y1: 0.7, feather: 0.1, invert: false, filters: {} };
      syncArea();
    }
    setTarget('area');
    if (areaFold.open) showAreaTool();
  });
  $('lbtargetall').addEventListener('click', () => setTarget('all'));
  for (const [id, shape] of [['lbshaperect', 'rect'], ['lbshapeellipse', 'ellipse']]) {
    $(id).addEventListener('click', () => {
      if (!area) return;
      area.shape = shape;
      syncArea();
      box.set({ shape });
      schedulePost();
    });
  }
  $('lbfeather').addEventListener('input', () => {
    if (!area) return;
    area.feather = Number($('lbfeather').value);
    $('lbfeatherval').textContent = area.feather.toFixed(2);
    box.set({ feather: area.feather });
    schedulePost();
  });
  $('lbinvert').addEventListener('change', () => {
    if (!area) return;
    area.invert = $('lbinvert').checked;
    schedulePost();
  });
  $('lbarearemove').addEventListener('click', () => {
    setTarget('all');
    area = null;
    syncArea();
    schedulePost();
  });

  // Brushes: the third tool that owns the pointer, so it excludes the other
  // two the way they exclude each other.
  paint = brushPaint({
    view: $('lbview'), img: $('lbimg'), onStroke: queueStroke,
    locked: () => passRunning() || S.imageId === null,
  });
  paint.init();
  brushFold.addEventListener('toggle', () => {
    if (brushFold.open) {
      crop.open = false;
      areaFold.open = false;
      paint.show();
      loadMasks();
    } else {
      paint.hide();
    }
  });
  for (const b of document.querySelectorAll('.lbtools [data-tool]')) {
    b.addEventListener('click', () => chooseTool(b.dataset.tool));
  }
  const sizes = () => {
    paint.set({
      size: Number($('lbbsize').value),
      soft: Number($('lbbsoft').value),
      erase: $('lbberase').checked,
    });
    $('lbbsizeval').textContent = `${$('lbbsize').value} px`;
    $('lbbsoftval').textContent = Number($('lbbsoft').value).toFixed(2);
  };
  for (const id of ['lbbsize', 'lbbsoft', 'lbberase']) $(id).addEventListener('input', sizes);
  sizes();
  // The amount re-filters the tool's strokes live, debounced like a slider.
  $('lbbamount').addEventListener('input', () => {
    $('lbbamountval').textContent = Number($('lbbamount').value).toFixed(2);
    const t = brushTool;
    if (!(t in strokes())) return;
    clearTimeout(amountTimer);
    amountTimer = setTimeout(() => brushSend(() => ({ tool: t, amount: amountFor(t) })), 120);
  });
  // Re-pointing Restore moves the strokes already painted to the new step.
  $('lbbfrom').addEventListener('change', () => {
    if ('restore' in strokes()) brushSend(() => ({ tool: 'restore', amount: amountFor('restore'), ...fromFor('restore') }));
  });
  $('lbbclear').addEventListener('click', () => clearTool(brushTool));
  $('lbbclearall').addEventListener('click', () => {
    for (const t of Object.keys(TOOL_LABELS)) clearTool(t);
  });
  chooseTool(brushTool);

  onJobs(syncCrop);
  rememberFolds($('lightbox'));
}
