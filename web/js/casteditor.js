import { $, esc, getJSON, post } from './dom.js';
import { S } from './state.js';
import { choosePicture, chooseSound, upload } from './source-slot.js';
import { askForSound, playSound } from './sounds.js';
import { queueChanged, soonJobs, stopKind } from './jobs.js';
import { onLook } from './viewer.js';

// ---- The cast editor ----------------------------------------------------------
//
// `#castdlg`: one cast entry (`src/cast.rs`), made or changed. Opened from the
// Cast tab (a tile, or New cast…) and from Refine & adjust's *Save as cast*.
//
// Nothing the person types reaches the server until Save, except what a draw
// needs: a draw goes into the entry's `tries/` (a new entry gets its folder,
// as a draft, at its first draw), and the strip shows every try. Save keeps
// the picked one and drops the rest; Discard drops them all, and a draft with
// them. So "Draw again" never loses a good roll, and nothing half-made is
// ever an entry a prompt can find.

// Wire kinds (`MemberKind`) to what the page calls them.
export const KIND_LABEL = { character: 'Character', place: 'Place', thing: 'Prop' };

const MODEL_KEY = 'yasdig.cast.model';

let cur = null; // { id, entry, pick, voice, onSaved } — the entry open, and the unsaved choices
let base = ''; // what "unchanged" is, as JSON
let models = [];
let drawing = false;
let audio = null;
// The prompt of the picture in Refine & adjust, as its last result said
// (`GenResult.prompt`, variables rolled): *Save as cast* starts the look from it.
let viewerPrompt = '';
const changedHooks = [];

// The Cast tab redraws when an entry is saved, discarded or drawn into.
export function onCastChanged(fn) {
  changedHooks.push(fn);
}
const changed = () => { for (const fn of changedHooks) fn(); };

const err = (msg) => { $('ceerr').textContent = msg ?? ''; };
// A try is being drawn into the entry by someone else — the Assistant: the
// editor waits for it, and closing leaves the draft where it is.
const othersDrawing = () => !!cur?.entry?.drawing && !drawing;
const kindNow = () => $('cekind').querySelector('.active')?.dataset.kind ?? 'character';
const modelNow = () => models.find((m) => m.name === $('cemodel').value) ?? null;
// The row shows on a character with a model that draws turnarounds; the
// tick says whether this draw does.
const canTurn = () => kindNow() === 'character' && !!modelNow()?.references;
const turnaround = () => canTurn() && $('ceturn').checked;

function fields() {
  return {
    name: $('cename').value.trim(),
    kind: kindNow(),
    look: $('celook').value.trim(),
    image_model: $('cemodel').value,
  };
}

const state = () => JSON.stringify({ f: fields(), pick: cur?.pick ?? null, voice: cur?.voice ?? null });
const dirty = () => !!cur && (state() !== base || (cur.entry?.tries?.length ?? 0) > 0);

// What `pick` would put on screen: the try picked, the person's picture, the
// saved one, or nothing.
function shownPicture() {
  const e = cur.entry;
  const v = e ? `?v=${e.rev}` : '';
  const p = cur.pick;
  if (p?.try != null) {
    const t = e.tries.find((x) => x.n === p.try);
    return { pic: `/cast/${e.id}/tries/${p.try}.png`, sheet: t?.sheet ? `/cast/${e.id}/tries/${p.try}-sheet.png` : null };
  }
  if (p?.output) return { pic: `/outputs/${encodeURIComponent(p.output)}/thumb.png`, sheet: null };
  if (p?.outputs) {
    return { pic: `/outputs/${encodeURIComponent(p.outputs.picture)}/thumb.png`,
      sheet: `/outputs/${encodeURIComponent(p.outputs.sheet)}/thumb.png` };
  }
  if (e?.picture) return { pic: `/cast/${e.id}/picture.png${v}`, sheet: e.sheet ? `/cast/${e.id}/sheet.png${v}` : null };
  return { pic: null, sheet: null };
}

function drawPictures() {
  const { pic, sheet } = shownPicture();
  $('cepic').hidden = !pic;
  if (pic) $('cepic').src = pic;
  $('cenopic').hidden = !!pic || drawing;
  $('cesheet').hidden = !sheet;
  if (sheet) $('cesheet').src = sheet;
  const e = cur.entry;
  const tries = e?.tries ?? [];
  const saved = e?.picture && !e.draft
    ? [`<button type="button" data-try="saved" title="The saved picture" class="${cur.pick == null ? 'active' : ''}">
        <img src="/cast/${e.id}/thumb.png?v=${e.rev}" alt="Saved"></button>`]
    : [];
  $('cetries').innerHTML = saved.concat(tries.map((t) => `<button type="button" data-try="${t.n}"
      title="Try ${t.n + 1} · seed ${t.seed} · ${esc(t.model)}" class="${cur.pick?.try === t.n ? 'active' : ''}">
      <img src="/cast/${e.id}/tries/${t.n}.png" alt="Try ${t.n + 1}"></button>`)).join('');
  $('cedraw').textContent = drawing ? 'Stop' : (pic || tries.length ? 'Draw again' : 'Draw');
  const wait = othersDrawing();
  $('cewait').hidden = !wait;
  if (wait) $('cewait').textContent = `Creating ${cur.entry.name || 'it'}, please wait…`;
  $('cedraw').disabled = wait;
  // An entry is saved with its picture: nothing to save until one is drawn
  // (or chosen). And a draw lands in the entry's tries: saved meanwhile, it
  // would be a try nobody sees until the editor is opened again.
  $('cesave').disabled = drawing || wait || !pic;
}

function drawVoice() {
  const character = kindNow() === 'character';
  $('cevoicerow').hidden = !character;
  const v = cur.voice;
  const has = v?.output || (v !== 'remove' && cur.entry?.voice);
  $('cevoicetext').textContent = v?.output ? v.output : has ? 'Saved' : 'None';
  $('cevoiceplay').hidden = !has;
  $('cevoicenone').hidden = !has;
}

function drawForm() {
  for (const b of $('cekind').querySelectorAll('button')) b.classList.toggle('active', b.dataset.kind === kindNow());
  $('ceturnrow').hidden = !canTurn();
  const e = cur.entry;
  $('cetitle').textContent = e && !e.draft ? (e.name || 'Cast') : 'New cast';
  $('cedraft').textContent = e?.draft ? 'not saved yet' : '';
  drawVoice();
  drawPictures();
}

function setKind(kind) {
  for (const b of $('cekind').querySelectorAll('button')) b.classList.toggle('active', b.dataset.kind === kind);
  drawForm();
}

async function loadModels() {
  try {
    models = (await getJSON('/models')).filter((m) => !m.fill && m.exists);
  } catch {
    models = [];
  }
  $('cemodel').innerHTML = models.map((m) => `<option value="${esc(m.name)}">${esc(m.name)} · ${esc(m.family_label)}${
    m.references ? ' (turnarounds)' : ''}</option>`).join('');
}

// The model a new entry is drawn with: the last one used here, else one
// that draws turnarounds, else the Image tab's.
function defaultModel() {
  let last = null;
  try { last = localStorage.getItem(MODEL_KEY); } catch { /* private window */ }
  const has = (n) => n && models.some((m) => m.name === n);
  if (has(last)) return last;
  const refs = models.find((m) => m.references);
  if (refs) return refs.name;
  return has($('template')?.value) ? $('template').value : models[0]?.name ?? '';
}

// Open the editor: on entry `id`, or a new one (`prefill`: `name`, `kind`,
// `look`, and output stems it starts with — `picture`, `sheet` (with a
// picture), `voice`). `onSaved(entry)` runs after a
// save: the Image tab's cast row adds what was made from it.
// `opts.chat`: the Assistant chat making this member — its words win over
// the entry's, its newest try is the one picked, and the person's edits to
// the words are written back to it (`/chats/cast`), so the agent's next
// turn changes what is on screen.
export async function openCastEditor(id = null, prefill = {}, onSaved = null, opts = {}) {
  err('');
  $('ceprog').textContent = '';
  await loadModels();
  let entry = null;
  if (id) {
    try {
      entry = await getJSON(`/cast/${encodeURIComponent(id)}`);
    } catch (e) {
      alert(String(e.message || e));
      return;
    }
  }
  const chat = opts.chat ?? null;
  cur = { id, entry, pick: null, voice: null, onSaved, chat };
  const words = (k) => (chat ? prefill[k] ?? entry?.[k] : entry?.[k] ?? prefill[k]) ?? '';
  $('cename').value = words('name');
  $('celook').value = words('look');
  const model = entry?.image_model;
  $('cemodel').value = models.some((m) => m.name === model) ? model : defaultModel();
  setKind((chat ? prefill.kind ?? entry?.kind : entry?.kind ?? prefill.kind) ?? 'character');
  if (prefill.picture) {
    cur.pick = prefill.sheet ? { outputs: { picture: prefill.picture, sheet: prefill.sheet } } : { output: prefill.picture };
  }
  if (prefill.voice) cur.voice = { output: prefill.voice };
  // A reopened draft's last try is what it was left on.
  const tries = entry?.tries ?? [];
  if ((entry?.draft || chat) && tries.length) cur.pick = { try: tries[tries.length - 1].n };
  base = JSON.stringify({ f: fields(), pick: null, voice: null });
  drawForm();
  if (!$('castdlg').open) $('castdlg').showModal();
  if (!id) $('cename').focus();
}

async function drawTry() {
  if (drawing) {
    stopKind('cast');
    return;
  }
  err('');
  const f = fields();
  if (!f.look) {
    err('Describe it first: its picture is drawn from its look.');
    $('celook').focus();
    return;
  }
  if (!f.image_model) {
    err('No image model to draw with: Get models has some.');
    return;
  }
  try { localStorage.setItem(MODEL_KEY, f.image_model); } catch { /* private window */ }
  const mine = cur;
  drawing = true;
  drawPictures();
  try {
    if (!mine.id) {
      mine.entry = await post('/cast/new', f);
      mine.id = mine.entry.id;
      changed();
    }
    soonJobs();
    const e = await post('/cast/draw', { id: mine.id, ...f, turnaround: turnaround() });
    mine.entry = e;
    const last = e.tries[e.tries.length - 1];
    if (last) mine.pick = { try: last.n };
    changed();
  } catch (e) {
    if (cur === mine) err(String(e.message || e));
  } finally {
    drawing = false;
    if (cur === mine && $('castdlg').open) drawForm();
  }
}

// The person's words, into the chat the editor is bound to: what the agent
// changes on its next turn. Debounced, so typing is one write.
let backTimer = null;
function writeBack() {
  if (!cur?.chat || !cur.id) return;
  clearTimeout(backTimer);
  const to = { id: cur.chat, cast: cur.id, ...fields() };
  delete to.image_model;
  backTimer = setTimeout(() => {
    post('/chats/cast', to).catch(() => { /* the chat moved on to another member */ });
  }, 500);
}

// The draw someone else was making: re-read the entry when the queue moves,
// and once it has landed, show the newest try.
async function refreshWhileDrawn() {
  if (!$('castdlg').open || !othersDrawing()) return;
  const mine = cur;
  try {
    const e = await getJSON(`/cast/${encodeURIComponent(mine.id)}`);
    if (cur !== mine) return;
    const landed = !e.drawing;
    mine.entry = e;
    if (landed && e.tries.length) mine.pick = { try: e.tries[e.tries.length - 1].n };
    drawForm();
    if (landed) changed();
  } catch {
    // Gone: the Cast tab's listing says so.
  }
}

async function save() {
  err('');
  const body = { ...fields(), picture: cur.pick, voice: cur.voice };
  if (cur.id) body.id = cur.id;
  const b = $('cesave');
  b.disabled = true;
  try {
    const e = await post('/cast/save', body);
    const done = cur.onSaved;
    cur = { id: e.id, entry: e, pick: null, voice: null, chat: cur.chat };
    base = state();
    changed();
    $('castdlg').close();
    done?.(e);
  } catch (e) {
    err(String(e.message || e));
  } finally {
    b.disabled = false;
  }
}

// Close, dropping what wasn't saved: the tries, and a draft with them.
async function discard() {
  const id = cur?.id;
  const draw = drawing;
  cur = null;
  $('castdlg').close();
  stopAudio();
  if (!id) return;
  // A draw still running lands in a folder that is about to go; stopped,
  // it never does.
  if (draw) await stopKind('cast');
  try {
    await post('/cast/discard', { id });
  } catch {
    // Gone already; the tab's listing says what is there.
  }
  changed();
}

function close() {
  if (!cur || othersDrawing()) {
    // A draw into it is under way elsewhere: nothing here to drop.
    cur = null;
    stopAudio();
    $('castdlg').close();
    return;
  }
  if (dirty() && !confirm('Close without saving?\n\nThe tries drawn since the last save are dropped.')) return;
  discard();
}

function stopAudio() {
  audio?.pause();
  audio = null;
}

function playVoice() {
  if (cur.voice?.output) {
    stopAudio();
    playSound(cur.voice.output);
    return;
  }
  if (audio) {
    stopAudio();
    return;
  }
  audio = new Audio(`/cast/${cur.entry.id}/voice.wav?v=${cur.entry.rev}`);
  audio.addEventListener('ended', () => { audio = null; });
  audio.play().catch(() => { audio = null; });
}

// Refine & adjust's *Save as cast*: the picture as it is shown, as a new
// entry's picture, its prompt as the look to start from. Through an upload, so the entry copies an output in like
// any picture of the person's own.
async function fromViewer() {
  if (S.imageId === null) return;
  const b = $('lbcast');
  b.disabled = true;
  $('lberr').textContent = '';
  try {
    const data = await post('/download-image', { image_id: S.imageId });
    const bytes = Uint8Array.from(atob(data.image_base64), (c) => c.charCodeAt(0));
    const stem = await upload(new File([bytes], 'cast.png', { type: 'image/png' }));
    await openCastEditor(null, { picture: stem, look: viewerPrompt });
  } catch (e) {
    $('lberr').textContent = String(e.message || e);
  } finally {
    b.disabled = false;
  }
}

export function initCastEditor() {
  $('cekind').addEventListener('click', (e) => {
    const b = e.target.closest('button[data-kind]');
    if (!b) return;
    setKind(b.dataset.kind);
    writeBack();
  });
  for (const id of ['cename', 'celook']) $(id).addEventListener('input', writeBack);
  $('cemodel').addEventListener('change', drawForm);
  queueChanged.subscribe(refreshWhileDrawn);
  $('cedraw').addEventListener('click', drawTry);
  $('cetries').addEventListener('click', (e) => {
    const b = e.target.closest('button[data-try]');
    if (!b) return;
    cur.pick = b.dataset.try === 'saved' ? null : { try: Number(b.dataset.try) };
    drawPictures();
  });
  $('cepick').addEventListener('click', () => choosePicture((stem) => {
    cur.pick = { output: stem };
    drawPictures();
  }, { cast: false }));
  $('ceupload').addEventListener('click', () => $('cefile').click());
  $('cefile').addEventListener('change', async () => {
    const f = $('cefile').files[0];
    $('cefile').value = '';
    if (!f) return;
    try {
      cur.pick = { output: await upload(f) };
      drawPictures();
    } catch (e) {
      err(String(e.message || e));
    }
  });
  $('cevoicepick').addEventListener('click', () => chooseSound((stem) => {
    cur.voice = { output: stem };
    drawVoice();
  }, { cast: false }));
  $('cevoiceup').addEventListener('click', () => askForSound((s) => {
    cur.voice = { output: s.stem };
    drawVoice();
  }, err));
  $('cevoicenone').addEventListener('click', () => {
    stopAudio();
    cur.voice = 'remove';
    drawVoice();
  });
  $('cevoiceplay').addEventListener('click', playVoice);
  $('cesave').addEventListener('click', save);
  $('cediscard').addEventListener('click', () => {
    if (dirty() && !confirm('Close without saving?\n\nThe tries drawn since the last save are dropped.')) return;
    discard();
  });
  $('ceclose').addEventListener('click', close);
  // Escape asks first, as Close does.
  $('castdlg').addEventListener('cancel', (e) => {
    e.preventDefault();
    close();
  });
  $('lbcast').addEventListener('click', fromViewer);
  onLook((d) => { viewerPrompt = d.prompt ?? ''; });
}
