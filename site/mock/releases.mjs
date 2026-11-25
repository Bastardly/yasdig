// What `serve.mjs` answers in place of api.github.com, so the download dialogs
// can be seen in every state they have without a release to make them. Picked
// by the page's `?mock=` (download.js passes it on); never copied into _site/.
//
// The release's shape is the slice of GitHub's that download.js reads: tag,
// dates, `prerelease`, and assets with `name`, `browser_download_url` and
// `digest`.

export const SCENARIOS = {
  clean: 'the newest release, attested',
  prerelease: 'the newest is a pre-release',
  noattest: 'no build attestation (a release from before them)',
  nolinux: 'only the Windows archive is up yet',
  down: 'the GitHub API answers 500: the buttons keep the Releases link',
};

const HASH_WIN = 'a3f1c0de9b7e4d21c6f5a8b90e1d2c3b4a5968778695a4b3c2d1e0f9a8b7c6d5';
const HASH_LINUX = '5e4d3c2b1a0f9e8d7c6b5a49382716f5e4d3c2b1a0f9e8d7c6b5a49382716f50';

export function releases(name) {
  const assets = [
    { name: 'yasdig-windows-x86_64.zip', digest: `sha256:${HASH_WIN}` },
    { name: 'yasdig-linux-x86_64.tar.gz', digest: `sha256:${HASH_LINUX}` },
  ].map((a) => ({ ...a, browser_download_url: `/mock-api/download/${a.name}` }));
  const rel = {
    tag_name: 'v0.10.2', draft: false, prerelease: false,
    published_at: new Date(Date.now() - 86400e3).toISOString(),
    body: 'Notes for the release.',
    assets,
  };
  switch (name) {
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
