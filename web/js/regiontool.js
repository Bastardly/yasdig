// ---- A box over the picture, for the viewer's Crop and Area folds -----------
//
// One widget, two uses: Crop draws a box with the outside dimmed and an
// optional aspect lock; Area draws a rectangle or an ellipse with its feather
// shown as a dashed edge inside and out. Both are a box in the image's own
// pixels, dragged by eight handles, moved by its middle, and redrawn by a drag
// anywhere else on the picture.
//
// The overlay lies over the image the way the Refine fold's mask does
// (`lay()` in redraw.js): the image's own box, in the scrolling pane's
// coordinates, so it zooms and pans with the picture and needs no maths of its
// own for either. Positions inside it are percentages of that box, so a fit
// view and 1:1 draw the same thing.
//
// The pane pans on a left drag at 1:1 (`zoomPane`). While a box is up, a left
// drag on the picture is the box's; the handler stops it before the pane
// sees it. Space+drag and the middle button still pan — they are let through.
//
// A factory, gridmark.js's shape: nothing touches the DOM until `init()`.
export function regionTool({ view, img, onChange }) {
  let el = null;
  let box = null;            // {x, y, w, h} in natural pixels
  let opts = { shape: 'rect', aspect: null, dim: false, feather: 0 };
  let space = false;

  const nat = () => ({ w: img.naturalWidth || 1, h: img.naturalHeight || 1 });

  function lay() {
    if (!el || el.hidden) return;
    el.style.left = `${img.offsetLeft}px`;
    el.style.top = `${img.offsetTop}px`;
    el.style.width = `${img.clientWidth}px`;
    el.style.height = `${img.clientHeight}px`;
    draw();
  }

  function draw() {
    if (!box) return;
    const { w, h } = nat();
    const pct = (v, of) => `${(v / of) * 100}%`;
    const b = el.querySelector('.regionbox');
    b.style.left = pct(box.x, w);
    b.style.top = pct(box.y, h);
    b.style.width = pct(box.w, w);
    b.style.height = pct(box.h, h);
    b.classList.toggle('ellipse', opts.shape === 'ellipse');
    el.classList.toggle('dim', !!opts.dim);
    // The feather, as fractions of the frame's short side centred on the edge:
    // the dashed lines are where the blend starts and ends.
    const half = (opts.feather * Math.min(w, h)) / 2;
    for (const [cls, d] of [['.featherin', half], ['.featherout', -half]]) {
      const f = el.querySelector(cls);
      f.hidden = !half;
      f.style.left = pct(box.x + d, w);
      f.style.top = pct(box.y + d, h);
      f.style.width = pct(Math.max(0, box.w - 2 * d), w);
      f.style.height = pct(Math.max(0, box.h - 2 * d), h);
      f.classList.toggle('ellipse', opts.shape === 'ellipse');
    }
  }

  // Keep the box inside the picture, at least 16 px, and on its aspect.
  function settle(b, anchor = 'se') {
    const { w, h } = nat();
    let { x, y, w: bw, h: bh } = b;
    bw = Math.max(16, Math.min(bw, w));
    bh = Math.max(16, Math.min(bh, h));
    if (opts.aspect) {
      // Fit the aspect inside what was dragged, keeping the anchored corner.
      if (bw / bh > opts.aspect) bw = bh * opts.aspect; else bh = bw / opts.aspect;
      if (anchor.includes('w')) x = b.x + b.w - bw;
      if (anchor.includes('n')) y = b.y + b.h - bh;
    }
    x = Math.min(Math.max(0, x), w - bw);
    y = Math.min(Math.max(0, y), h - bh);
    return { x: Math.round(x), y: Math.round(y), w: Math.round(bw), h: Math.round(bh) };
  }

  // The largest box of the aspect, centred, inset by `margin` of each side.
  function centred(margin = 0.1) {
    const { w, h } = nat();
    let bw = w * (1 - 2 * margin), bh = h * (1 - 2 * margin);
    if (opts.aspect) {
      if (bw / bh > opts.aspect) bw = bh * opts.aspect; else bh = bw / opts.aspect;
    }
    return settle({ x: (w - bw) / 2, y: (h - bh) / 2, w: bw, h: bh });
  }

  function toNatural(e) {
    const r = img.getBoundingClientRect();
    const { w, h } = nat();
    return { x: ((e.clientX - r.left) * w) / r.width, y: ((e.clientY - r.top) * h) / r.height };
  }

  function down(e) {
    // Let the pane have its pans.
    if (e.button !== 0 || space) return;
    e.preventDefault();
    e.stopPropagation();
    const handle = e.target.dataset?.h;
    const p0 = toNatural(e);
    const b0 = { ...box };
    const inside = b0 && p0.x >= b0.x && p0.x <= b0.x + b0.w && p0.y >= b0.y && p0.y <= b0.y + b0.h;
    const mode = handle || (inside ? 'move' : 'new');
    const move = (ev) => {
      const p = toNatural(ev);
      const dx = p.x - p0.x, dy = p.y - p0.y;
      let b;
      if (mode === 'move') {
        b = { ...b0, x: b0.x + dx, y: b0.y + dy };
        box = settle(b);
      } else if (mode === 'new') {
        const x = Math.min(p0.x, p.x), y = Math.min(p0.y, p.y);
        // The side being dragged, so an aspect lock keeps the start point put.
        const dragged = (p.x < p0.x ? 'w' : 'e') + (p.y < p0.y ? 'n' : 's');
        box = settle({ x, y, w: Math.abs(dx), h: Math.abs(dy) }, dragged);
      } else {
        b = { ...b0 };
        if (mode.includes('e')) b.w = b0.w + dx;
        if (mode.includes('s')) b.h = b0.h + dy;
        if (mode.includes('w')) { b.x = b0.x + dx; b.w = b0.w - dx; }
        if (mode.includes('n')) { b.y = b0.y + dy; b.h = b0.h - dy; }
        if (b.w < 16) { if (mode.includes('w')) b.x = b0.x + b0.w - 16; b.w = 16; }
        if (b.h < 16) { if (mode.includes('n')) b.y = b0.y + b0.h - 16; b.h = 16; }
        box = settle(b, mode);
      }
      draw();
      onChange?.(box);
    };
    const up = () => {
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', up);
      el.removeEventListener('pointercancel', up);
    };
    el.setPointerCapture(e.pointerId);
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', up);
    el.addEventListener('pointercancel', up);
  }

  return {
    init() {
      el = document.createElement('div');
      el.className = 'region';
      el.hidden = true;
      el.innerHTML = `<div class="featherout" hidden></div><div class="featherin" hidden></div>
        <div class="regionbox">${['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w']
          .map((h) => `<span class="handle" data-h="${h}"></span>`).join('')}</div>`;
      view.appendChild(el);
      el.addEventListener('pointerdown', down);
      img.addEventListener('load', lay);
      new ResizeObserver(lay).observe(img);
      // Space held: the pane's pan, as for the Refine fold's mask.
      document.addEventListener('keydown', (e) => { if (e.code === 'Space') space = true; });
      document.addEventListener('keyup', (e) => { if (e.code === 'Space') space = false; });
    },
    // Put a box up. `b` in natural pixels, or null for the centred default.
    show(b, o = {}) {
      opts = { ...opts, ...o };
      el.hidden = false;
      box = b ? settle(b) : centred();
      lay();
      onChange?.(box);
    },
    hide() { if (el) el.hidden = true; },
    shown: () => !!el && !el.hidden,
    box: () => box,
    // Change the shape, aspect, dimming or feather of the box that is up. A new
    // aspect re-fits the box about its centre.
    set(o) {
      opts = { ...opts, ...o };
      if (box && 'aspect' in o && opts.aspect) {
        const cx = box.x + box.w / 2, cy = box.y + box.h / 2;
        let bw = box.w, bh = box.h;
        // Turned between landscape and portrait: swap the sides first, so the
        // box turns about its centre rather than shrinking to fit.
        if (opts.aspect !== 1 && (bw > bh) !== (opts.aspect > 1)) [bw, bh] = [bh, bw];
        if (bw / bh > opts.aspect) bw = bh * opts.aspect; else bh = bw / opts.aspect;
        box = settle({ x: cx - bw / 2, y: cy - bh / 2, w: bw, h: bh });
        onChange?.(box);
      }
      draw();
    },
    // The picture changed size under the box (a crop, a new image): start over.
    reset() { box = el && !el.hidden ? centred() : null; lay(); if (box) onChange?.(box); },
    lay,
  };
}
