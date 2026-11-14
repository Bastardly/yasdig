import { $, esc, getJSON, post, setPlayIcon, decodeFrames, stillsOf } from './dom.js';
import { S } from './state.js';
import { setBusy, setIdle } from './busy.js';
import { soonJobs, stopAllKind } from './jobs.js';
import { floatPanels } from './panels.js';
import { upOptions, setUpFactor } from './image.js';
import { FORMATS, available, exportClip, outSize } from './export.js';

// ---- The clip dialog ----------------------------------------------------------
//
// A video played full size, Refine & adjust's shape: a rail, the player on a
// pane, floating panels. Two kinds open here:
//
// - A Video from the Video tab (`work/videos/{id}/`, or starred in
//   `user/favorite_clips/`), whole: its generated shots joined in storyboard
//   order, as the stage plays it. Enlarge makes a new version of the Video —
//   one job per shot (`Engine::enlarge_clip`), every shot into the same
//   `v{n}` — so a version is computed from the shots, never stored: a shot
//   drawn again or extended since has lost its versions, and the version says
//   which shots it no longer covers and offers to enlarge just those. History
//   lists the versions with Compare against the original; Save to output and
//   Download's AVI are the server's join (`VideoStore::joined`), which refuses
//   shots that can't be one picture.
// - A clip saved to output (`{stem}.avi`), opened from the Outputs tab: it
//   plays and downloads, its frames read out of the AVI
//   (`/outputs/{stem}/frames/{k}.jpg`).
//
// Download encodes the version on screen into a file to share (`export.js`).
//
// The player is this module's own, not the Video stage's. Opening the dialog
// stops the stage (`onOpen`): one clip plays at a time, the one looked at.

let view = null; // { kind: 'video', id, shots: [{c, label}] } | { kind: 'output', stem, info }
let version = 0;
let frames = [];
let base = []; // the original's frames, while Compare is on
let at = 0;
let timer = null;
let split = 0.5;
let seq = 0; // bumped per load: only the newest may draw
let panels = null;
// Told when a shot's versions change, so the Video tab's grid and stage hold
// the shot as it now is.
let onClip = () => {};
let onOpen = () => {};
// The formats this browser can make of the version on screen, and a
// download in progress (its AbortController).
let canMake = new Set(['avi']);
let exporting = null;

const isVid = () => view?.kind === 'video';
const shots = () => (isVid() ? view.shots : []);
const fps = () => Math.max(1, isVid() ? shots()[0]?.c.fps ?? 1 : view.info.fps);
const versionOf = (c, n) => (c.versions ?? []).find((x) => x.n === n);

// The Video's versions: every `n` some shot has, with what made it (off the
// first shot holding it) and the shots it doesn't cover.
function versions() {
  if (!isVid()) return [];
  const first = shots()[0]?.c;
  const ns = [...new Set(shots().flatMap(({ c }) => (c.versions ?? []).map((x) => x.n)))].sort((a, b) => a - b);
  return [{ n: 0, label: 'Original', width: first?.width, height: first?.height, missing: [] },
    ...ns.map((n) => {
      const v = shots().map(({ c }) => versionOf(c, n)).find(Boolean);
      return {
        ...v,
        missing: shots().filter(({ c }) => !versionOf(c, n)),
        label: `Enlarged ${v.factor}×${v.ltx ? ` by ${v.ltx.model} at ${v.ltx.strength}` : ''}`
          + `${v.sharpness ? ` · sharpness ${v.sharpness}` : ''}`
          + `${v.clarity ? ` · clarity ${v.clarity}` : ''}`,
      };
    })];
}

// Why version `v` can't be one file, or null: shots it doesn't cover, or
// shots of another size or rate (made before the lock was let go). The server's join
// refuses the same; said here before a download is started.
function joinProblem(v) {
  const ver = versions().find((x) => x.n === v);
  if (ver?.missing.length) {
    return `This version doesn't cover ${ver.missing.map((s) => s.label).join(', ')} — Enlarge them first.`;
  }
  const size = (c) => (v ? versionOf(c, v) : c);
  const [a] = shots();
  if (!a) return null;
  const odd = shots().filter(({ c }) => size(c).width !== size(a.c).width || size(c).height !== size(a.c).height
    || c.fps !== a.c.fps);
  return odd.length
    ? `${odd.map((s) => s.label).join(', ')} ${odd.length > 1 ? 'are' : 'is'} another size or frame rate than ${a.label} — generate ${odd.length > 1 ? 'them' : 'it'} again.`
    : null;
}

// ---- Enlarge's method ---------------------------------------------------------
//
// FSR, or LTX's latent ×2 and refine (`Engine::enlarge_clip_ltx`): offered
// only while some installed LTX model has its latent upscaler. Read from
// `/video-models` on each open, since a download may have landed since.

let method = 'fsr';
let ltxModels = [];

async function loadLtxModels() {
  try {
    const all = await getJSON('/video-models');
    ltxModels = (Array.isArray(all) ? all : []).filter((m) => m.ltx && m.exists && m.latent_upscaler);
  } catch {
    ltxModels = [];
  }
  const pick = $('cbltxmodel').value;
  $('cbltxmodel').innerHTML = ltxModels.map((m) => `<option value="${esc(m.name)}">${esc(m.name)}</option>`).join('');
  if (ltxModels.some((m) => m.name === pick)) $('cbltxmodel').value = pick;
  if (!ltxModels.length) method = 'fsr';
  syncMethod();
}

function syncMethod() {
  $('cbmethodrow').hidden = !ltxModels.length;
  for (const b of $('cbmethodrow').querySelectorAll('[data-method]')) b.classList.toggle('active', b.dataset.method === method);
  const ltx = method === 'ltx';
  $('cbltx').hidden = !ltx;
  $('cbfsr').hidden = ltx;
  $('cbfsrnote').hidden = ltx;
  // A distilled model refines on its own schedule: no steps to choose.
  const m = ltxModels.find((x) => x.name === $('cbltxmodel').value);
  $('cbltxstepsbox').hidden = !!m?.hires_sigmas;
  syncRail();
}

// A shot's frame `k` of version `v` — its original's when the version
// doesn't cover it, so an unfinished version still plays whole.
function shotFrameUrl(c, v, k) {
  const id = encodeURIComponent(c.id);
  return v && versionOf(c, v) ? `/videos/${id}/v${v}/frames/${k}.png` : `/videos/${id}/frames/${k}.png`;
}

// Frame `k` of the whole: the shot it falls in, and its frame there.
function frameUrl(v, k) {
  if (!isVid()) return `/outputs/${encodeURIComponent(view.stem)}/frames/${k}.jpg`;
  for (const { c } of shots()) {
    if (k < c.frames) return shotFrameUrl(c, v, k);
    k -= c.frames;
  }
  return '';
}

function frameCount() {
  return isVid() ? shots().reduce((t, { c }) => t + c.frames, 0) : view.info.frames;
}

// Shot by shot, so narrated stills stay a still per part (an enlarged
// version of one is still a still per part).
async function decode(v) {
  if (!isVid()) return (await decodeFrames(frameCount(), (k) => frameUrl(v, k), null)).filter(Boolean);
  const got = await Promise.all(shots().map(({ c }) => decodeFrames(c.frames, (k) => shotFrameUrl(c, v, k), stillsOf(c))));
  return got.flat().filter(Boolean);
}

function stop() {
  clearInterval(timer);
  timer = null;
  $('cbaudio').pause();
  setPlayIcon($('cbplay'), false);
}

function draw(i) {
  if (!frames[i]) return;
  at = i;
  const cv = $('cbcanvas');
  const ctx = cv.getContext('2d');
  ctx.drawImage(frames[i], 0, 0, cv.width, cv.height);
  // Compare: the original on the left of the line, scaled to this version's
  // size, so the same moment is judged side by side.
  if ($('cbcompare').checked && base[i] && version) {
    const x = Math.round(cv.width * split);
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, x, cv.height);
    ctx.clip();
    ctx.drawImage(base[i], 0, 0, cv.width, cv.height);
    ctx.restore();
    ctx.fillStyle = getComputedStyle(cv).getPropertyValue('--state').trim() || '#ddd9d2';
    ctx.fillRect(x - 1, 0, 2, cv.height);
  }
  $('cbscrub').value = i;
  $('cbat').textContent = `${shotAt(i)}${i + 1}/${frames.length} · ${(i / fps()).toFixed(2)}s`;
}

// "Shot 2 · " for a Video of more than one shot.
function shotAt(i) {
  if (shots().length < 2) return '';
  for (const { c, label } of shots()) {
    if (i < c.frames) return `${label} · `;
    i -= c.frames;
  }
  return '';
}

function play() {
  if (timer) { stop(); return; }
  if (!frames.length) return;
  setPlayIcon($('cbplay'), true);
  const a = $('cbaudio');
  if (a.src) {
    a.currentTime = at / fps();
    a.play().catch(() => {});
  }
  timer = setInterval(() => {
    const next = (at + 1) % frames.length;
    draw(next);
    if (next === 0 && a.src) {
      a.currentTime = 0;
      a.play().catch(() => {});
    }
  }, 1000 / fps());
}

// Show version `v`: its frames decoded before the first is drawn, as the
// stage's player does, so the first pass plays at the clip's own rate.
async function show(v) {
  const mine = ++seq;
  stop();
  version = v;
  drawHistory();
  syncRail();
  const got = await decode(v);
  if (mine !== seq) return;
  frames = got;
  if (!frames.length) {
    $('cberr').textContent = 'Its frames could not be loaded.';
    return;
  }
  const cv = $('cbcanvas');
  cv.width = frames[0].naturalWidth;
  cv.height = frames[0].naturalHeight;
  $('cbscrub').max = Math.max(0, frames.length - 1);
  if ($('cbcompare').checked) await loadBase();
  if (mine !== seq) return;
  // Shown, not played — the stage's rule.
  draw(Math.min(at, frames.length - 1));
  probeFormats();
}

async function loadBase() {
  if (base.length || !isVid()) return;
  base = await decode(0);
}

function drawHistory() {
  const list = versions();
  $('cbcmprow').hidden = list.length < 2;
  $('cbhistory').innerHTML = list.map((v) => {
    const holder = shots().find(({ c }) => !v.n || versionOf(c, v.n))?.c;
    const id = encodeURIComponent(holder?.id ?? '');
    const gap = v.missing.length ? ` · ${v.missing.length} shot${v.missing.length > 1 ? 's' : ''} not enlarged` : '';
    return `
    <li><button type="button" data-v="${v.n}" class="${v.n === version ? 'active' : ''}">
      <img src="${v.n ? `/videos/${id}/v${v.n}/frames/0.png` : `/videos/${id}/thumb.png`}" alt="" loading="lazy">
      <span>${esc(v.label)} · ${v.width}×${v.height}${esc(gap)}</span>
    </button></li>`;
  }).reverse().join('');
  const n = list.length - 1;
  $('sum-cb-history').textContent = n ? `— ${n} version${n > 1 ? 's' : ''}` : '';
  panels?.dot('cb-history', n > 0 && version > 0);
}

function syncRail() {
  const vid = isVid();
  // A saved clip only plays and downloads.
  for (const b of $('clipbox').querySelectorAll('.lbrail .railbtn')) b.hidden = !vid && b.dataset.panel !== 'cb-download';
  $('cbsave').hidden = !vid;
  if (!vid) {
    for (const id of ['panel-cb-enlarge', 'panel-cb-history']) if ($(id).open) $(id).open = false;
  }
  // On a version that doesn't cover every shot, Enlarge finishes it, with
  // what made it; otherwise it makes a new one from the boxes.
  const gap = vid && !enlarging ? versions().find((x) => x.n === version)?.missing.length ?? 0 : 0;
  $('cbenlargerest').hidden = !gap;
  if (gap) $('cbenlargerest').textContent = `Enlarge the ${gap} shot${gap > 1 ? 's' : ''} this version doesn't cover`;
  $('cbenlargestop').hidden = !enlarging;
  $('sum-cb-enlarge').textContent = method === 'ltx' ? '— 2× by LTX' : `— ${S.upFactor}×`;
}

// ---- Download ----------------------------------------------------------------

const aviUrl = () => (isVid()
  ? `/videos/${encodeURIComponent(view.id)}${version ? `/v${version}` : ''}/joined.avi`
  : `/outputs/${encodeURIComponent(view.stem)}/clip.avi`);
const baseName = () => `${isVid() ? view.id : view.stem}${isVid() && version ? `-v${version}` : ''}`;
const audioUrl = () => $('cbaudio').getAttribute('src') || null;

function remembered() {
  try { return localStorage.getItem('yasdig.clipformat'); } catch { return null; }
}

function picked() {
  return $('cbformats').querySelector('input:checked')?.value ?? 'avi';
}

// The radios for what this browser can make; the last pick if it still can,
// else the first.
function drawFormats() {
  const keep = $('cbformats').querySelector('input:checked')?.value ?? remembered();
  const offer = FORMATS.filter((f) => canMake.has(f.id));
  const want = offer.some((f) => f.id === keep) ? keep : offer[0].id;
  $('cbformats').innerHTML = offer.map((f) => `<label><input type="radio" name="cbformat" value="${f.id}"
    ${f.id === want ? 'checked' : ''}> ${f.label}</label>`).join('');
  syncDownload();
}

// What the pick makes, and the boxes that apply to it.
function syncDownload() {
  const fmt = picked();
  const f = FORMATS.find((x) => x.id === fmt);
  $('cbformatnote').textContent = f.note;
  $('cbencrow').hidden = fmt === 'avi';
  $('cbsize').parentElement.hidden = fmt === 'gif';
  $('sum-cb-download').textContent = `— ${f.label}`;
  const missing = FORMATS.filter((x) => !canMake.has(x.id)).map((x) => x.label);
  if (!exporting) {
    const cv = $('cbcanvas');
    const [w, h] = fmt === 'avi' ? [cv.width, cv.height] : outSize(fmt, cv.width, cv.height, parseFloat($('cbscale').value) || 1);
    $('cbdlnote').textContent = (cv.width ? `${w}×${h}. ` : '')
      + (missing.length ? `This browser can't make ${missing.join(' or ')} — another browser may.` : '');
  }
}

// Ask the browser once a version's size is known.
async function probeFormats() {
  const mine = seq;
  const cv = $('cbcanvas');
  let got = new Set(['avi', 'gif']);
  try {
    got = await available(cv.width, cv.height, fps());
  } catch { /* GIF and AVI need no encoder */ }
  if (mine !== seq) return;
  canMake = got;
  drawFormats();
}

function saveBlob(blob, name) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60000);
}

// The Download button says what a download is doing — the panel's note
// can be scrolled out of sight under the boxes, the button can't. `null`
// gives it back.
function working(label) {
  const b = $('cbdlgo');
  b.disabled = label !== null;
  b.textContent = label ?? 'Download';
}

async function download() {
  const fmt = picked();
  try { localStorage.setItem('yasdig.clipformat', fmt); } catch { /* a convenience */ }
  const f = FORMATS.find((x) => x.id === fmt);
  const why = isVid() && joinProblem(version);
  if (why) {
    $('cberr').textContent = `Download: ${why}`;
    return;
  }
  // A saved clip's AVI is a file already. A Video's is joined by the server
  // on request — seconds of JPEG encoding for a long one — so it is fetched
  // here, with a line saying so, and a refusal shows as the server's words.
  if (fmt === 'avi' && !isVid()) {
    const a = document.createElement('a');
    a.href = aviUrl();
    a.download = `${baseName()}.avi`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    return;
  }
  if (fmt === 'avi') {
    exporting = new AbortController();
    working('Joining…');
    $('cbdlstop').hidden = false;
    $('cberr').textContent = '';
    $('cbdlnote').textContent = `Joining ${frameCount()} frames into one AVI…`;
    try {
      const r = await fetch(aviUrl(), { signal: exporting.signal });
      if (!r.ok) throw new Error((await r.json().catch(() => null))?.error ?? `the server answered ${r.status}`);
      const blob = await r.blob();
      saveBlob(blob, `${baseName()}.avi`);
      $('cbdlnote').textContent = `AVI, ${(blob.size / 1e6).toFixed(1)} MB.`;
    } catch (e) {
      if (e?.name === 'AbortError') $('cbdlnote').textContent = 'Cancelled.';
      else { $('cbdlnote').textContent = ''; $('cberr').textContent = `Download: ${e.message || e}`; }
    } finally {
      exporting = null;
      working(null);
      $('cbdlstop').hidden = true;
    }
    return;
  }
  const v = version;
  exporting = new AbortController();
  working('Starting…');
  $('cbdlstop').hidden = false;
  $('cberr').textContent = '';
  $('cbdlnote').textContent = 'Starting…';
  try {
    const { blob, note } = await exportClip({
      fmt,
      frameUrl: (k) => frameUrl(v, k),
      frames: frames.length || frameCount(),
      fps: fps(),
      width: $('cbcanvas').width,
      height: $('cbcanvas').height,
      // A GIF has no sound to carry.
      audioUrl: fmt === 'gif' ? null : audioUrl(),
      scale: parseFloat($('cbscale').value) || 1,
      size: $('cbsize').value,
      signal: exporting.signal,
      onProgress: (done, total) => {
        $('cbdlnote').textContent = `Encoding frame ${done} of ${total}…`;
        working(`Encoding ${Math.round((100 * done) / total)}%…`);
      },
    });
    saveBlob(blob, `${baseName()}.${f.ext}`);
    $('cbdlnote').textContent = `${f.label}, ${(blob.size / 1e6).toFixed(1)} MB.${note ? ` ${note}` : ''}`;
  } catch (e) {
    if (e?.name === 'AbortError') $('cbdlnote').textContent = 'Cancelled.';
    else { $('cbdlnote').textContent = ''; $('cberr').textContent = `Download: ${e.message || e}`; }
  } finally {
    exporting = null;
    working(null);
    $('cbdlstop').hidden = true;
  }
}

// A Video's sound is the server's join of its shots' (`joined.wav`): one
// track, each shot's sound at its frames and silence under a silent one. The
// query names the shots as they are, so a shot drawn again isn't heard stale.
function sound() {
  const a = $('cbaudio');
  const has = isVid() ? shots().some(({ c }) => c.audio) : view.info.audio;
  const key = shots().map(({ c }) => `${c.id}.${c.created_ms}.${c.frames}`).join('-');
  const src = !has ? '' : isVid()
    ? `/videos/${encodeURIComponent(view.id)}/joined.wav?${key}`
    : `/outputs/${encodeURIComponent(view.stem)}/audio.wav`;
  if (src) a.src = src; else a.removeAttribute('src');
}

function open() {
  base = [];
  at = 0;
  $('cberr').textContent = '';
  $('cbprog').textContent = '';
  $('cbcompare').checked = false;
  sound();
  onOpen();
  loadLtxModels();
  if (!$('clipbox').open) $('clipbox').showModal();
  panels.layout();
}

// A Video's generated shots in storyboard order, named as the storyboard
// names them — for a Video read from a listing rather than off the stage.
function shotsOfVideo(v) {
  const many = v.scenes.length > 1;
  return v.scenes.flatMap((s, si) => s.shots.map((id, k) => ({ id, label: many ? `Scene ${si + 1} · Shot ${k + 1}` : `Shot ${k + 1}` })))
    .map(({ id, label }) => ({ c: (v.clips ?? []).find((c) => c.id === id), label }))
    .filter(({ c }) => c && c.rendered !== false && c.frames);
}

// Open Video `id` whole — `list` its generated shots in order (`{c, label}`),
// as the stage has them — on its newest version that covers every shot, the
// playhead at frame `from` of the whole.
export function openVideoView(id, list, from = 0) {
  if (!list.length) return;
  view = { kind: 'video', id, shots: list.map((x) => ({ ...x })) };
  open();
  at = from;
  const whole = versions().filter((v) => !v.missing.length).at(-1);
  show(whole?.n ?? 0);
}

// Open a clip saved to output, from the Outputs tab. Saved as `{id}` or
// `{id}-v{n}`, so while the clip it came from is still on the Video tab it
// opens as that clip — Enlarge and History and all — on that version;
// otherwise the file only plays.
export async function openOutputClip(stem) {
  const [, id, v] = stem.match(/^(.*?)(?:-v(\d+))?$/);
  try {
    // A saved Video's stem is its id (a shot saved before Videos were
    // joined, the shot's: it opens as its Video).
    const videos = await getJSON('/videos');
    const found = Array.isArray(videos)
      ? videos.find((x) => x.id === id || (x.clips ?? []).some((c) => c.id === id)) : null;
    const list = found ? shotsOfVideo(found) : [];
    if (list.length) {
      view = { kind: 'video', id: found.id, shots: list };
      open();
      const n = Number(v ?? 0);
      show(versions().some((x) => x.n === n) ? n : 0);
      return;
    }
  } catch { /* the file alone, below */ }
  try {
    const info = await getJSON(`/outputs/${encodeURIComponent(stem)}/video.json`);
    view = { kind: 'output', stem, info };
  } catch (e) {
    $('oerr').textContent = `Could not open ${stem}: ${e.message || e}`;
    return;
  }
  open();
  show(0);
}

// What the boxes ask for, as `/videos/enlarge` takes it.
function asked() {
  return {
    upscale: method === 'ltx' ? 2 : parseFloat($('cbfactor').value) || S.upFactor,
    sharpness: parseFloat($('cbsharp').value) || 0,
    clarity: parseFloat($('cbclarity').value) || 0,
    ltx: method === 'ltx' ? {
      model: $('cbltxmodel').value,
      strength: parseFloat($('cbltxstrength').value) || 0.45,
      steps: parseInt($('cbltxsteps').value, 10) || 8,
    } : null,
  };
}

// What made version `v`, to enlarge the shots it lacks the same way.
const madeBy = (v) => ({
  upscale: v.factor, sharpness: v.sharpness, clarity: v.clarity,
  ltx: v.ltx ? { model: v.ltx.model, strength: v.ltx.strength, steps: v.ltx.steps } : null,
});

// An enlarge in flight: the shots go one queued job at a time. An FSR job
// can't be killed (it finishes, `jobs.md`), so the loop has its own stop,
// read between shots; *Stop* sets it and stops an LTX shot drawing.
let enlarging = null; // { stopped }

// Enlarge the Video: a new version from the boxes (`rest` false), or the
// shots the version on screen doesn't cover, as it was made (`rest` true).
// Every shot is written as the same `n`: one past any version a shot has.
async function enlarge(rest) {
  if (!isVid() || enlarging) return;
  const ver = rest ? versions().find((x) => x.n === version) : null;
  if (rest && !ver?.missing.length) return;
  const n = ver ? ver.n : Math.max(0, ...shots().flatMap(({ c }) => (c.versions ?? []).map((x) => x.n))) + 1;
  const todo = ver ? ver.missing : shots();
  const how = ver ? madeBy(ver) : asked();
  // The Video it started on: the dialog may be closed, or show another,
  // before the last shot is done.
  const mine = view;
  const run = { stopped: false };
  enlarging = run;
  setBusy('cbenlarge', 'Enlarging…', 'enlarge');
  syncRail();
  $('cberr').textContent = '';
  soonJobs();
  const t = performance.now();
  let done = 0;
  try {
    for (const [k, s] of todo.entries()) {
      if (run.stopped) break;
      $('cbprog').textContent = todo.length > 1 ? `${s.label}: ${k + 1} of ${todo.length}…` : `${s.label}…`;
      let data;
      try {
        data = await post('/videos/enlarge', { id: s.c.id, version: n, ...how });
      } catch (e) {
        throw new Error(`${s.label}: ${e.message || e}`);
      }
      const at = mine.shots.findIndex((x) => x.c.id === s.c.id);
      if (at >= 0) mine.shots[at] = { ...mine.shots[at], c: data.clip };
      onClip(data.clip);
      done++;
    }
    if (view === mine) {
      $('cbprog').textContent = `Version ${n}: ${done} shot${done === 1 ? '' : 's'} in ${((performance.now() - t) / 1000).toFixed(1)} s`
        + (done < todo.length ? ` — stopped; ${todo.length - done} not enlarged` : '');
    }
  } catch (e) {
    if (view === mine) {
      $('cbprog').textContent = '';
      $('cberr').textContent = run.stopped ? 'Stopped.' : String(e.message || e);
    }
  } finally {
    enlarging = null;
    setIdle('cbenlarge');
    // What finished is kept: a stopped run's version lists what it lacks.
    if (view === mine && $('clipbox').open && versions().some((x) => x.n === n)) show(n);
    else syncRail();
  }
}

async function stopEnlarge() {
  if (!enlarging) return;
  enlarging.stopped = true;
  $('cbenlargestop').hidden = true;
  await stopAllKind('enlarge');
}

async function save() {
  if (!isVid()) return;
  const why = joinProblem(version);
  if (why) {
    $('cberr').textContent = `Save: ${why}`;
    return;
  }
  setBusy('cbsave', 'Saving…');
  $('cberr').textContent = '';
  try {
    const data = await post('/videos/save', { id: view.id, version });
    $('cbprog').textContent = `Saved → ${data.saved}`;
  } catch (e) {
    $('cberr').textContent = String(e.message || e);
  } finally {
    setIdle('cbsave');
  }
}

// Compare's line follows a drag on the picture.
function dragSplit(e) {
  if (!$('cbcompare').checked || !version) return;
  const move = (ev) => {
    const r = $('cbcanvas').getBoundingClientRect();
    split = Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width));
    draw(at);
  };
  move(e);
  const cv = $('cbcanvas');
  cv.setPointerCapture?.(e.pointerId);
  const up = () => {
    cv.removeEventListener('pointermove', move);
    cv.removeEventListener('pointerup', up);
  };
  cv.addEventListener('pointermove', move);
  cv.addEventListener('pointerup', up);
}

export function initClipView({ onClipChange, onOpen: opened } = {}) {
  if (onClipChange) onClip = onClipChange;
  if (opened) onOpen = opened;
  panels = floatPanels({
    host: $('clipbox'),
    rail: $('clipbox').querySelector('.lbrail'),
    defs: [{ id: 'cb-enlarge' }, { id: 'cb-history', corner: 'bottom-right' }, { id: 'cb-download' }],
  });
  panels.init();
  $('cbfactor').innerHTML = upOptions();
  $('cbfactor').value = S.upFactor;
  $('cbfactor').addEventListener('change', (e) => {
    setUpFactor(parseFloat(e.target.value));
    syncRail();
  });
  $('cbplay').addEventListener('click', play);
  $('cbscrub').addEventListener('input', (e) => {
    stop();
    draw(parseInt(e.target.value, 10) || 0);
  });
  $('cbhistory').addEventListener('click', (e) => {
    const b = e.target.closest('[data-v]');
    if (b) show(Number(b.dataset.v));
  });
  $('cbcompare').addEventListener('change', async () => {
    if ($('cbcompare').checked) await loadBase();
    draw(at);
  });
  $('cbcanvas').addEventListener('pointerdown', dragSplit);
  $('cbenlarge').addEventListener('click', () => enlarge(false));
  $('cbenlargerest').addEventListener('click', () => enlarge(true));
  $('cbenlargestop').addEventListener('click', stopEnlarge);
  $('cbmethodrow').addEventListener('click', (e) => {
    const b = e.target.closest('[data-method]');
    if (!b) return;
    method = b.dataset.method;
    syncMethod();
  });
  $('cbltxmodel').addEventListener('change', syncMethod);
  $('cbsave').addEventListener('click', save);
  $('cbdlgo').addEventListener('click', download);
  $('cbdlstop').addEventListener('click', () => exporting?.abort());
  $('cbformats').addEventListener('change', syncDownload);
  $('cbscale').addEventListener('change', syncDownload);
  $('cbclose').addEventListener('click', () => $('clipbox').close());
  // Closing stops playback and lets the decoded frames go.
  $('clipbox').addEventListener('close', () => {
    exporting?.abort();
    stop();
    seq++;
    frames = [];
    base = [];
  });
}
