// yasdig.com, built: `node site/build.mjs` writes the whole site into `_site/`
// (gitignored), which `pages.yml` uploads and `serve.mjs` serves. Node ≥ 20, no
// packages — the repo has no package.json and this doesn't start one.
//
// Every route is a whole HTML file, not a single page filled in by script, so a
// search engine, a link preview and a visitor with scripts off all get the text.
//
// Where each page comes from:
//   site/layout.html        the shell: head, rail, burger bar, footer
//   site/pages/*.html       one body per route; a leading comment holds its
//                           title and description (`meta`)
//   site/partials/*.html    pasted in by `<!-- include name -->`
//   web/js/help.js TOPICS   the guide: one page per Help topic, the app's own
//                           text, so a sentence is written once (help.js's rule)
//   web/tokens.css          the app's tokens, copied as they are — the site
//                           can't drift from the app's palette
//
// `<media src="x.webm" alt="…" caption="…">` in a page becomes a picture or a
// looping clip from `site/media/`; a file not there yet becomes a labelled
// empty frame, and the build lists it, so the pages can be written before the
// screenshots exist.

import { readFileSync, writeFileSync, mkdirSync, rmSync, cpSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const SITE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(SITE, '..');
const OUT = join(ROOT, '_site');
const ORIGIN = 'https://yasdig.com';
const REPO = 'https://github.com/Bastardly/yasdig';

// The rail, top to bottom. `path` is the route; the page body is
// `site/pages/{page}.html`. The Guide's rows are added from TOPICS below.
const FEATURES = [
  { page: 'image', path: '/image/', label: 'Image' },
  { page: 'refine', path: '/refine/', label: 'Refine & adjust' },
  { page: 'video', path: '/video/', label: 'Video' },
  { page: 'assistant', path: '/assistant/', label: 'Assistant' },
  { page: 'cast', path: '/cast/', label: 'Cast' },
  { page: 'models', path: '/models/', label: 'Models' },
];
const TOP = [
  { page: 'index', path: '/', label: 'Overview' },
  { page: 'download', path: '/download/', label: 'Download' },
];

// A Help topic whose `fill(el)` asks the running server for part of its page
// can't do that here: its placeholder becomes a pointer instead. A new topic
// with a `fill` and no row here stops the build, rather than shipping
// "Loading…" forever.
const FILLED = {
  models: [/<div id="helpfamilies">[\s\S]*?<\/div>/,
    `<p>In the app, this is a table of every family with the parts it needs, the
     recommended set and which files you already have. The
     <a href="/models/">Models page</a> lists the families and what each takes on the card.</p>`],
  about: [/<pre class="licences" id="licencetext">[\s\S]*?<\/pre>/,
    `<p>They are in <a href="${REPO}/blob/master/THIRD_PARTY_LICENSES.md">THIRD_PARTY_LICENSES.md</a>
     beside the source, and in the app under Help → <i>About &amp; licences</i>.</p>`],
};

const read = (p) => readFileSync(p, 'utf8');
const esc = (s) => String(s).replace(/[&<>"]/g,
  (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

// The Help tab's topics, read the way `modules.test.js` evaluates the page's
// module graph: with just enough DOM stubbed for `<prompt-box>` to be declared.
// No module reads `document` at load (web.md's rule), so nothing else is needed.
async function helpTopics() {
  globalThis.HTMLElement ??= class {};
  globalThis.customElements ??= { define() {} };
  // The query makes a fresh copy each build, so `serve.mjs` shows a Help edit
  // on reload (Node caches a module by its URL; help.js's imports stay cached,
  // and TOPICS doesn't depend on them).
  const help = await import(`${pathToFileURL(join(ROOT, 'web/js/help.js')).href}?t=${Date.now()}`);
  return help.menuOrder().map((t) => {
    let html = t.html;
    if (t.fill) {
      const f = FILLED[t.id];
      if (!f) throw new Error(`Help topic "${t.id}" has a fill(el): add a FILLED row in site/build.mjs`);
      if (!f[0].test(html)) throw new Error(`Help topic "${t.id}": FILLED's placeholder no longer matches`);
      html = html.replace(f[0], f[1]);
    }
    // The topic's heading is the page's one h1.
    html = html.replace(/<h2>([\s\S]*?)<\/h2>/, '<h1>$1</h1>');
    const first = /<p>([\s\S]*?)<\/p>/.exec(html);
    return { id: t.id, title: t.title, html, description: first ? summary(first[1]) : t.title };
  });
}

// A meta description from a paragraph: the text alone, one line, cut at a
// word near 155 characters (about what a results page shows).
function summary(html) {
  const text = html.replace(/<[^>]+>/g, '').replace(/&nbsp;/g, ' ').replace(/\s+/g, ' ').trim()
    .replace(/&amp;/g, '&');
  if (text.length <= 155) return text;
  return text.slice(0, 155).replace(/\s+\S*$/, '') + '…';
}

// `<!-- key: value … -->` at the head of a page body.
function meta(src) {
  const m = /^\s*<!--([\s\S]*?)-->/.exec(src);
  const out = {};
  if (m) {
    for (const line of m[1].split('\n')) {
      const kv = /^\s*(\w+):\s*(.*\S)\s*$/.exec(line);
      if (kv) out[kv[1]] = kv[2];
    }
  }
  return { meta: out, body: m ? src.slice(m[0].length) : src };
}

// A picture's size from its header, so the page reserves the room before it
// loads (no jump as screenshots arrive). PNG and WebP; anything else gets the
// 16:9 frame from the stylesheet.
function imageSize(path) {
  const b = readFileSync(path);
  if (b.length > 24 && b.toString('latin1', 1, 4) === 'PNG') {
    return [b.readUInt32BE(16), b.readUInt32BE(20)];
  }
  if (b.length > 30 && b.toString('latin1', 0, 4) === 'RIFF' && b.toString('latin1', 8, 12) === 'WEBP') {
    const kind = b.toString('latin1', 12, 16);
    if (kind === 'VP8X') return [1 + b.readUIntLE(24, 3), 1 + b.readUIntLE(27, 3)];
    if (kind === 'VP8L') {
      const v = b.readUInt32LE(21);
      return [1 + (v & 0x3fff), 1 + ((v >> 14) & 0x3fff)];
    }
    if (kind === 'VP8 ') return [b.readUInt16LE(26) & 0x3fff, b.readUInt16LE(28) & 0x3fff];
  }
  return null;
}

const missing = [];
const attrs = (s) => Object.fromEntries([...s.matchAll(/(\w+)="([^"]*)"/g)].map((m) => [m[1], m[2]]));

// The empty frames are for the preview only: the deploy (`build()` with no
// `preview`) leaves a missing file out altogether, so yasdig.com never shows a
// dashed box where a screenshot should be. Both list what is missing.
let preview = false;

function media(src, where) {
  return src.replace(/<media\s([^>]*)>/g, (_, a) => {
    const { src: file, alt = '', caption = '' } = attrs(a);
    const path = join(SITE, 'media', file);
    const cap = caption ? `<figcaption>${caption}</figcaption>` : '';
    if (!existsSync(path)) {
      missing.push(`${file}  (${where}: ${alt})`);
      if (!preview) return '';
      return `<figure class="media"><div class="ph" role="img" aria-label="${esc(alt)}"><span>${esc(alt)}</span><code>${esc(file)}</code></div>${cap}</figure>`;
    }
    if (/\.(webm|mp4)$/.test(file)) {
      // Its poster: the same name as a picture, when there is one. Starts and
      // stops with scrolling (site.js); preload none, so a page of clips costs
      // nothing until one is in view.
      const stem = file.replace(/\.\w+$/, '');
      const poster = ['webp', 'png', 'jpg'].map((x) => `${stem}.${x}`)
        .find((p) => existsSync(join(SITE, 'media', p)));
      return `<figure class="media"><video src="/media/${file}"${poster ? ` poster="/media/${poster}"` : ''}
        muted loop playsinline preload="none" aria-label="${esc(alt)}"></video>${cap}</figure>`;
    }
    const size = imageSize(path);
    return `<figure class="media"><img src="/media/${file}" alt="${esc(alt)}" loading="lazy"${
      size ? ` width="${size[0]}" height="${size[1]}"` : ''}>${cap}</figure>`;
  });
}

function includes(src) {
  return src.replace(/<!--\s*include (\w+)\s*-->/g, (_, name) => read(join(SITE, 'partials', `${name}.html`)));
}

function rail(current, topics) {
  const item = (r) => `<a class="btn btn-choice railbtn${r.path === current ? ' active" aria-current="page' : ''}" href="${r.path}">${esc(r.label)}</a>`;
  return [
    ...TOP.map(item),
    '<b class="railtitle">Features</b>',
    ...FEATURES.map(item),
    `<a class="railtitle${current === '/guide/' ? ' here" aria-current="page' : ''}" href="/guide/">Guide</a>`,
    ...topics.map((t) => item({ path: `/guide/${t.id}/`, label: t.title })),
  ].join('\n      ');
}

function page({ path, title, description, body, current, topics, extraHead = '' }) {
  const canonical = ORIGIN + path;
  const og = existsSync(join(SITE, 'media', 'og.png'))
    ? `<meta property="og:image" content="${ORIGIN}/media/og.png">\n<meta name="twitter:card" content="summary_large_image">` : '';
  return read(join(SITE, 'layout.html'))
    .replaceAll('{{title}}', esc(title))
    .replaceAll('{{description}}', esc(description))
    .replaceAll('{{canonical}}', canonical)
    .replace('{{og}}', og)
    .replace('{{head}}', extraHead)
    .replace('{{rail}}', rail(current, topics))
    // The rail's own Download button, except where the download already is.
    .replace('{{railfoot}}', current === '/' || current === '/download/' ? ''
      : '<a class="btn btn-run" href="/download/">Download</a>')
    .replace('{{body}}', () => body);
}

function write(path, html) {
  const file = path.endsWith('/') ? join(OUT, path, 'index.html') : join(OUT, path);
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, html);
}

// The program, for search engines' rich results.
const JSONLD = `<script type="application/ld+json">${JSON.stringify({
  '@context': 'https://schema.org',
  '@type': 'SoftwareApplication',
  name: 'YASDIG',
  alternateName: 'Yet Another Stable Diffusion Image Generator',
  applicationCategory: 'MultimediaApplication',
  operatingSystem: 'Windows 10, Windows 11, Linux',
  offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
  license: 'https://www.apache.org/licenses/LICENSE-2.0',
  url: ORIGIN,
  downloadUrl: `${REPO}/releases`,
  codeRepository: REPO,
})}</script>`;

export async function build(opts = {}) {
  preview = !!opts.preview;
  missing.length = 0;
  rmSync(OUT, { recursive: true, force: true });
  mkdirSync(OUT, { recursive: true });
  const topics = await helpTopics();
  const routes = [];

  for (const r of [...TOP, ...FEATURES, { page: '404', path: '/404.html' }]) {
    const { meta: m, body } = meta(read(join(SITE, 'pages', `${r.page}.html`)));
    if (!m.title || !m.description) throw new Error(`site/pages/${r.page}.html: its comment needs title: and description:`);
    write(r.path, page({
      path: r.path, current: r.path, topics, title: m.title, description: m.description,
      body: media(includes(body), r.page),
      extraHead: (r.page === 'index' ? JSONLD : '') + (r.page === '404' ? '<meta name="robots" content="noindex">' : ''),
    }));
    if (r.page !== '404') routes.push(r.path);
  }

  for (const t of topics) {
    const path = `/guide/${t.id}/`;
    write(path, page({
      path, current: path, topics,
      title: `${t.title} — YASDIG guide`,
      description: t.description,
      body: `<p class="crumb"><a href="/guide/">Guide</a> · from the app's Help tab</p>
<article class="card helpdoc">${t.html}</article>`,
    }));
    routes.push(path);
  }

  // The guide's own page: every topic with its first sentence or two.
  write('/guide/', page({
    path: '/guide/', current: '/guide/', topics,
    title: 'Guide — YASDIG',
    description: 'How YASDIG works, from the app\'s own Help tab: models and their parts, memory, samplers, schedulers, step caching, references, cast, and Refine & adjust.',
    body: `<h1>Guide</h1>
<p class="lede">The app's Help tab, page for page: what to pick, and why.</p>
${topics.map((t) => `<a class="strip card" href="/guide/${t.id}/"><div><h3>${esc(t.title)}</h3>
  <p>${esc(t.description)}</p><span class="more">Read →</span></div></a>`).join('\n')}`,
  }));
  routes.push('/guide/');

  // A token the site's stylesheet names must exist in one of the two files: a
  // missing one is no error to the browser, just a rule that silently does
  // nothing (an undefined --sp-8 once took the page's padding away). This is
  // what catches a token renamed in the app.
  const css = read(join(SITE, 'style.css'));
  const defined = new Set([...(read(join(ROOT, 'web/tokens.css')) + css).matchAll(/(--[\w-]+)\s*:/g)].map((m) => m[1]));
  const undef = [...new Set([...css.matchAll(/var\((--[\w-]+)/g)].map((m) => m[1]))].filter((t) => !defined.has(t));
  if (undef.length) throw new Error(`site/style.css names tokens nothing defines: ${undef.join(', ')}`);

  cpSync(join(ROOT, 'web/tokens.css'), join(OUT, 'tokens.css'));
  for (const f of ['style.css', 'CNAME']) cpSync(join(SITE, f), join(OUT, f));
  cpSync(join(SITE, 'js'), join(OUT, 'js'), { recursive: true });
  if (existsSync(join(SITE, 'media'))) {
    cpSync(join(SITE, 'media'), join(OUT, 'media'), {
      recursive: true, filter: (p) => !p.endsWith('.md'),
    });
  }
  writeFileSync(join(OUT, 'sitemap.xml'), `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${routes.map((p) => `  <url><loc>${ORIGIN}${p}</loc></url>`).join('\n')}
</urlset>
`);
  writeFileSync(join(OUT, 'robots.txt'), `User-agent: *\nAllow: /\nSitemap: ${ORIGIN}/sitemap.xml\n`);
  return { routes, missing: [...missing] };
}

// Run as a script (not imported by serve.mjs): build once and report.
// `--preview` keeps the empty frames, as serve.mjs does.
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const pv = process.argv.includes('--preview');
  const { routes, missing: miss } = await build({ preview: pv });
  console.log(`_site/: ${routes.length} pages`);
  for (const r of routes) console.log(`  ${r}`);
  if (miss.length) {
    console.log(`\nMedia not in site/media yet (${miss.length}) — ${
      pv ? 'each shows as an empty frame' : 'left off the deployed pages until it is'}:`);
    for (const m of miss) console.log(`  ${m}`);
  }
}
