// What `serve.mjs` answers in place of api.github.com, so the download dialogs
// can be seen in every state they have without a release to make them. Picked
// by the page's `?mock=` (download.js passes it on); never copied into _site/.
//
// The release's shape is the slice of GitHub's that download.js reads: tag,
// dates, `prerelease`, `body` (with the scan comment scan-notes.sh writes), and
// assets with `name`, `browser_download_url` and `digest`.

export const SCENARIOS = {
  clean: 'scanned, nothing flagged, attested',
  flagged: 'one engine of 69 flags the Linux program',
  unscanned: 'released minutes ago: no scan comment yet',
  oldwin: 'an older release whose scan still lists the Windows files',
  prerelease: 'the newest is a pre-release',
  noattest: 'no build attestation (a release from before them)',
  nolinux: 'only the Windows archive is up yet',
  down: 'the GitHub API answers 500: the buttons keep the Releases link',
};

const HASH_WIN = 'a3f1c0de9b7e4d21c6f5a8b90e1d2c3b4a5968778695a4b3c2d1e0f9a8b7c6d5';
const HASH_LINUX = '5e4d3c2b1a0f9e8d7c6b5a49382716f5e4d3c2b1a0f9e8d7c6b5a49382716f50';

function scanFile(os, kind, flagged = 0) {
  const hash = os === 'linux' ? HASH_LINUX : HASH_WIN;
  return {
    os, kind, status: 'completed', engines: 69, malicious: flagged, suspicious: 0,
    scanned: new Date(Date.now() - 3 * 3600e3).toISOString(),
    report: `https://www.virustotal.com/gui/file/${hash}`,
  };
}

function scanComment(files) {
  return `\n\n<!-- yasdig-scan-begin -->\n## VirusTotal\n(mock)\n<!-- yasdig-scan ${
    JSON.stringify({ files })} -->\n<!-- yasdig-scan-end -->`;
}

export function releases(name) {
  const tag = 'v0.10.2';
  const assets = [
    { name: 'yasdig-windows-x86_64.zip', digest: `sha256:${HASH_WIN}` },
    { name: 'yasdig-linux-x86_64.tar.gz', digest: `sha256:${HASH_LINUX}` },
  ].map((a) => ({ ...a, browser_download_url: `/mock-api/download/${a.name}` }));
  const linuxScan = (flagged) => [scanFile('linux', 'archive'), scanFile('linux', 'program', flagged)];
  const rel = {
    tag_name: tag, draft: false, prerelease: false,
    published_at: new Date(Date.now() - 86400e3).toISOString(),
    body: 'Notes for the release.' + scanComment(linuxScan(0)),
    assets,
  };
  switch (name) {
    case 'flagged': rel.body = 'Notes.' + scanComment(linuxScan(1)); break;
    case 'unscanned': rel.body = 'Notes, no scan yet.'; rel.published_at = new Date().toISOString(); break;
    case 'oldwin':
      rel.tag_name = 'v0.9.0';
      rel.body = 'Notes.' + scanComment([...linuxScan(0), scanFile('windows', 'archive'), scanFile('windows', 'program', 1)]);
      break;
    case 'prerelease': rel.prerelease = true; rel.tag_name = 'v0.11.0-rc1'; break;
    case 'nolinux': rel.assets = assets.filter((a) => a.name.includes('windows')); break;
    default: break;
  }
  return [rel];
}

// GitHub's attestations answer: a list, empty when there is none.
export function attestations(name) {
  return { attestations: name === 'noattest' ? [] : [{ bundle: {}, repository_id: 1 }] };
}
