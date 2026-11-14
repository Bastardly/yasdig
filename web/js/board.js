import { $, esc, post } from './dom.js';
import { soonJobs } from './jobs.js';
import { castHtml, castLabel, framesLabel, keyMissing, rowKeyHtml } from './chatview.js';
import {
  addCastToShots, castEntry, clipCast, followVideo, holdVideo, onRowsChange, removeCastFromShots, saveStage, setBoard, videoSettings, videoTakes,
} from './video.js';
import { currentShotId, currentVideo, flushStory } from './storyboard.js';
import { choosePicture, chooseSound, upload } from './source-slot.js';
import { askForSound, playSound } from './sounds.js';
import { chooseCastEntry } from './castpicks.js';
import { openCastEditor } from './casteditor.js';

// ---- A film's Video, on the Video tab -----------------------------------------------
//
// A Video chat's film (`agent::chat::Film`) writes its own Video
// (`agent::filmvideo`): scenes, shots, each shot's parts as a plan, or as a
// draft once the shot is generated. The storyboard *is* the board — the
// person edits it as any Video, and the server reads it back before every
// turn and press. This module only follows: when a turn or a press lands it
// stages the film's Video afresh (`followVideo`), and while the chat's job
// runs that Video's panels are inert (`holdVideo`), so nothing typed then is
// lost under the agent's write. And the Cast panel, below.

let got = null; // the Video Assistant's chat shown, `{ chat, toolkit }`
let show = null; // the Assistant's `show`, for an answer that is the chat
let seen = null; // what the film's Video was last followed at: chat, video, busy
let pickFor = null; // the cast member an upload is for

const film = () => got?.chat.film ?? null;
// `session::MAX_CAST`: past it the server refuses an added member.
const MAX_CAST = 6;
const busy = () => ['queued', 'running'].includes(got?.chat.session.state);
const chatId = () => got?.chat.id ?? null;
const err = (msg) => { $('casterr').textContent = msg ?? ''; };
// The person is in a box of the Cast panel: a redraw would take it away.
const typing = (root) => root.contains(document.activeElement) && document.activeElement.matches('input, textarea');
const owns = () => !!film()?.video && film().video === currentVideo()?.id;
// The Generation panel and the Video it is of: the film's Video is written
// with it only when that is the one open (`routes::from_film_video`).
const panel = () => ({ settings: videoSettings(), video_id: currentVideo()?.id ?? null });

// Part 1 of the open shot, as the film has it: its first-frame buttons.
function partOfShot(id) {
  const f = film();
  return f ? f.shots.findIndex((s) => s.shot === id) : -1;
}

const HOOKS = {
  owns,
  rowExtras(k, refs) {
    if (!owns() || busy() || k !== 0) return '';
    const f = film();
    const i = partOfShot(currentShotId());
    if (i < 0) return '';
    const s = f.shots[i];
    const info = {
      fresh: true,
      key: s.key_stem ?? null,
      no_key: !!s.no_key,
      key_drawn: s.keyframe?.kind === 'item',
      key_stale: !!s.key_stem && keyMissing(f, s, i),
    };
    return rowKeyHtml(info, refs);
  },
  framesLabel: () => (owns() && !busy() ? framesLabel(film()) : null),
};

// The Video Assistant's chat, as it was just shown (or null). `showFn` is how
// an answer that is the chat reaches the panel again.
export function follow(g, showFn) {
  show = showFn;
  got = g && g.chat.context === 'video' ? g : null;
  drawCast();
  const f = film();
  setBoard(f ? HOOKS : null);
  const id = got?.chat.id ?? null;
  const opened = seen?.chat !== id;
  // While the job runs: its Video held, and nothing recorded as seen — the
  // first film's Video is named mid-job, and must still open when it ends.
  if (busy()) {
    if (f?.video) holdVideo(f.video);
    seen = { chat: id, video: opened ? null : seen.video, busy: true };
    return;
  }
  if (!f?.video) {
    holdVideo(null);
    seen = id ? { chat: id, video: null, busy: false } : null;
    return;
  }
  // A chat opened, a film's new Video, or a job that just ended: the Video
  // as the server now has it, the hold let go only once it is restaged.
  const fresh = opened || seen.video !== f.video;
  const ended = !!seen?.busy;
  seen = { chat: id, video: f.video, busy: false };
  if (fresh || ended) followVideo(f.video, { open: fresh }).finally(() => holdVideo(null));
  else holdVideo(null);
}

// ---- The Cast panel -------------------------------------------------------------

// No film: the clip's own cast — who the shots' references name — and *Add
// from cast…*, which puts a saved member on the shots that start fresh
// (`addCastToShots`). Only H3 follows references and voices.
function clipCastHtml() {
  const takes = videoTakes();
  const list = clipCast();
  const intro = '<p class="sub">No film here: the Assistant writes one when you ask it for a film. A clip’s cast is who its shots’ references show.</p>';
  if (!takes.references) {
    return `${intro}<p class="sub">This video model can’t follow reference pictures, so a cast member can’t go on its shots. Write their look into a shot’s prompt, or start a shot from their picture (Start from image → Choose… → Cast).</p>`;
  }
  const rows = list.map((m) => `<div class="storymember">
      <div class="storypic"><img src="/outputs/${encodeURIComponent(m.stem)}/thumb.png" alt=""></div>
      <div class="storyfields"><b>${esc(m.name)}</b>
        <span class="sub">In shot${m.shots.length === 1 ? '' : 's'} ${m.shots.map((k) => k + 1).join(', ')}${
          m.voice ? ' · with a voice' : ''}</span>
        <div class="btnrow"><button type="button" class="btn" data-clip-uncast="${esc(m.name)}"
          title="Take ${esc(m.name)} off every shot: their pictures, voice and name">Remove</button></div>
      </div></div>`).join('');
  return `${intro}${rows ? `<div class="storycast">${rows}</div>`
    : '<p class="sub">No one yet. Add from cast… puts a saved member on the shots that start fresh, or pick one in a shot’s References.</p>'}`;
}

function drawCast() {
  const f = film();
  if (!f) {
    $('castdraw').hidden = true;
    $('castfrom').hidden = !videoTakes().references;
    const html = clipCastHtml();
    if ($('castbody').dataset.html !== html) {
      $('castbody').innerHTML = html;
      $('castbody').dataset.html = html;
    }
    return;
  }
  delete $('castbody').dataset.html;
  const label = !busy() && castLabel(f);
  $('castdraw').hidden = !label;
  if (label) $('castdraw').textContent = label;
  $('castfrom').hidden = !f || busy() || f.cast.length >= MAX_CAST;
  const body = $('castbody');
  if (typing(body)) {
    body.dataset.stale = '1';
    return;
  }
  delete body.dataset.stale;
  body.innerHTML = castHtml(got?.chat ?? null, { busy: busy(), off: got?.toolkit.film_off, models: got?.toolkit.models ?? [] });
}

const shotList = (ns) => (ns.length === 1 ? `Shot ${ns[0]}` : `Shots ${ns.slice(0, -1).join(', ')} and ${ns.at(-1)}`);

// A saved member onto the shots, as its picture's and voice's output copies.
async function addToClip(id) {
  err('');
  try {
    const c = await castEntry(id);
    const { full, framed } = addCastToShots(c);
    const said = [];
    if (full.length) said.push(`${shotList(full)} had no room for ${c.name}: a shot follows at most 4 pictures and 3 sounds.`);
    if (framed.length) {
      said.push(`${shotList(framed)} ${framed.length === 1 ? 'starts' : 'start'} from a first frame, so ${c.name} is named `
        + 'there, not pictured: clear the frame for a shot to follow their picture.');
    }
    if (said.length) err(said.join(' '));
  } catch (ex) {
    err(String(ex.message || ex));
  }
}

// One edit of the film (`/chats/film`): the cast, a draw press. The open
// shot is saved first, so the server reads the storyboard as it is; an edit
// that draws nothing was written into the Video, which is followed.
async function edit(fields) {
  if (!got) return;
  err('');
  await flushBoard();
  try {
    const g = await post('/chats/film', { id: chatId(), ...fields, ...panel() });
    show?.(g);
    soonJobs();
    if (!busy() && film()?.video) followVideo(film().video);
  } catch (e) {
    err(String(e.message || e));
    drawCast();
  }
}

// Before a message is sent: the open shot saved, or the turn (which reads
// the storyboard) would miss what was typed a moment ago.
export async function flushBoard() {
  await saveStage();
  await flushStory();
}

// From the chat's storyboard card: the film's Video on the stage, the
// storyboard and Cast panels open.
export async function showBoard() {
  const id = film()?.video;
  if (!id) return;
  if (!(await followVideo(id, { open: true }))) {
    err('The film’s video is gone; the next storyboard turn makes another.');
    return;
  }
  $('panel-vid-board').open = true;
  $('panel-vid-cast').open = true;
}

// From the chat's storyboard card: the Cast panel alone — who the film
// is about, their pictures and (on H3) voices — without moving the stage.
export function showCast() {
  $('panel-vid-cast').open = true;
}

export function initBoard() {
  const body = $('castbody');
  body.addEventListener('change', (e) => {
    const t = e.target;
    if (t.matches('[data-film-model]')) edit({ image_model: t.value });
    else if (t.dataset.filmMember != null) edit({ member: Number(t.dataset.filmMember), [t.dataset.field]: t.value });
  });
  body.addEventListener('focusout', () => {
    if (body.dataset.stale) setTimeout(drawCast, 0);
  });
  body.addEventListener('click', (e) => {
    const b = e.target.closest('button');
    if (!b) return;
    const d = b.dataset;
    if (d.clipUncast != null) {
      removeCastFromShots(d.clipUncast);
      return;
    }
    const picture = (k) => (stem) => edit({ member: k, picture: { kind: 'output', id: stem } });
    if (d.filmPick != null) choosePicture(picture(Number(d.filmPick)));
    // Drawn on the same press, as a person pressing it means.
    else if (d.filmRedraw != null) edit({ member: Number(d.filmRedraw), picture: null, draw: true });
    else if (d.filmUpload != null) {
      pickFor = Number(d.filmUpload);
      $('castfile').click();
    } else if (d.filmVoiceplay != null) playSound(d.filmVoiceplay);
    else if (d.filmVoicepick != null) {
      const k = Number(d.filmVoicepick);
      chooseSound((stem) => edit({ member: k, voice: stem }));
    } else if (d.filmVoiceup != null) {
      const k = Number(d.filmVoiceup);
      askForSound((s) => edit({ member: k, voice: s.stem }), err);
    } else if (d.filmNovoice != null) edit({ member: Number(d.filmNovoice), voice: null });
    else if (d.filmTocast != null) {
      // The editor starts from the member: its words, and its picture,
      // turnaround and voice as outputs. Save is the person's, as always.
      const m = film()?.cast[Number(d.filmTocast)];
      if (!m) return;
      openCastEditor(null, {
        name: m.name, kind: m.kind, look: m.look, speaks: m.speaks ?? '',
        picture: m.stem ?? undefined, sheet: (m.stem && m.sheet_stem) || undefined, voice: m.voice ?? undefined,
      });
    }
  });
  $('castfrom').addEventListener('click', () => {
    if (film()) {
      const names = film().cast.map((m) => m.name.toLowerCase());
      chooseCastEntry((id) => edit({ from_cast: id }), (e) => names.includes(e.name.trim().toLowerCase()),
        'Everyone in your cast is in this film already.');
      return;
    }
    const names = clipCast().map((m) => m.name.toLowerCase());
    chooseCastEntry(addToClip, (e) => names.includes(e.name.trim().toLowerCase()),
      'Everyone in your cast is on these shots already.');
  });
  onRowsChange(() => { if (!film()) drawCast(); });
  drawCast();
  $('castfile').addEventListener('change', async () => {
    const f = $('castfile').files[0];
    $('castfile').value = '';
    if (!f || pickFor == null) return;
    try {
      await edit({ member: pickFor, picture: { kind: 'output', id: await upload(f) } });
    } catch (ex) {
      err(String(ex.message || ex));
    }
  });
  $('castdraw').addEventListener('click', () => edit({ draw: true, frames: false }));
  // Part 1's first-frame buttons (`rowKeyHtml`) and the Shot footer's.
  $('vshots').addEventListener('click', (e) => {
    const b = e.target.closest('[data-board-redraw], [data-board-addkey]');
    const shot = currentShotId();
    if (!b || !owns() || !shot) return;
    if (b.dataset.boardRedraw != null) {
      if (confirm('Draw this shot’s first frame again? The one there now is replaced.')) {
        edit({ shot_id: shot, redraw_keyframe: true, draw: true });
      }
    } else edit({ shot_id: shot, no_keyframe: false, draw: true });
  });
  $('vframes').addEventListener('click', () => edit({ draw: true, frames: true }));
  // The tab's model is the one the film is shot on: H3 follows the cast's
  // pictures, the rest start a shot from a first frame.
  $('vmodel').addEventListener('change', async () => {
    if (!owns() || busy() || !$('vmodel').value || $('vmodel').value === film()?.video_model) return;
    try {
      // A lock let go on the way (`askLetGo`) lands first: the film is
      // written with the new model, not the old lock's.
      await flushStory();
      show?.(await post('/chats/board', { id: chatId(), video: $('vmodel').value, ...panel() }));
    } catch (e) {
      $('verr').textContent = String(e.message || e);
    }
  });
}
