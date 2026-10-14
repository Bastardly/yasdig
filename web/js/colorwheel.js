import { hueWeight, oklchToSrgb, rampWeight } from './colorrange.js';

// ---- The colour wheel: picking an Area by colour --------------------------------
//
// A wheel over the picture. Its rim is every hue, drawn in OKLCh as the
// selection reads hue (`colorrange.js`), not HSL's. Just inside the rim is the
// selection band: white where a hue is fully selected, black where it isn't,
// grey across the fade. Four dots sit on it: the two filled ones are where the
// full selection starts and ends, the two hollow ones where the fade reaches
// nothing. In the middle, Lightness and Saturation are bars with the same four
// dots.
//
// - Click the rim or a bar with no range to start one there; drag a dot to
//   move that edge (it pushes the dots it meets along); drag the band between the filled dots to
//   move the whole range. Double-click a dot to go back to "any".
// - Under the bars, Feather: one dot, 0..1, how far the selection's edge is
//   spread in the picture (the panel has the same slider).
// - While a dot is held, `onPreview(range)` lets the viewer grey out what is
//   not selected; on letting go, `onChange(range)` is the edit.
// - The wheel gets out of the way when the pointer goes back to the panel,
//   so the sliders can be judged on the picture — the viewer calls `hide()`.
// - Drag its dark middle (anywhere that isn't a control) to move it; it opens
//   where it was left next time, per browser. Double-click the dark middle to
//   put it back over the picture's centre.
//
// Angles: hue 0° at the right, counter-clockwise, as on any colour wheel.
// A factory, regiontool.js's shape: nothing touches the DOM until `init()`.
export function colorWheel({ host, place, onChange, onPreview }) {
  let el = null;
  let canvas = null;
  let range = { hue: null, lightness: null, saturation: null };
  let geo = null;
  let drag = null;
  // Where the user put it: its centre as fractions of the dialog, so it lands
  // in the same place after the window changes size. `null` is "centred over
  // the picture", which is `place()`'s own answer.
  const KEY = 'yasdig.colorwheel.at';
  let pos = (() => {
    try { return JSON.parse(localStorage.getItem(KEY)) ?? null; } catch { return null; }
  })();
  const remember = (p) => {
    pos = p;
    try {
      if (p) localStorage.setItem(KEY, JSON.stringify(p)); else localStorage.removeItem(KEY);
    } catch { /* this session only */ }
  };
  let bounds = { w: 0, h: 0 };
  const inDisc = (x, y) => !!geo && Math.hypot(x - geo.cx, y - geo.cy) <= geo.R;

  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
  // The stylesheet's tokens, so the wheel's chrome is the page's.
  const tok = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

  // Layout in CSS pixels, from the wheel's size.
  function layout(size) {
    const R = size / 2 - 4;
    const ring = Math.max(18, size * 0.06);
    const band = [R - ring - 18, R - ring - 4];
    const barW = (band[0] - 16) * 1.4;
    const cx = size / 2, cy = size / 2;
    return {
      size, R, ring, band, cx, cy,
      bars: {
        lightness: { x: cx - barW / 2, y: cy - 56, w: barW, h: 14 },
        saturation: { x: cx - barW / 2, y: cy + 4, w: barW, h: 14 },
      },
      // The feather: one dot, 0..1, on a plain track under the other two.
      feather: { x: cx - barW / 2, y: cy + 62, w: barW, h: 6 },
    };
  }

  const angleAt = (x, y) => ((Math.atan2(-(y - geo.cy), x - geo.cx) * 180) / Math.PI + 360) % 360;
  const pointAt = (deg, r) => [geo.cx + r * Math.cos((deg * Math.PI) / 180), geo.cy - r * Math.sin((deg * Math.PI) / 180)];

  function draw() {
    if (!canvas || !geo) return;
    const dpr = window.devicePixelRatio || 1;
    const ctx = canvas.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, geo.size, geo.size);
    const { cx, cy, R, ring, band } = geo;

    // The disc behind everything, so the wheel reads over any picture.
    ctx.fillStyle = tok('--ink-800');
    ctx.beginPath();
    ctx.arc(cx, cy, R, 0, Math.PI * 2);
    ctx.fill();

    // The rim: one wedge per degree.
    for (let d = 0; d < 360; d++) {
      const [r, g, b] = oklchToSrgb(70, 45, d);
      ctx.fillStyle = `rgb(${r},${g},${b})`;
      ctx.beginPath();
      ctx.arc(cx, cy, R, (-(d + 1) * Math.PI) / 180, (-d * Math.PI) / 180);
      ctx.arc(cx, cy, R - ring, (-d * Math.PI) / 180, (-(d + 1) * Math.PI) / 180, true);
      ctx.fill();
    }

    // The selection band.
    for (let d = 0; d < 360; d++) {
      const w = range.hue ? hueWeight(range.hue, d + 0.5) : 1;
      const v = Math.round(w * 255);
      ctx.fillStyle = `rgb(${v},${v},${v})`;
      ctx.beginPath();
      ctx.arc(cx, cy, band[1], (-(d + 1) * Math.PI) / 180, (-d * Math.PI) / 180);
      ctx.arc(cx, cy, band[0], (-d * Math.PI) / 180, (-(d + 1) * Math.PI) / 180, true);
      ctx.fill();
    }
    if (range.hue) {
      const mid = (band[0] + band[1]) / 2;
      for (const k of ['a', 'b', 'c', 'd']) dot(ctx, ...pointAt(range.hue[k] % 360, mid), k === 'b' || k === 'c');
    }

    // The two bars.
    const midHue = range.hue ? ((range.hue.b + range.hue.c) / 2) % 360 : 30;
    for (const [key, bar] of Object.entries(geo.bars)) {
      const r = range[key];
      ctx.fillStyle = tok('--text-dim');
      ctx.font = `${tok('--fs-sm')} ${tok('--font-ui')}`;
      ctx.textBaseline = 'bottom';
      ctx.fillText(key === 'lightness' ? 'Lightness' : 'Saturation', bar.x, bar.y - 8);
      for (let i = 0; i < bar.w; i++) {
        const v = (i / bar.w) * 100;
        const [rr, gg, bb] = key === 'lightness' ? oklchToSrgb(v, 0, 0) : oklchToSrgb(65, v, midHue);
        ctx.fillStyle = `rgb(${rr},${gg},${bb})`;
        ctx.fillRect(bar.x + i, bar.y, 1, bar.h);
        const w = r ? rampWeight(r, v) : 1;
        const g = Math.round(w * 255);
        ctx.fillStyle = `rgb(${g},${g},${g})`;
        ctx.fillRect(bar.x + i, bar.y - 6, 1, 4);
      }
      if (r) for (const k of ['a', 'b', 'c', 'd']) dot(ctx, bar.x + (r[k] / 100) * bar.w, bar.y + bar.h / 2, k === 'b' || k === 'c');
    }

    // The feather: the track, filled up to the dot.
    const f = geo.feather, fv = range.feather ?? 0;
    ctx.fillStyle = tok('--text-dim');
    ctx.fillText(`Feather ${fv.toFixed(2)}`, f.x, f.y - 8);
    ctx.fillStyle = tok('--ink-400');
    ctx.fillRect(f.x, f.y, f.w, f.h);
    ctx.fillStyle = tok('--state');
    ctx.fillRect(f.x, f.y, fv * f.w, f.h);
    dot(ctx, f.x + fv * f.w, f.y + f.h / 2, true);
  }

  function dot(ctx, x, y, solid) {
    ctx.beginPath();
    ctx.arc(x, y, 6, 0, Math.PI * 2);
    ctx.fillStyle = solid ? tok('--state') : tok('--ink-900');
    ctx.fill();
    ctx.lineWidth = 2;
    ctx.strokeStyle = solid ? tok('--on-state') : tok('--state');
    ctx.stroke();
  }

  // What is under the pointer: a dot, the band, the rim or a bar.
  function hit(x, y) {
    const { cx, cy, R, band } = geo;
    const rad = Math.hypot(x - cx, y - cy);
    const mid = (band[0] + band[1]) / 2;
    if (range.hue) {
      for (const k of ['a', 'b', 'c', 'd']) {
        const [px, py] = pointAt(range.hue[k] % 360, mid);
        if (Math.hypot(x - px, y - py) <= 9) return { channel: 'hue', key: k };
      }
    }
    for (const [key, bar] of Object.entries(geo.bars)) {
      const r = range[key];
      if (r) {
        for (const k of ['a', 'b', 'c', 'd']) {
          if (Math.hypot(x - (bar.x + (r[k] / 100) * bar.w), y - (bar.y + bar.h / 2)) <= 9) return { channel: key, key: k };
        }
      }
      if (x >= bar.x && x <= bar.x + bar.w && y >= bar.y - 8 && y <= bar.y + bar.h + 4) return { channel: key, key: 'bar' };
    }
    const f = geo.feather;
    if (x >= f.x - 9 && x <= f.x + f.w + 9 && y >= f.y - 10 && y <= f.y + f.h + 10) return { channel: 'feather', key: 'f' };
    if (rad >= band[0] - 4 && rad <= R) return { channel: 'hue', key: rad < band[1] + 2 ? 'band' : 'rim' };
    return null;
  }

  // Move one edge of a range to `v`, pushing the edges it runs into along
  // with it, so a ≤ b ≤ c ≤ d holds without a dot ever stopping at its
  // neighbour. Bars stay in 0..100; a hue range stays within one turn, with
  // `a` back in 0..360. Worked from the range as it was when the drag began,
  // so a dot pushed and then let past again springs back to where it was.
  function pushEdge(r, k, v, hue) {
    const order = ['a', 'b', 'c', 'd'];
    const i = order.indexOf(k);
    const lo = hue ? (i < 3 ? r.d - 360 : -Infinity) : 0;
    const hi = hue ? (i > 0 ? r.a + 360 : Infinity) : 100;
    v = clamp(v, lo, hi);
    const next = { ...r };
    order.forEach((e, j) => {
      if (j < i) next[e] = Math.min(r[e], v);
      else if (j > i) next[e] = Math.max(r[e], v);
      else next[e] = v;
    });
    if (hue) {
      const shift = Math.floor(next.a / 360) * 360;
      for (const e of order) next[e] -= shift;
    }
    return next;
  }

  // A hue near `cur`: the pointer's angle, unwrapped to the turn `cur` is in.
  const near = (cur, deg) => cur + ((((deg - cur) % 360) + 540) % 360) - 180;

  function fresh(channel, v) {
    if (channel === 'hue') {
      const a = (v - 30 + 360) % 360;
      return { a, b: a + 15, c: a + 45, d: a + 60 };
    }
    const c = clamp(v, 20, 80);
    return { a: c - 20, b: c - 10, c: c + 10, d: c + 20 };
  }

  function down(e) {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const r = canvas.getBoundingClientRect();
    const at = (ev) => [ev.clientX - r.left, ev.clientY - r.top];
    const [x, y] = at(e);
    const h = hit(x, y);
    if (!h) {
      if (inDisc(x, y)) moveWheel(e);
      return;
    }
    if (h.channel === 'feather') {
      // A press anywhere on the track puts the dot there; drag from it.
      const f = geo.feather;
      drag = (px) => {
        range = { ...range, feather: Math.round(clamp((px - f.x) / f.w, 0, 1) * 100) / 100 };
        draw();
        onPreview?.(range);
      };
      drag(x);
      follow(e, (ev) => drag?.(...at(ev)), () => {
        drag = null;
        onPreview?.(null);
        onChange?.(range);
      });
      return;
    }
    const valueAt = (px, py) => (h.channel === 'hue' ? angleAt(px, py)
      : clamp(((px - geo.bars[h.channel].x) / geo.bars[h.channel].w) * 100, 0, 100));
    if (!range[h.channel] || h.key === 'rim' || h.key === 'bar') {
      if (!range[h.channel] || h.key === 'rim') range = { ...range, [h.channel]: fresh(h.channel, valueAt(x, y)) };
    }
    const start = range[h.channel];
    const hue = h.channel === 'hue';
    const v0 = valueAt(x, y);
    // The pointer's hue, unwrapped as it goes round, so a drag can carry a
    // range past 360° (or a dot all the way round) without a jump.
    let cur = hue && h.key.length === 1 ? start[h.key] : v0;
    drag = (px, py) => {
      let r;
      if (hue) cur = near(cur, angleAt(px, py));
      if (h.key.length === 1) {
        r = pushEdge(start, h.key, hue ? cur : valueAt(px, py), hue);
      } else {
        // The whole range, by how far the pointer has moved.
        let dv = hue ? cur - v0 : valueAt(px, py) - v0;
        if (!hue) dv = clamp(dv, -start.a, 100 - start.d);
        r = { a: start.a + dv, b: start.b + dv, c: start.c + dv, d: start.d + dv };
        if (hue) {
          const shift = Math.floor(r.a / 360) * 360;
          for (const k of ['a', 'b', 'c', 'd']) r[k] -= shift;
        }
      }
      range = { ...range, [h.channel]: r };
      draw();
      onPreview?.(range);
    };
    drag(x, y);
    follow(e, (ev) => drag?.(...at(ev)), () => {
      drag = null;
      onPreview?.(null);
      onChange?.(range);
    });
  }

  // Track a press to its end. Every way a press can end ends it once: the
  // button let go (on the canvas, which holds the capture, or anywhere on
  // the page should the capture have been lost), a cancelled pointer, or
  // the capture taken away — say by the wheel hiding under the pointer.
  function follow(e, move, end) {
    let done = false;
    const off = () => {
      if (done) return;
      done = true;
      canvas.removeEventListener('pointermove', move);
      canvas.removeEventListener('lostpointercapture', off);
      for (const t of ['pointerup', 'pointercancel']) window.removeEventListener(t, off, true);
      end();
    };
    canvas.addEventListener('pointermove', move);
    canvas.addEventListener('lostpointercapture', off);
    for (const t of ['pointerup', 'pointercancel']) window.addEventListener(t, off, true);
    try { canvas.setPointerCapture(e.pointerId); } catch { /* the window listeners still end it */ }
  }

  // Drag the whole wheel by its dark middle, kept inside the dialog.
  function moveWheel(e) {
    const x0 = e.clientX, y0 = e.clientY;
    const l0 = el.offsetLeft, t0 = el.offsetTop;
    const size = geo.size;
    canvas.style.cursor = 'grabbing';
    follow(e, (ev) => {
      el.style.left = `${clamp(l0 + ev.clientX - x0, 0, bounds.w - size)}px`;
      el.style.top = `${clamp(t0 + ev.clientY - y0, 0, bounds.h - size)}px`;
    }, () => {
      canvas.style.cursor = 'move';
      remember({ cx: (el.offsetLeft + size / 2) / bounds.w, cy: (el.offsetTop + size / 2) / bounds.h });
    });
  }

  // Where it opens: where it was left, else where `place()` puts it.
  function position() {
    const p = place();
    bounds = { w: p.width, h: p.height };
    let { left, top } = p;
    if (pos) {
      left = clamp(pos.cx * p.width - p.size / 2, 0, p.width - p.size);
      top = clamp(pos.cy * p.height - p.size / 2, 0, p.height - p.size);
    }
    el.style.left = `${left}px`;
    el.style.top = `${top}px`;
    return p.size;
  }

  return {
    init() {
      el = document.createElement('div');
      el.className = 'colorwheel';
      el.hidden = true;
      canvas = document.createElement('canvas');
      el.append(canvas);
      host.append(el);
      canvas.addEventListener('pointerdown', down);
      // The pointer says what a press would do: pick, or move the wheel.
      canvas.addEventListener('pointermove', (e) => {
        if (e.buttons) return;
        const r = canvas.getBoundingClientRect();
        const x = e.clientX - r.left, y = e.clientY - r.top;
        canvas.style.cursor = hit(x, y) ? 'pointer' : inDisc(x, y) ? 'move' : 'default';
      });
      // A dot double-clicked: that channel back to "any".
      canvas.addEventListener('dblclick', (e) => {
        const r = canvas.getBoundingClientRect();
        const x = e.clientX - r.left, y = e.clientY - r.top;
        const h = hit(x, y);
        // The dark middle double-clicked: back over the picture's centre.
        if (!h && inDisc(x, y)) {
          remember(null);
          position();
          return;
        }
        if (!h || h.key.length !== 1) return;
        range = { ...range, [h.channel]: h.channel === 'feather' ? 0 : null };
        draw();
        onChange?.(range);
      });
    },
    // Open on `r` (or every colour), sized and placed by the viewer.
    show(r) {
      range = { hue: null, lightness: null, saturation: null, ...(r ?? {}) };
      const size = position();
      geo = layout(size);
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.round(size * dpr);
      canvas.height = Math.round(size * dpr);
      canvas.style.width = canvas.style.height = `${size}px`;
      el.hidden = false;
      draw();
    },
    hide() {
      if (el && !el.hidden) el.hidden = true;
    },
    shown: () => !!el && !el.hidden,
    contains: (node) => !!el && el.contains(node),
  };
}
