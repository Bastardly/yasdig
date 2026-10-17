// ---- Text on the picture: the Refine & adjust dialog's Text panel -----------
//
// The page draws the text, on a canvas: the browser has every font on the
// machine and shapes any script, which a font crate and bundled font files on
// the server would only approximate. One `paint` draws it twice — at screen
// size as the preview laid over the picture (Redraw's mask rule: same box,
// scrolls with it), and at the picture's own size as a transparent PNG
// (`png()`) that `/text` lays over the picture as a History step.
//
// Sizes are in the picture's pixels, so the preview is the full-size layer
// scaled. The text's place is its block's centre as fractions of the frame,
// so it stays put across Fit and 1:1. While the panel is open a drag on the
// picture moves it; Space and the middle button are left to the pane's pan.
//
// A factory, brushpaint.js's shape: nothing touches the DOM until `init()`.

// Fonts worth offering when this machine has them, beside the generic four
// that always exist. Common on Windows (the first block) or on Linux.
const CANDIDATES = [
  'Arial', 'Helvetica', 'Verdana', 'Tahoma', 'Georgia', 'Times New Roman', 'Courier New',
  'Impact', 'Trebuchet MS', 'Comic Sans MS', 'Segoe UI', 'Palatino Linotype',
  'DejaVu Sans', 'DejaVu Serif', 'Liberation Sans', 'Liberation Serif', 'Noto Sans',
  'Noto Serif', 'Ubuntu', 'Cantarell', 'Fira Sans', 'Inter', 'Roboto', 'Open Sans',
];
export const GENERIC = [['sans-serif', 'Sans-serif'], ['serif', 'Serif'], ['monospace', 'Monospace'],
  ['system-ui', 'System']];

// Whether a font is installed: text set in it measures differently from the
// same text in a fallback. `document.fonts.check()` can't say — it answers
// yes for any name it has nothing to load for, installed or not.
export function installedFonts() {
  const ctx = document.createElement('canvas').getContext('2d');
  const sample = 'mmmmmmmmmlli 1234 WQ@';
  const width = (font) => { ctx.font = font; return ctx.measureText(sample).width; };
  const base = ['monospace', 'serif', 'sans-serif'].map((f) => width(`72px ${f}`));
  return CANDIDATES.filter((name) =>
    ['monospace', 'serif', 'sans-serif'].some((f, i) => width(`72px "${name}", ${f}`) !== base[i]));
}

export function textTool({ view, img, onMove }) {
  let overlay = null;
  let active = false;
  let space = false;
  let frame = 0;
  // The block's centre, as fractions of the frame.
  let at = { x: 0.5, y: 0.5 };
  let o = {
    text: '', family: 'sans-serif', bold: false, italic: false, size: 64, line: 1.2,
    align: 'center', color: '#ffffff', opacity: 1, outline: 0, outlineColor: '#000000', shadow: false,
  };

  const nat = () => [img.naturalWidth || 1, img.naturalHeight || 1];
  const lines = () => o.text.replace(/\s+$/, '').split('\n');
  const has = () => o.text.trim() !== '';

  // The text onto `ctx`, a canvas over a `w × h` picture, `k` canvas pixels
  // per picture pixel.
  function paint(ctx, w, h, k) {
    ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
    if (!has()) return;
    const size = o.size * k;
    const quote = /^[a-z-]+$/.test(o.family) ? o.family : `"${o.family}"`;
    ctx.font = `${o.italic ? 'italic ' : ''}${o.bold ? '700' : '400'} ${size}px ${quote}`;
    ctx.textBaseline = 'middle';
    ctx.textAlign = o.align;
    ctx.lineJoin = 'round';
    ctx.globalAlpha = o.opacity;
    const ls = lines();
    const step = size * o.line;
    const widest = Math.max(...ls.map((l) => ctx.measureText(l).width));
    const cx = at.x * w * k, cy = at.y * h * k;
    const x = ctx.textAlign === 'left' ? cx - widest / 2 : ctx.textAlign === 'right' ? cx + widest / 2 : cx;
    const y0 = cy - ((ls.length - 1) * step) / 2;
    const shadow = () => {
      ctx.shadowColor = 'rgba(0, 0, 0, 0.6)';
      ctx.shadowBlur = size * 0.15;
      ctx.shadowOffsetX = ctx.shadowOffsetY = size * 0.05;
    };
    const plain = () => { ctx.shadowColor = 'transparent'; };
    ls.forEach((l, i) => {
      const y = y0 + i * step;
      // The shadow goes with the first thing drawn — the outline when there
      // is one — so it isn't cast twice.
      if (o.shadow) shadow(); else plain();
      if (o.outline > 0) {
        ctx.strokeStyle = o.outlineColor;
        ctx.lineWidth = o.outline * 2 * k; // half of it is under the fill
        ctx.strokeText(l, x, y);
        plain();
      }
      ctx.fillStyle = o.color;
      ctx.fillText(l, x, y);
    });
    ctx.globalAlpha = 1;
    plain();
  }

  function lay() {
    if (!overlay || !active) return;
    overlay.style.left = `${img.offsetLeft}px`;
    overlay.style.top = `${img.offsetTop}px`;
    overlay.style.width = `${img.clientWidth}px`;
    overlay.style.height = `${img.clientHeight}px`;
    const dpr = window.devicePixelRatio || 1;
    overlay.width = Math.max(1, Math.round(img.clientWidth * dpr));
    overlay.height = Math.max(1, Math.round(img.clientHeight * dpr));
    draw();
  }

  function draw() {
    if (frame || !overlay) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      const [w, h] = nat();
      paint(overlay.getContext('2d'), w, h, overlay.width / w);
    });
  }

  // A drag anywhere on the picture carries the text by as much.
  function down(e) {
    if (!active || space || e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const r = overlay.getBoundingClientRect();
    const x0 = e.clientX, y0 = e.clientY, from = { ...at };
    const move = (ev) => {
      at = {
        x: Math.min(1, Math.max(0, from.x + (ev.clientX - x0) / r.width)),
        y: Math.min(1, Math.max(0, from.y + (ev.clientY - y0) / r.height)),
      };
      draw();
    };
    const up = () => {
      overlay.classList.remove('moving');
      overlay.removeEventListener('pointermove', move);
      overlay.removeEventListener('pointerup', up);
      overlay.removeEventListener('pointercancel', up);
      onMove?.();
    };
    overlay.classList.add('moving');
    overlay.setPointerCapture(e.pointerId);
    overlay.addEventListener('pointermove', move);
    overlay.addEventListener('pointerup', up);
    overlay.addEventListener('pointercancel', up);
  }

  return {
    init() {
      overlay = document.createElement('canvas');
      overlay.className = 'textview';
      overlay.hidden = true;
      view.append(overlay);
      overlay.addEventListener('pointerdown', down);
      img.addEventListener('load', lay);
      new ResizeObserver(lay).observe(img);
      document.addEventListener('keydown', (e) => { if (e.code === 'Space') space = true; });
      document.addEventListener('keyup', (e) => { if (e.code === 'Space') space = false; });
    },
    show() { active = true; overlay.hidden = false; lay(); },
    hide() { active = false; overlay.hidden = true; },
    set(next) { o = { ...o, ...next }; draw(); },
    has,
    // The layer at the picture's own size, for `/text`.
    png() {
      const [w, h] = nat();
      const c = document.createElement('canvas');
      c.width = w;
      c.height = h;
      paint(c.getContext('2d'), w, h, 1);
      return c.toDataURL('image/png');
    },
  };
}
