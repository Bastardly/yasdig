// ---- Painting for the viewer's brushes -------------------------------------
//
// One mask per tool, each a canvas at the picture's own size: white where it
// is painted, its alpha the weight the server blends the tool's effect by
// (`crate::brush`). What is on screen is a display-sized overlay of the
// chosen tool's mask in the mask colour, shown while the pointer is over the
// picture or a stroke is going — off the picture you see the result clean,
// as in the Refine dialog.
//
// Dabs rather than Redraw's line segments, because softness here is the
// stroke's own falloff — a radial gradient per dab — not a blur of the view:
// the mask *is* what gets blended, so a soft edge has to be in it.
//
// The brush is sized in screen pixels and converted, so it is the same size
// on screen in Fit and at 1:1. The right button erases; Space and the middle
// button are left to the pane's pan.
//
// A factory, regiontool.js's shape: nothing touches the DOM until `init()`.
export function brushPaint({ view, img, onStroke, locked }) {
  let overlay = null;
  let ring = null;
  let tool = 'dodge';
  let opts = { size: 60, soft: 0.5, erase: false };
  let active = false;
  let space = false;
  let frame = 0;
  // tool → canvas at the picture's size
  const masks = new Map();

  const nat = () => [img.naturalWidth || 1, img.naturalHeight || 1];

  function canvasFor(t) {
    const [w, h] = nat();
    let c = masks.get(t);
    if (!c || c.width !== w || c.height !== h) {
      c = document.createElement('canvas');
      c.width = w;
      c.height = h;
      masks.set(t, c);
    }
    return c;
  }

  function lay() {
    if (!overlay || !active) return;
    overlay.style.left = `${img.offsetLeft}px`;
    overlay.style.top = `${img.offsetTop}px`;
    overlay.style.width = `${img.clientWidth}px`;
    overlay.style.height = `${img.clientHeight}px`;
    const dpr = window.devicePixelRatio || 1;
    const [w, h] = nat();
    overlay.width = Math.max(1, Math.round(Math.min(img.clientWidth * dpr, w)));
    overlay.height = Math.max(1, Math.round(Math.min(img.clientHeight * dpr, h)));
    draw();
  }

  function draw() {
    if (frame || !overlay) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      const ctx = overlay.getContext('2d');
      ctx.clearRect(0, 0, overlay.width, overlay.height);
      const m = masks.get(tool);
      if (!m) return;
      ctx.drawImage(m, 0, 0, overlay.width, overlay.height);
      // Keep the mask's shape, take the colour.
      ctx.globalCompositeOperation = 'source-in';
      ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue('--mask').trim();
      ctx.fillRect(0, 0, overlay.width, overlay.height);
      ctx.globalCompositeOperation = 'source-over';
    });
  }

  // One soft round dab, `r` in the mask's pixels.
  function dab(ctx, x, y, r, erase) {
    const g = ctx.createRadialGradient(x, y, r * (1 - opts.soft), x, y, r);
    g.addColorStop(0, 'rgba(255,255,255,1)');
    g.addColorStop(1, 'rgba(255,255,255,0)');
    ctx.globalCompositeOperation = erase ? 'destination-out' : 'source-over';
    ctx.fillStyle = g;
    ctx.beginPath();
    ctx.arc(x, y, r, 0, Math.PI * 2);
    ctx.fill();
    ctx.globalCompositeOperation = 'source-over';
  }

  function placeRing(e) {
    if (!ring) return;
    const r = view.getBoundingClientRect();
    ring.style.width = ring.style.height = `${opts.size}px`;
    ring.style.left = `${e.clientX - r.left + view.scrollLeft - opts.size / 2}px`;
    ring.style.top = `${e.clientY - r.top + view.scrollTop - opts.size / 2}px`;
  }

  function stroke(e) {
    if (!active || space || (e.button !== 0 && e.button !== 2) || locked?.()) return;
    e.preventDefault();
    e.stopPropagation();
    const erase = opts.erase || e.button === 2;
    const m = canvasFor(tool);
    const ctx = m.getContext('2d');
    const at = (ev) => {
      const r = img.getBoundingClientRect();
      return [(ev.clientX - r.left) * (m.width / r.width), (ev.clientY - r.top) * (m.height / r.height),
        m.width / r.width];
    };
    let [lx, ly, k] = at(e);
    const radius = () => (opts.size / 2) * k;
    dab(ctx, lx, ly, radius(), erase);
    draw();
    // Dabs a quarter of the brush apart along the drag, so a fast stroke is
    // still a stroke and not a string of beads.
    const move = (ev) => {
      const [x, y] = at(ev);
      const step = Math.max(1, radius() / 4);
      const n = Math.ceil(Math.hypot(x - lx, y - ly) / step);
      for (let i = 1; i <= n; i++) dab(ctx, lx + ((x - lx) * i) / n, ly + ((y - ly) * i) / n, radius(), erase);
      [lx, ly] = [x, y];
      placeRing(ev);
      draw();
    };
    const up = () => {
      overlay.classList.remove('painting');
      overlay.removeEventListener('pointermove', move);
      overlay.removeEventListener('pointerup', up);
      overlay.removeEventListener('pointercancel', up);
      onStroke?.(tool);
    };
    overlay.classList.add('painting');
    overlay.setPointerCapture(e.pointerId);
    overlay.addEventListener('pointermove', move);
    overlay.addEventListener('pointerup', up);
    overlay.addEventListener('pointercancel', up);
  }

  return {
    init() {
      overlay = document.createElement('canvas');
      overlay.className = 'brushview';
      overlay.hidden = true;
      ring = document.createElement('div');
      ring.className = 'brushring';
      ring.hidden = true;
      view.append(overlay, ring);
      overlay.addEventListener('pointerdown', stroke);
      overlay.addEventListener('pointermove', placeRing);
      overlay.addEventListener('pointerenter', () => { ring.hidden = !active; });
      overlay.addEventListener('pointerleave', () => { ring.hidden = true; });
      overlay.addEventListener('contextmenu', (e) => e.preventDefault());
      img.addEventListener('load', lay);
      new ResizeObserver(lay).observe(img);
      document.addEventListener('keydown', (e) => { if (e.code === 'Space') space = true; });
      document.addEventListener('keyup', (e) => { if (e.code === 'Space') space = false; });
    },
    show() { active = true; overlay.hidden = false; lay(); },
    hide() { active = false; overlay.hidden = true; ring.hidden = true; },
    active: () => active,
    setTool(t) { tool = t; draw(); },
    set(o) { opts = { ...opts, ...o }; },
    // The tool's mask as a PNG data URL, for `/brush`.
    dataUrl: (t) => canvasFor(t).toDataURL('image/png'),
    clear(t) {
      const m = masks.get(t);
      m?.getContext('2d').clearRect(0, 0, m.width, m.height);
      draw();
    },
    // Forget every mask: a different picture.
    reset() { masks.clear(); draw(); },
    // Whether the page holds this tool's mask at the picture's size.
    has(t) {
      const m = masks.get(t);
      const [w, h] = nat();
      return !!m && m.width === w && m.height === h;
    },
    // Paint the server's copy of a tool's mask back in, after a crop, a pass
    // or a reopen left the page without it.
    async load(t, url) {
      const pic = new Image();
      pic.src = url;
      await pic.decode();
      const c = canvasFor(t);
      const ctx = c.getContext('2d');
      ctx.clearRect(0, 0, c.width, c.height);
      ctx.drawImage(pic, 0, 0, c.width, c.height);
      draw();
    },
    lay,
  };
}
