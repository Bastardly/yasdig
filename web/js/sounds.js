import { $, esc } from './dom.js';
import { chooseSound } from './source-slot.js';
import { encodeWav, fitSound, SOUND_RATE } from './wavenc.js';

// Sounds: `work/output/` `.wav` outputs a MiniMax H3 shot follows
// (`--ref-audio`) — a voice, a sound. Uploaded through the browser's own
// decoder, chosen in the shared `#picker`, played from their tile. The
// Video tab's *Sounds…* (`editSounds`) and a film character's voice both
// end in stems.

// Whatever the browser can play, as 32 kHz stereo PCM (what H3's audio VAE
// takes; sd.cpp resamples anything else), cut to H3's limit.
export async function uploadSound(file) {
  if (!file) throw new Error('no file');
  let decoded;
  const ctx = new AudioContext();
  try {
    decoded = await ctx.decodeAudioData(await file.arrayBuffer());
  } catch {
    throw new Error('the browser could not read that as a sound');
  } finally {
    ctx.close?.();
  }
  const fit = fitSound(decoded.duration);
  if (fit.error) throw new Error(fit.error);
  const frames = Math.round(fit.seconds * SOUND_RATE);
  const off = new OfflineAudioContext(2, frames, SOUND_RATE);
  const src = off.createBufferSource();
  src.buffer = decoded;
  src.connect(off.destination);
  src.start();
  const out = await off.startRendering();
  const wav = encodeWav([out.getChannelData(0), out.getChannelData(1)], SOUND_RATE);
  const resp = await fetch('/upload-audio', { method: 'POST', headers: { 'Content-Type': 'audio/wav' }, body: wav });
  const data = await resp.json();
  if (!resp.ok || data.error) throw new Error(data.error || ('HTTP ' + resp.status));
  return { stem: data.stem, seconds: data.seconds, trimmed: fit.trimmed };
}

// One sound plays at a time; a second press on the same one stops it.
let playing = null;
export function playSound(stem) {
  const same = playing?.stem === stem;
  playing?.audio.pause();
  playing = null;
  if (same) return;
  const audio = new Audio(`/outputs/${encodeURIComponent(stem)}/sound.wav`);
  playing = { stem, audio };
  audio.addEventListener('ended', () => { if (playing?.audio === audio) playing = null; });
  audio.play().catch(() => { playing = null; });
}

// A file input that ends in an upload: `onDone({stem, seconds, trimmed})`
// or `onErr(message)`.
export function askForSound(onDone, onErr) {
  const input = document.createElement('input');
  input.type = 'file';
  input.accept = 'audio/*';
  input.addEventListener('change', async () => {
    try {
      onDone(await uploadSound(input.files[0]));
    } catch (e) {
      onErr(e.message);
    }
  });
  input.click();
}

// ---- The Sounds dialog -------------------------------------------------------
//
// `#soundsdlg`, borrowed by a Video tab shot as `#refsdlg` is: up to three
// sounds, in the order H3 numbers them `<Audio 1>`…, each maybe the voice of
// someone the shot's lines name. `onDone(stems, voices)` on Done.
const MAX = 3;
let picked = [];
let voices = [];
let done = null;

function drawSounds() {
  $('soundslist').innerHTML = picked.map((s, i) => `
    <li><b>&lt;Audio ${i + 1}&gt;</b> <span class="sub">${esc(s)}</span>
      <button type="button" class="btn" data-play="${esc(s)}">Play</button>
      <button type="button" class="btn" data-drop="${i}">Remove</button>
      <input type="text" data-voice="${i}" value="${esc(voices[i] ?? '')}" placeholder="Voice of — a name the lines use"
        aria-label="Whose voice sound ${i + 1} is"></li>`).join('')
    || '<li class="sub">No sounds yet.</li>';
  $('soundschoose').hidden = $('soundsupload').hidden = picked.length >= MAX;
}

export function editSounds(stems, onDone, note, voiceOf = []) {
  picked = [...stems];
  voices = picked.map((_, i) => voiceOf[i] ?? '');
  done = onDone;
  $('soundsnote').textContent = note;
  $('soundserr').textContent = '';
  drawSounds();
  $('soundsdlg').showModal();
}

export function initSounds() {
  $('soundslist').addEventListener('click', (e) => {
    const play = e.target.closest('[data-play]');
    if (play) { playSound(play.dataset.play); return; }
    const drop = e.target.closest('[data-drop]');
    if (drop) {
      picked.splice(Number(drop.dataset.drop), 1);
      voices.splice(Number(drop.dataset.drop), 1);
      drawSounds();
    }
  });
  $('soundslist').addEventListener('input', (e) => {
    if (e.target.dataset.voice != null) voices[Number(e.target.dataset.voice)] = e.target.value;
  });
  const add = (stem) => {
    if (!picked.includes(stem) && picked.length < MAX) { picked.push(stem); voices.push(''); }
    drawSounds();
  };
  $('soundschoose').addEventListener('click', () => chooseSound(add));
  $('soundsupload').addEventListener('click', () => {
    $('soundserr').textContent = '';
    askForSound((s) => {
      add(s.stem);
      if (s.trimmed) $('soundserr').textContent = `Kept its first ${s.seconds.toFixed(0)} seconds: H3 takes no more.`;
    }, (msg) => { $('soundserr').textContent = msg; });
  });
  $('soundsdone').addEventListener('click', () => {
    $('soundsdlg').close();
    done?.(picked, voices.map((v) => v.trim()));
    done = null;
  });
  // Escape is a cancel: nothing changes.
  $('soundsdlg').addEventListener('cancel', () => { done = null; });
}
