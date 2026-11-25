// The download buttons and their dialogs (site/partials/downloads.html), on the
// Overview and Download pages. Moved out of the old one-page site unchanged,
// except that the GitHub API's address is one constant: `serve.mjs` answers
// it from site/mock/ when the page is opened on this computer.

// On this computer, the preview's mock of the API; `?mock=noattest` (and the
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
    for (const [os, test] of [['win', WIN], ['linux', LINUX]]) {
      const a = rel.assets.find((x) => test.test(x.name));
      if (a) askFirst(os, a, rel);
    }
    const date = new Date(rel.published_at).toLocaleDateString(undefined,
      { day: 'numeric', month: 'short', year: 'numeric' });
    document.getElementById('release').textContent =
      `${rel.tag_name}${rel.prerelease ? ' · pre-release' : ''} · ${date}`;
  } catch { /* keep the Releases page links */ }
})();

const esc = (s) => String(s).replace(/[&<>"]/g,
  (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const day = (iso) => new Date(iso).toLocaleDateString(undefined,
  { day: 'numeric', month: 'short', year: 'numeric' });

// What is known about this download, for its dialog: the version, the file's
// SHA-256 (GitHub's own `digest` of the asset, so it is there from the moment
// the file is), and whether GitHub attests it built the file (filled in on
// open). No VirusTotal line since 2026-11-25: the release pipeline still
// scans (scan.yml), but a machine-learning guess shown as "1 engine flagged it"
// reads as "a trojan" to most visitors, whatever the note under it says.
function checked(os, asset, rel) {
  const hash = (asset.digest || '').replace(/^sha256:/, '');
  const check = os === 'win'
    ? `certutil -hashfile ${asset.name} SHA256` : `sha256sum ${asset.name}`;
  return `<h3>Checked</h3>
    <p><b>Version:</b> ${esc(rel.tag_name)} <span class="sub">· ${day(rel.published_at)}</span></p>
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
function askFirst(os, asset, rel) {
  const dlg = document.getElementById(`${os}-dialog`);
  const go = document.getElementById(`${os}-go`);
  const box = document.getElementById(`${os}-checked`);
  go.href = asset.browser_download_url;
  box.innerHTML = checked(os, asset, rel);
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
