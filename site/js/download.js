// The download buttons and their dialogs (site/partials/downloads.html), on the
// Overview and Download pages. Moved out of the old one-page site unchanged,
// except that the GitHub API's address is one constant: `serve.mjs` answers
// it from site/mock/ when the page is opened on this computer.

// On this computer, the preview's mock of the API; `?mock=flagged` (and the
// other scenarios in site/mock/releases.mjs) picks what it answers.
const LOCAL = /^(localhost|127\.0\.0\.1|\[::1\])$/.test(location.hostname);
const API = LOCAL ? '/mock-api' : 'https://api.github.com';
const MOCK = LOCAL ? new URLSearchParams(location.search).get('mock') : null;
const api = (path) => `${API}${path}${MOCK ? `${path.includes('?') ? '&' : '?'}mock=${encodeURIComponent(MOCK)}` : ''}`;

// Which system is this? The button for it is the amber one. A Mac, a phone or a
// tablet gets neither, and a line saying there is no build for it.
(function () {
  const ua = navigator.userAgent;
  const os = /Windows/i.test(ua) ? 'win'
    : /Android|iPhone|iPad|Macintosh|Mac OS/i.test(ua) ? null
    : /Linux|X11/i.test(ua) ? 'linux' : null;
  if (os) document.getElementById(os === 'win' ? 'dl-win' : 'dl-linux').classList.add('btn-run');
  else document.getElementById('noos').hidden = false;
})();

// The newest release, pre-releases included — GitHub's own
// releases/latest/download/… link skips those, and every release is one for
// now. On any failure the buttons keep pointing at the Releases page.
(async function () {
  try {
    const r = await fetch(api('/repos/Bastardly/yasdig/releases?per_page=10'));
    if (!r.ok) return;
    // The archives (a zip and a tar.gz holding a `yasdig/` folder), or the
    // bare executables older releases carry.
    const WIN = /windows.*\.(zip|exe)$/, LINUX = /linux.*x86_64(\.tar\.gz)?$/;
    const rel = (await r.json()).find((x) => !x.draft
      && x.assets.some((a) => WIN.test(a.name) || LINUX.test(a.name)));
    if (!rel) return;
    for (const [id, test] of [['dl-win', WIN], ['dl-linux', LINUX]]) {
      const a = rel.assets.find((x) => test.test(x.name));
      if (a) document.getElementById(id).href = a.browser_download_url;
    }
    const scan = scanOf(rel.body);
    for (const [os, test] of [['win', WIN], ['linux', LINUX]]) {
      const a = rel.assets.find((x) => test.test(x.name));
      if (a) askFirst(os, a, rel, scan);
    }
    const date = new Date(rel.published_at).toLocaleDateString(undefined,
      { day: 'numeric', month: 'short', year: 'numeric' });
    document.getElementById('release').textContent =
      `${rel.tag_name}${rel.prerelease ? ' · pre-release' : ''} · ${date}`;
  } catch { /* keep the Releases page links */ }
})();

// The scan the release pipeline put in the notes (.github/scripts/
// scan-notes.sh): one `<!-- yasdig-scan {…} -->` comment, scan.json minified.
// Change both or neither. No comment, or one that doesn't parse: no scan yet.
function scanOf(body) {
  const m = /<!-- yasdig-scan (\{.*?\}) -->/.exec(body || '');
  try { return m ? JSON.parse(m[1]) : null; } catch { return null; }
}

const esc = (s) => String(s).replace(/[&<>"]/g,
  (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const day = (iso) => new Date(iso).toLocaleDateString(undefined,
  { day: 'numeric', month: 'short', year: 'numeric' });

// What is known about this download, for its dialog: the version, the file's
// SHA-256 (GitHub's own `digest` of the asset, so it is there from the moment
// the file is), VirusTotal's verdicts on the archive and on the program inside
// it, and whether GitHub attests it built the file (filled in on open).
function checked(os, asset, rel, scan) {
  const hash = (asset.digest || '').replace(/^sha256:/, '');
  const mine = (scan?.files || []).filter((f) => f.os === (os === 'win' ? 'windows' : 'linux'));
  const archive = mine.find((f) => f.kind === 'archive');
  const program = mine.find((f) => f.kind === 'program');
  const done = (f) => f && f.status === 'completed';
  const flagged = mine.filter(done).reduce((n, f) => n + f.malicious + f.suspicious, 0);
  let vt;
  // Windows isn't scanned since 2026-11-01 (scan.yml); no line rather than a
  // "not scanned yet" that never comes.
  if (os === 'win' && !mine.length) {
    vt = '';
  } else if (done(archive) || done(program)) {
    const part = (f, what) => (done(f)
      ? `<a href="${esc(f.report)}">${f.malicious + f.suspicious} of ${f.engines}</a> engines flagged ${what}` : null);
    vt = `<p${flagged ? ' class="flagged"' : ''}><b>VirusTotal:</b> ${
      [part(archive, 'the download'), part(program, 'the program inside')].filter(Boolean).join(' · ')}
      <span class="sub">— scanned ${day((archive || program).scanned)}</span></p>`
      + (flagged ? `<p class="sub">A few engines flag most new programs that aren't signed yet: their
        verdict is a guess from how the file looks, not something found in it. The report
        lists each engine and what it said.</p>` : '');
  } else {
    vt = `<p><b>VirusTotal:</b> <span class="sub">not scanned yet — the results appear a few
      minutes after a release${hash ? `, or <a href="https://www.virustotal.com/gui/file/${hash}">look the file up</a>` : ''}.</span></p>`;
  }
  const check = os === 'win'
    ? `certutil -hashfile ${asset.name} SHA256` : `sha256sum ${asset.name}`;
  return `<h3>Checked</h3>
    <p><b>Version:</b> ${esc(rel.tag_name)} <span class="sub">· ${day(rel.published_at)}</span></p>
    ${vt}
    ${hash ? `<p><b>SHA-256:</b> <button type="button" class="copy" data-copy="${hash}">Copy</button></p>
      <p class="hash"><code>${hash}</code></p>
      <p class="sub">Check yours with <code>${esc(check)}</code>: it should print the same.</p>` : ''}
    <p class="attest" hidden><b>Built by GitHub</b> from the public source —
      <a href="https://github.com/Bastardly/yasdig/attestations">the attestation</a>; check it with
      <code>gh attestation verify ${esc(asset.name)} --repo Bastardly/yasdig</code>.</p>`;
}

// Each button explains before it downloads (#win-dialog, #linux-dialog). The
// dialog's own link does the downloading, so the browser sees an ordinary click
// on a file link and nothing here has to start a download by script.
function askFirst(os, asset, rel, scan) {
  const dlg = document.getElementById(`${os}-dialog`);
  const go = document.getElementById(`${os}-go`);
  const box = document.getElementById(`${os}-checked`);
  go.href = asset.browser_download_url;
  box.innerHTML = checked(os, asset, rel, scan);
  // Whether GitHub signed for this build: asked once, on first open. Releases
  // from before the attestations simply don't show the line.
  let asked = false;
  const attest = async () => {
    if (asked || !asset.digest) return;
    asked = true;
    try {
      const r = await fetch(api(`/repos/Bastardly/yasdig/attestations/${asset.digest}`));
      if (r.ok && ((await r.json()).attestations || []).length) box.querySelector('.attest').hidden = false;
    } catch { /* no line */ }
  };
  document.getElementById(os === 'win' ? 'dl-win' : 'dl-linux').addEventListener('click', (e) => {
    if (typeof dlg.showModal !== 'function') return; // an old browser: just download
    e.preventDefault();
    dlg.showModal();
    go.focus();
    attest();
  });
  go.addEventListener('click', () => dlg.close());
  dlg.querySelector('[data-cancel]').addEventListener('click', () => dlg.close());
  box.addEventListener('click', async (e) => {
    const b = e.target.closest('[data-copy]');
    if (!b) return;
    try {
      await navigator.clipboard.writeText(b.dataset.copy);
      b.textContent = 'Copied';
    } catch { b.textContent = 'Select it below'; }
    setTimeout(() => { b.textContent = 'Copy'; }, 1500);
  });
  // A click on the backdrop closes it too; the card itself doesn't.
  dlg.addEventListener('click', (e) => { if (e.target === dlg) dlg.close(); });
}
