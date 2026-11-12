// ---- The storyboard: a Video's scenes and shots -----------------------------
//
// Video › Scene › Shot › Part. The Storyboard panel lists the open Video's
// scenes, each with its shots (one unbroken camera run each; a cut is the
// join between two), first to last; a shot's parts are the Shot panel's rows.
// The Scene panel holds the scene of the shot on the stage: its header, its
// description (what every part of every shot of it is conditioned on, the old
// *Every shot*) and its negative.
//
// The order and the scenes' words are the server's record (`video.json`,
// `POST /videos/story`), saved a moment after an edit; a shot is a clip of its
// own (`{video}-s{n}`), added and removed by its own routes. What a shot's
// mark says (done, outdated, not generated, changed) is computed by
// `video.js` from the shot's clip and the scene — never stored.
//
// `video.js` owns the stage and the rows, and hands its part in as hooks
// (`initStoryboard`), so this module imports nothing of it.
import { $, esc, post, randomSeed } from './dom.js';

let video = null;
let shotId = null;
let H = null;
let saveTimer = null;
let saving = Promise.resolve();
// Scenes whose description and negative the person unlocked, this visit.
const unlocked = new Set();

export const currentVideo = () => video;
export const currentShotId = () => shotId;
export const clipOf = (id) => video?.clips?.find((c) => c.id === id) ?? null;
export const sceneOf = (id = shotId) => video?.scenes.find((s) => s.shots.includes(id)) ?? null;

// "Scene 2 · Shot 3" for a shot id, numbered within its scene.
export function shotName(id) {
  const si = video?.scenes.findIndex((s) => s.shots.includes(id)) ?? -1;
  if (si < 0) return 'Shot 1';
  const k = video.scenes[si].shots.indexOf(id);
  return video.scenes.length > 1 ? `Scene ${si + 1} · Shot ${k + 1}` : `Shot ${k + 1}`;
}

// A scene whose shots were made with its words keeps them, until unlocked:
// changing them would make every one of those shots outdated.
export function sceneLocked(scene) {
  if (!scene || unlocked.has(scene.id)) return false;
  return scene.shots.some((id) => clipOf(id)?.rendered);
}

// The Video as the server last answered, onto the board. `shot` stages that
// shot; otherwise the one on the stage stays when it is still there.
export function setVideo(v, { shot = null } = {}) {
  const was = video?.id;
  video = v;
  if (!v) {
    shotId = null;
    unlocked.clear();
  } else {
    if (v.id !== was) unlocked.clear();
    const ids = v.scenes.flatMap((s) => s.shots);
    if (shot && ids.includes(shot)) shotId = shot;
    else if (!ids.includes(shotId)) shotId = ids[0] ?? null;
  }
  drawBoard();
  fillScene();
}

// The listing moved (a render landed, a star): the open Video as it now is,
// unless an edit of the order is still on its way.
export function refreshFrom(videos) {
  if (!video || saveTimer) return;
  const v = videos.find((x) => x.id === video.id);
  if (v) setVideo(v);
}

// The shot on the stage changed under a new id (a shot drawn again lands
// under a fresh one and takes the old one's place).
export function followShot(id) {
  shotId = id;
  drawBoard();
}

// ---- Drawing ---------------------------------------------------------------------

const MARK = {
  done: ['✓', 'Generated'],
  outdated: ['↻', 'Outdated: made before its scene or the video changed — Generate makes it again'],
  changed: ['✎', 'Changed since it was made — Generate shot draws the change'],
  new: ['○', 'Not generated'],
  busy: ['…', 'Generating'],
};

export function drawBoard() {
  const el = $('vboard');
  if (!el || !H) return;
  // Not while typing it: the board redraws as the panels change.
  if (video && document.activeElement !== $('vtitle')) $('vtitle').value = video.title ?? '';
  // A new video, not saved yet: its one scene and one shot, which open their
  // panels like any other (there is no rail button for either).
  if (!video) {
    const text = H.preview(null).slice(0, 100);
    el.innerHTML = `<section class="sbscene on">
      <div class="sbhead"><button type="button" class="btn sbname" title="Open this scene's description and negative">
        <b>Scene 1</b>${$('vscenetitle').value ? ` · ${esc($('vscenetitle').value)}` : ''}</button></div>
      <ol class="sbshots"><li class="sbshot new on">
        <button type="button" class="sbpick" title="Open this shot in the Shot panel">
          <span class="sbnopic"></span>
          <span class="sbline"><b>Shot 1</b> <span class="sbmark" title="${esc(MARK.new[1])}">${MARK.new[0]}</span></span>
          <span class="sbtext">${text ? esc(text) : '<i>no words yet</i>'}</span>
        </button></li></ol>
      <p class="sub">Not saved yet: it is when you generate it, press Save, or add a shot or a scene.</p>
    </section>`;
    syncHead();
    return;
  }
  const many = video.scenes.length > 1;
  const allShots = video.scenes.reduce((n, s) => n + s.shots.length, 0);
  el.innerHTML = video.scenes.map((s, si) => {
    const secs = s.shots.reduce((t, id) => t + (H.seconds(clipOf(id)) || 0), 0);
    const on = s.shots.includes(shotId);
    // A scene whose every shot is made is marked done, as its shots are.
    const done = s.shots.length > 0 && s.shots.every((id) => H.state(clipOf(id), s) === 'done');
    return `<section class="sbscene${on ? ' on' : ''}${done ? ' done' : ''}" data-scene="${esc(s.id)}">
      <div class="sbhead">
        <button type="button" class="btn sbname" title="Open this scene's description and negative">
          <b>Scene ${si + 1}</b>${s.title ? ` · ${esc(s.title)}` : ''}</button>
        <span class="sub">${secs ? `${secs.toFixed(1)} s` : ''}</span>
        <span class="sbtools">
          <button type="button" class="btn btn-icon sbsceneup" title="Move this scene up" aria-label="Move scene ${si + 1} up"${si === 0 ? ' disabled' : ''}>↑</button>
          <button type="button" class="btn btn-icon sbscenedown" title="Move this scene down" aria-label="Move scene ${si + 1} down"${si === video.scenes.length - 1 ? ' disabled' : ''}>↓</button>
          ${many ? `<button type="button" class="fpclose sbscenex" aria-label="Delete scene ${si + 1}">&times;</button>` : ''}
        </span>
      </div>
      <ol class="sbshots">${s.shots.map((id, k) => shotHtml(id, k, si, allShots)).join('')}</ol>
      <div class="btnrow sbfoot">
        <button type="button" class="btn sbaddshot">+ Shot</button>
        <button type="button" class="btn sbgen" title="Generate every shot of this scene that is not generated or is outdated">Generate scene</button>
      </div>
    </section>`;
  }).join('');
  syncHead();
}

function shotHtml(id, k, si, allShots) {
  const c = clipOf(id);
  const st = H.state(c, video.scenes[si]);
  const [glyph, tip] = MARK[st] ?? MARK.new;
  const text = H.preview(c).slice(0, 100);
  const pic = c?.thumb_width
    ? `<img src="/videos/${esc(id)}/thumb.png?${c.created_ms}" alt="" loading="lazy">`
    : '<span class="sbnopic"></span>';
  const secs = H.seconds(c);
  const first = si === 0 && k === 0;
  const last = si === video.scenes.length - 1 && k === video.scenes[si].shots.length - 1;
  // A shot drawing keeps its place: it lands under a new id there.
  const busy = st === 'busy';
  return `<li class="sbshot ${st}${id === shotId ? ' on' : ''}" data-shot="${esc(id)}">
    <button type="button" class="sbpick" title="Open this shot in the Shot panel">
      ${pic}
      <span class="sbline"><b>Shot ${k + 1}</b> <span class="sbmark" title="${esc(tip)}">${glyph}</span>
        ${secs ? `<span class="sub">${secs.toFixed(1)} s</span>` : ''}</span>
      <span class="sbtext">${text ? esc(text) : '<i>no words yet</i>'}</span>
    </button>
    <span class="sbtools">
      <button type="button" class="btn btn-icon sbup" title="Move this shot up — past the top, into the scene before" aria-label="Move shot up"${first || busy ? ' disabled' : ''}>↑</button>
      <button type="button" class="btn btn-icon sbdown" title="Move this shot down — past the bottom, into the scene after" aria-label="Move shot down"${last || busy ? ' disabled' : ''}>↓</button>
      ${allShots > 1 && !busy ? '<button type="button" class="fpclose sbx" aria-label="Delete this shot">&times;</button>' : ''}
    </span>
  </li>`;
}

// The panel's head line: what the shots share — redrawn as Generation changes.
export function syncHead() {
  if (!H) return;
  const n = video ? video.scenes.reduce((t, s) => t + s.shots.length, 0) : 1;
  const scenes = video?.scenes.length ?? 1;
  $('vsum-board').textContent = `— ${scenes} scene${scenes > 1 ? 's' : ''} · ${n} shot${n > 1 ? 's' : ''}`;
  $('vboardsum').textContent = H.summary();
}

// The Scene panel: the scene of the shot on the stage.
export function fillScene() {
  const s = sceneOf();
  const si = video?.scenes.indexOf(s) ?? 0;
  $('vscenehead').textContent = s ? `Scene ${si + 1}${s.title ? ` · ${s.title}` : ''}` : 'Scene 1';
  $('vscenetitle').value = s?.title ?? $('vscenetitle').value;
  if (s) {
    $('veverytext').value = s.every ?? '';
    $('vnegative').value = s.negative ?? '';
  }
  syncSceneLock();
}

function syncSceneLock() {
  const locked = sceneLocked(sceneOf());
  for (const id of ['veverytext', 'vnegative']) $(id).readOnly = locked;
  $('vscenelock').hidden = !locked;
}

// ---- Saving ----------------------------------------------------------------------

function storyBody(extra = {}) {
  return {
    id: video.id,
    title: video.title,
    scenes: video.scenes.map((s) => ({ ...s })),
    ...extra,
  };
}

// The order and words to the server; answers the Video as it now is.
export function saveStory(extra = {}) {
  clearTimeout(saveTimer);
  saveTimer = null;
  if (!video) return Promise.resolve(null);
  const body = storyBody(extra);
  saving = saving.then(() => post('/videos/story', body)).then((data) => {
    if (!saveTimer && data?.video && data.video.id === video?.id) {
      video = data.video;
      drawBoard();
      H.changed();
    }
    return data?.video ?? null;
  }).catch((e) => {
    $('verr').textContent = 'The storyboard was not saved: ' + (e.message || e);
    return null;
  });
  return saving;
}

function saveSoon() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => saveStory(), 500);
}

// The Video's lock let go: the next render sets a new one, and the shots
// made the old way are marked outdated.
export async function unlockVideo() {
  if (!video) return;
  const n = video.clips.filter((c) => c.rendered).length;
  if (!confirm(`${n} shot${n === 1 ? ' was' : 's were'} made with this model, size and frame rate. `
    + 'Changing them marks those shots outdated, and Generate video makes them again.')) return;
  await saveStory({ unlock: true });
  H.changed();
}

function unlockScene() {
  const s = sceneOf();
  if (!s) return;
  const n = s.shots.filter((id) => clipOf(id)?.rendered).length;
  if (!confirm(`${n} shot${n === 1 ? ' was' : 's were'} made with this scene's description and negative. `
    + 'Changing them marks those shots outdated, and Generate scene makes them again.')) return;
  unlocked.add(s.id);
  syncSceneLock();
  $('veverytext').focus();
}

// ---- Changing the order ------------------------------------------------------------

function moveShot(id, by) {
  const scenes = video.scenes;
  const si = scenes.findIndex((s) => s.shots.includes(id));
  const s = scenes[si];
  const k = s.shots.indexOf(id);
  const to = k + by;
  s.shots.splice(k, 1);
  if (to < 0) scenes[si - 1].shots.push(id);
  else if (to > s.shots.length) scenes[si + 1].shots.unshift(id);
  else s.shots.splice(to, 0, id);
  drawBoard();
  fillScene();
  saveStory();
}

function moveScene(sceneId, by) {
  const i = video.scenes.findIndex((s) => s.id === sceneId);
  const [s] = video.scenes.splice(i, 1);
  video.scenes.splice(i + by, 0, s);
  drawBoard();
  fillScene();
  saveStory();
}

async function addShot(sceneId) {
  const v = await H.ensureVideo();
  if (!v) return;
  const scene = v.scenes.find((s) => s.id === sceneId) ?? v.scenes.at(-1);
  await saving;
  const had = new Set(v.scenes.flatMap((s) => s.shots));
  try {
    const data = await post('/videos/shot', { id: v.id, scene: scene.id, plan: H.shotPlan(scene) });
    const fresh = data.video.scenes.flatMap((s) => s.shots).find((x) => !had.has(x));
    setVideo(data.video, { shot: fresh });
    H.stage(clipOf(fresh));
    H.changed();
  } catch (e) {
    $('verr').textContent = 'Could not add a shot: ' + (e.message || e);
  }
}

async function addScene() {
  const v = await H.ensureVideo();
  if (!v) return;
  const n = v.scenes.map((s) => parseInt(s.id.slice(1), 10) || 0).reduce((a, b) => Math.max(a, b), 0);
  const scene = { id: `c${n + 1}`, title: '', every: '', negative: H.defaultNegative(), seed: randomSeed(), shots: [] };
  v.scenes.push(scene);
  const saved = await saveStory();
  if (saved) await addShot(scene.id);
}

async function deleteShot(id) {
  const c = clipOf(id);
  if (c?.rendered && !confirm(`Delete ${shotName(id)}? Its frames go with it.`)) return;
  await saving;
  const ids = video.scenes.flatMap((s) => s.shots);
  const next = ids[ids.indexOf(id) + 1] ?? ids[ids.indexOf(id) - 1];
  try {
    const data = await post('/videos/shot/delete', { id });
    const staged = id === shotId;
    setVideo(data.video, { shot: staged ? next : shotId });
    if (staged) H.stage(clipOf(shotId));
    H.changed();
  } catch (e) {
    $('verr').textContent = 'Could not delete that shot: ' + (e.message || e);
  }
}

async function deleteScene(sceneId) {
  const s = video.scenes.find((x) => x.id === sceneId);
  const made = s.shots.filter((id) => clipOf(id)?.rendered).length;
  if (!confirm(`Delete scene ${video.scenes.indexOf(s) + 1} and its ${s.shots.length} shot(s)?`
    + (made ? ` ${made} of them are generated; their frames go too.` : ''))) return;
  await saving;
  const staged = s.shots.includes(shotId);
  try {
    for (const id of s.shots) await post('/videos/shot/delete', { id });
    video.scenes = video.scenes.filter((x) => x.id !== sceneId);
    const v = await saveStory();
    if (v) {
      setVideo(v, staged ? {} : { shot: shotId });
      if (staged) H.stage(clipOf(shotId));
    }
    H.changed();
  } catch (e) {
    $('verr').textContent = 'Could not delete that scene: ' + (e.message || e);
    H.reload();
  }
}

// A shot onto the stage.
function pick(id) {
  // The shot already on the stage (a Video's first, as opened) only opens its panel.
  if (id === shotId && H.staged() === id) { $('vfold-prompt').open = true; return; }
  shotId = id;
  H.stage(clipOf(id));
  drawBoard();
  fillScene();
  $('vfold-prompt').open = true;
}

export function initStoryboard(hooks) {
  H = hooks;
  $('vboard').addEventListener('click', (e) => {
    if (!video) {
      if (e.target.closest('.sbpick')) $('vfold-prompt').open = true;
      else if (e.target.closest('.sbname')) $('panel-vid-scene').open = true;
      return;
    }
    const shot = e.target.closest('.sbshot')?.dataset.shot;
    const scene = e.target.closest('.sbscene')?.dataset.scene;
    if (shot && e.target.closest('.sbpick')) pick(shot);
    else if (shot && e.target.closest('.sbup')) moveShot(shot, -1);
    else if (shot && e.target.closest('.sbdown')) moveShot(shot, 1);
    else if (shot && e.target.closest('.sbx')) deleteShot(shot);
    else if (scene && e.target.closest('.sbsceneup')) moveScene(scene, -1);
    else if (scene && e.target.closest('.sbscenedown')) moveScene(scene, 1);
    else if (scene && e.target.closest('.sbscenex')) deleteScene(scene);
    else if (scene && e.target.closest('.sbaddshot')) addShot(scene);
    else if (scene && e.target.closest('.sbgen')) H.generate(video.scenes.find((s) => s.id === scene));
    else if (scene && e.target.closest('.sbname')) {
      const s = video.scenes.find((x) => x.id === scene);
      if (!s.shots.includes(shotId) && s.shots[0]) pick(s.shots[0]);
      $('panel-vid-scene').open = true;
    }
  });
  $('vsceneadd').addEventListener('click', addScene);
  $('vtitle').addEventListener('input', () => {
    if (!video) return;
    video.title = $('vtitle').value;
    saveSoon();
  });
  // The Scene panel's boxes are the scene's: kept as typed, saved a moment
  // later; the marks follow, since a changed description outdates its shots.
  for (const id of ['vscenetitle', 'veverytext', 'vnegative']) {
    $(id).addEventListener('input', () => {
      const s = sceneOf();
      if (!s) return;
      s.title = $('vscenetitle').value;
      s.every = $('veverytext').value;
      s.negative = $('vnegative').value;
      drawBoard();
      $('vscenehead').textContent = `Scene ${video.scenes.indexOf(s) + 1}${s.title ? ` · ${s.title}` : ''}`;
      saveSoon();
    });
  }
  $('vsceneunlock').addEventListener('click', unlockScene);
  window.addEventListener('pagehide', () => {
    if (saveTimer && video) post('/videos/story', storyBody(), { keepalive: true }).catch(() => {});
  });
  drawBoard();
}
