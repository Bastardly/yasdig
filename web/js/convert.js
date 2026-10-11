import { $, esc, post, rememberFolds } from './dom.js';
import { setBusy, setIdle, cancelRun } from './busy.js';
import { soonJobs } from './jobs.js';

// ---- Convert ----------------------------------------------------------------

// The list the server published: which files exist, what each one is, and what
// every offered type would cost. The page does no arithmetic on any of it — the
// estimate is keyed by type name and looked up, because the rule for what gets
// quantized is sd.cpp's and lives in Rust where it is tested against the real
// files.
let cfiles = [];
let ctypes = [];
let crunning = false;

function gb(bytes) {
  return (bytes / (1024 ** 3)).toFixed(2) + ' GB';
}

function currentConvertFile() {
  return cfiles.find((f) => f.id === $('csource').value) || null;
}

async function loadConvertModels() {
  try {
    const data = await (await fetch('/convert-models')).json();
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
  $('ctypenote').textContent = t ? `${t.bits} bits per weight — ${t.note}` : '';
  if (!f) {
    $('crole').textContent = '';
    $('cnote').textContent = '';
    $('csize').textContent = '';
    $('cdest').textContent = '';
    return;
  }
  $('crole').textContent = `${f.role} · stored as ${f.dtype} · goes in as ${f.slot}`;
  $('cnote').textContent = f.note || '';
  const after = f.estimates[ty];
  if (after === undefined) {
    $('csize').textContent = '';
  } else {
    // Signed, and shown as a percentage, because the interesting cases go both
    // ways: q4_K halves an fp8 transformer, and f16 doubles it.
    const pct = Math.round(((after - f.bytes) / f.bytes) * 100);
    const dir = pct === 0 ? 'no change' : (pct < 0 ? `${-pct}% smaller` : `${pct}% larger`);
    $('csize').innerHTML = `<b>${gb(f.bytes)} → ~${gb(after)}</b> (${dir}, estimated)`;
  }
  // The destination is shown rather than chosen: it goes beside the source so
  // discovery finds it, and the server derives it the same way.
  const dot = f.file.lastIndexOf('.');
  $('cdest').textContent =
    `Writes ${(dot < 0 ? f.file : f.file.slice(0, dot))}-${ty}.gguf beside the original.`;
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
      `It is picked up on the next restart — the model list is read at startup.</div>`;
    // So the new file appears in the list, and the old one now shows a sibling.
    loadConvertModels();
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
  rememberFolds($('tab-convert'));
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
