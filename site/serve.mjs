// The site on this computer, before it deploys: `node site/serve.mjs [port]`,
// then http://127.0.0.1:8090/. No packages; Node ≥ 20.
//
// Each page request rebuilds `_site/` first (build.mjs, well under a second),
// so an edit to a page, the layout, the stylesheet or the app's Help topics
// shows on reload — the app's own rule for web/ (`web::serve` reads fresh).
// Files are served as GitHub Pages serves them: `/image/` is
// `_site/image/index.html`, and anything missing is the 404 page.
//
// The GitHub API the download buttons ask is answered here from site/mock/
// (download.js points at /mock-api on localhost): `?mock=flagged` and the rest
// of `SCENARIOS` show every state the dialogs have; they are listed in the
// console on start.

import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from './build.mjs';
import { SCENARIOS, attestations, releases } from './mock/releases.mjs';

const OUT = join(dirname(fileURLToPath(import.meta.url)), '..', '_site');
const PORT = Number(process.argv[2]) || 8090;
const TYPES = {
  '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8', '.json': 'application/json',
  '.xml': 'application/xml', '.txt': 'text/plain; charset=utf-8',
  '.png': 'image/png', '.webp': 'image/webp', '.jpg': 'image/jpeg',
  '.webm': 'video/webm', '.mp4': 'video/mp4', '.svg': 'image/svg+xml',
};

const json = (res, status, body) => {
  res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
  res.end(JSON.stringify(body));
};

async function rebuild() {
  const { missing } = await build({ preview: true });
  return missing.length;
}

async function file(path) {
  try {
    const s = await stat(path);
    if (s.isDirectory()) return file(join(path, 'index.html'));
    return { path, body: await readFile(path) };
  } catch { return null; }
}

createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const mock = url.searchParams.get('mock') || 'clean';
  try {
    // ---- The mock GitHub API
    if (url.pathname.startsWith('/mock-api/')) {
      if (mock === 'down') return json(res, 500, { message: 'mock: the API is down' });
      if (/\/releases$/.test(url.pathname)) return json(res, 200, releases(mock));
      if (/\/attestations\//.test(url.pathname)) return json(res, 200, attestations(mock));
      if (url.pathname.startsWith('/mock-api/download/')) {
        res.writeHead(200, { 'content-type': 'text/plain', 'content-disposition': 'attachment' });
        return res.end('A mock download from site/serve.mjs — the real one is on GitHub.\n');
      }
      return json(res, 404, { message: 'mock: no such route' });
    }

    // ---- The site. A page (no extension, or .html) rebuilds first.
    const ext = extname(url.pathname);
    if (!ext || ext === '.html') await rebuild();
    const rel = normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, '');
    const hit = await file(join(OUT, rel));
    if (hit) {
      res.writeHead(200, { 'content-type': TYPES[extname(hit.path)] || 'application/octet-stream', 'cache-control': 'no-store' });
      return res.end(hit.body);
    }
    const nf = await file(join(OUT, '404.html'));
    res.writeHead(404, { 'content-type': TYPES['.html'] });
    res.end(nf ? nf.body : 'Not found');
  } catch (e) {
    // A build error (a page missing its title, a Help topic that won't
    // evaluate) is shown in the browser rather than as a dead page.
    res.writeHead(500, { 'content-type': 'text/plain; charset=utf-8' });
    res.end(`The site didn't build:\n\n${e.stack || e}`);
  }
}).listen(PORT, '127.0.0.1', async () => {
  const n = await rebuild().catch((e) => { console.error(e); return 0; });
  console.log(`yasdig.com preview: http://127.0.0.1:${PORT}/`);
  console.log(`\nDownload dialog scenarios — add ?mock=<name> to / or /download/:`);
  for (const [k, v] of Object.entries(SCENARIOS)) console.log(`  ${k.padEnd(11)} ${v}`);
  if (n) console.log(`\n${n} media file(s) not in site/media yet: \`node site/build.mjs\` lists them.`);
});
