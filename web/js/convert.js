import { $, esc, getJSON, post } from './dom.js';
import { setBusy, setIdle, cancelRun } from './busy.js';
import { soonJobs } from './jobs.js';
import { loadInstalled } from './getmodels.js';

// ---- Convert ----------------------------------------------------------------
//
// A panel of the Models tab (`#panel-gm-convert`, its rail button under
// Installed): the file, the weight type, Convert. The size the result will be
// is in the file's option and the type's tooltip; the result line says what it
// came to.

// The list the server published: which files exist, what each one is, and what
// every offered type would cost. The page does no arithmetic on any of it — the
// estimate is keyed by type name and looked up, because the rule for what gets
// quantized is sd.cpp's and lives in Rust where it is tested against the real
// files.
let cfiles = [];
let ctypes = [];
let crunning = false;

// Decimal, like the Models tab and the memory box: the same file must read as the
// same number of GB on every tab.
function gb(bytes) {
  return (bytes / 1e9).toFixed(2) + ' GB';
}

function currentConvertFile() {
  return cfiles.find((f) => f.id === $('csource').value) || null;
}

export async function loadConvertModels() {
  try {
    const data = await getJSON('/convert-models');
    ctypes = data.types || [];
    cfiles = data.files || [];
  } catch (e) {
    $('cerr').textContent = String(e.message || e);
    return;
  }
  $('ctype').innerHTML = ctypes
    .map((t) => `<option value="${esc(t.name)}">${esc(t.name)}</option>`)
    .join('');
  // q4_K is the default because it is the answer to the question this tab
  // exists for — a 14B video model that has to fit beside its encoder on a
  // 24 GB card. Falls back to whatever is first if the list ever changes.
  if (ctypes.some((t) => t.name === 'q4_K')) $('ctype').value = 'q4_K';

  if (!cfiles.length) {
    $('csource').innerHTML = '<option value="">No convertible model files found</option>';
    $('cgo').disabled = true;
    syncConvert();
    return;
  }
  // Under headings, in the order the server sorted them (heading, then name) —
  // the way the Image tab's model select groups by family.
  const groups = [...new Set(cfiles.map((f) => f.group))];
  $('csource').innerHTML = groups.map((g) => `<optgroup label="${esc(g)}">${
    cfiles.filter((f) => f.group === g)
      .map((f) => `<option value="${esc(f.id)}">${esc(f.file)} — ${gb(f.bytes)} ${esc(f.dtype)}</option>`)
      .join('')}</optgroup>`).join('');
  syncConvert();
}

function syncConvert() {
  const f = currentConvertFile();
  const ty = $('ctype').value;
  const t = ctypes.find((x) => x.name === ty);
  // What the panel used to spell out in lines, kept as the controls' titles.
  $('ctype').title = t ? `${t.bits} bits per weight — ${t.note}` : '';
  if (!f) { $('csource').title = ''; return; }
  const after = f.estimates[ty];
  const dot = f.file.lastIndexOf('.');
  $('csource').title = `${f.role} · stored as ${f.dtype}${f.note ? ` · ${f.note}` : ''}`
    + (after === undefined ? '' : `\n${gb(f.bytes)} → ~${gb(after)} as ${ty} (estimated)`)
    + `\nWrites ${(dot < 0 ? f.file : f.file.slice(0, dot))}-${ty}.gguf beside the original.`;
  $('cgo').disabled = crunning || f.dtype === ty;
}

async function startConvert() {
  if (crunning) return;
  const f = currentConvertFile();
  if (!f) return;
  crunning = true;
  $('cerr').textContent = '';
  $('cresult').innerHTML = '';
  $('cstop').hidden = false;
  $('cstop').disabled = false;
  $('cstop').textContent = 'Stop';
  setBusy('cgo', 'Converting…', 'convert');
  try {
    const pending = post('/convert', { source: f.id, type: $('ctype').value });
    soonJobs();
    const r = await pending;
    const mins = (r.ms / 60000).toFixed(1);
    // The estimate's only feedback. Reported next to the outcome rather than
    // quietly discarded, because it is the number the select was chosen on.
    const off = r.estimated_bytes
      ? ` (estimated ${gb(r.estimated_bytes)}, off by ` +
        `${Math.abs(Math.round(((r.bytes - r.estimated_bytes) / r.estimated_bytes) * 1000) / 10)}%)`
      : '';
    $('cresult').innerHTML =
      `<div class="meta spaced"><b>Done in ${mins} min.</b><br>` +
      `${esc(r.out)}<br>${gb(r.source_bytes)} → <b>${gb(r.bytes)}</b>${esc(off)}<br>` +
      (r.folded ? `Folded ${r.folded} fp8 scales into their weights.<br>` : '') +
      `It is in Installed, beside the original.</div>`;
    // Installed lists it, and reloads this list too.
    loadInstalled();
  } catch (e) {
    const msg = String(e.message || e);
    $('cerr').textContent = msg.includes('cancelled by request')
      ? 'Stopped. The partial file was removed.'
      : msg.includes('removed from the job queue')
        ? 'Removed from the queue before it started.'
        : msg;
  } finally {
    crunning = false;
    $('cstop').hidden = true;
    setIdle('cgo');
    syncConvert();
  }
}

export function initConvert() {
  $('cgo').addEventListener('click', startConvert);
  $('cstop').addEventListener('click', () => cancelRun('cstop', 'convert'));
  $('csource').addEventListener('change', () => {
    // A VAE wants f16, never a q-type, so picking one moves the type there;
    // anything else goes back to q4_K, the tab's default. Changing the type
    // after that is still free.
    const f = currentConvertFile();
    const want = f?.group === 'VAEs' ? 'f16' : 'q4_K';
    if (ctypes.some((t) => t.name === want)) $('ctype').value = want;
    syncConvert();
  });
  $('ctype').addEventListener('change', syncConvert);

  loadConvertModels();
}
