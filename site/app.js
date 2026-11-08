// ua101-rs site: copy keys, the working patch bay, faders and switches.
// Without script the page is complete: the patch bay shows its two cables.

const SVG = 'http://www.w3.org/2000/svg';

// ---------- copy keys ----------
const status = document.querySelector('[data-copy-status]');
document.querySelectorAll('[data-copy]').forEach((btn) => {
  btn.addEventListener('click', async () => {
    const text = document.getElementById(btn.dataset.copy).textContent.trim();
    let ok = false;
    try {
      await navigator.clipboard.writeText(text);
      ok = true;
    } catch {
      const range = document.createRange();
      range.selectNodeContents(document.getElementById(btn.dataset.copy));
      const sel = getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
    }
    btn.textContent = ok ? 'Copied' : 'Selected';
    if (status && btn.closest('.hero')) {
      status.textContent = ok ? 'Copied. Paste it into a terminal.' : 'Selected. Press Ctrl+C to copy.';
    }
    setTimeout(() => { btn.textContent = 'Copy'; }, 2000);
  });
});

// ---------- patch bay ----------
const patch = document.querySelector('[data-patch]');
if (patch) {
  const INPUTS = 12, OUTPUTS = 10, IN_X = 150, OUT_X = 490, ROW = 30, TOP = 22;
  const inY = (i) => TOP + i * ROW;
  const outY = (o) => TOP + (INPUTS - OUTPUTS) * ROW / 2 + o * ROW;
  const routes = new Array(OUTPUTS).fill(null);
  routes[2] = 0; // IN 1 to OUT 3, as drawn without script
  routes[3] = 1; // IN 2 to OUT 4
  let enabled = true, dragging = null, live = null, picked = null;

  const el = (name, attrs, parent) => {
    const n = document.createElementNS(SVG, name);
    for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
    if (parent) parent.appendChild(n);
    return n;
  };
  const curve = (x1, y1, x2, y2) => {
    const dx = Math.abs(x2 - x1) * 0.45, sag = 18 + Math.abs(y2 - y1) * 0.15;
    return `M${x1} ${y1} C ${x1 + dx} ${y1 + sag}, ${x2 - dx} ${y2 + sag}, ${x2} ${y2}`;
  };

  const cables = patch.querySelector('[data-cables]');
  const inG = patch.querySelector('[data-inputs]');
  const outG = patch.querySelector('[data-outputs]');
  inG.replaceChildren(); // the static drawing is rebuilt as live jacks
  outG.replaceChildren();
  const jack = (parent, x, y, label, side, idx) => {
    const g = el('g', { class: 'jack', tabindex: '0', role: 'button', 'data-side': side, 'data-idx': idx }, parent);
    el('circle', { cx: x, cy: y, r: 8 }, g);
    el('circle', { class: 'core', cx: x, cy: y, r: 4 }, g);
    const t = el('text', side === 'in'
      ? { x: x - 18, y: y + 4, 'text-anchor': 'end' }
      : { x: x + 18, y: y + 4, 'text-anchor': 'start' }, parent);
    t.textContent = label;
    return g;
  };
  const inJacks = [...Array(INPUTS)].map((_, i) => jack(inG, IN_X, inY(i), `IN ${i + 1}`, 'in', i));
  const outJacks = [...Array(OUTPUTS)].map((_, o) => jack(outG, OUT_X, outY(o), `OUT ${o + 1}`, 'out', o));

  const render = () => {
    cables.replaceChildren();
    routes.forEach((i, o) => {
      if (i !== null) el('path', { class: 'cable', d: curve(IN_X, inY(i), OUT_X, outY(o)) }, cables);
    });
    if (live) cables.appendChild(live);
    inJacks.forEach((j, i) => {
      j.classList.toggle('is-used', routes.includes(i) || dragging === i || picked === i);
      j.setAttribute('aria-label', `Input ${i + 1}${picked === i ? ', picked: choose an output' : ''}`);
    });
    outJacks.forEach((j, o) => {
      j.classList.toggle('is-used', routes[o] !== null);
      j.setAttribute('aria-label', `Output ${o + 1}: ${routes[o] === null ? 'computer audio' : `input ${routes[o] + 1}`}`);
    });
    patch.classList.toggle('is-off', !enabled);
  };

  const toSvg = (e) => {
    const p = patch.createSVGPoint();
    p.x = e.clientX; p.y = e.clientY;
    return p.matrixTransform(patch.getScreenCTM().inverse());
  };
  const outAt = (pt) => {
    const o = Math.round((pt.y - outY(0)) / ROW);
    return o >= 0 && o < OUTPUTS && Math.abs(pt.x - OUT_X) < 40 ? o : null;
  };
  let target = null;
  const setTarget = (o) => {
    if (target !== null) outJacks[target].classList.remove('is-target');
    target = o;
    if (o !== null) outJacks[o].classList.add('is-target');
  };

  patch.addEventListener('pointerdown', (e) => {
    const j = e.target.closest('.jack');
    if (!j) return;
    const idx = Number(j.dataset.idx);
    if (j.dataset.side === 'in') dragging = idx;
    else if (routes[idx] !== null) { dragging = routes[idx]; routes[idx] = null; }
    else return;
    patch.setPointerCapture(e.pointerId);
    live = el('path', { class: 'cable cable--live' });
    const pt = toSvg(e);
    live.setAttribute('d', curve(IN_X, inY(dragging), pt.x, pt.y));
    render();
  });
  patch.addEventListener('pointermove', (e) => {
    if (dragging === null) return;
    const pt = toSvg(e), o = outAt(pt);
    setTarget(o);
    const x = o === null ? pt.x : OUT_X, y = o === null ? pt.y : outY(o);
    live.setAttribute('d', curve(IN_X, inY(dragging), x, y));
  });
  const end = (e) => {
    if (dragging === null) return;
    const o = e.type === 'pointerup' ? outAt(toSvg(e)) : null;
    if (o !== null) routes[o] = dragging;
    dragging = null; live = null;
    setTarget(null);
    render();
  };
  patch.addEventListener('pointerup', end);
  patch.addEventListener('pointercancel', end);

  // Keyboard: Enter on an input picks it, Enter on an output plugs it in
  // (or unplugs it when nothing is picked).
  patch.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    const j = e.target.closest('.jack');
    if (!j) return;
    e.preventDefault();
    const idx = Number(j.dataset.idx);
    if (j.dataset.side === 'in') picked = picked === idx ? null : idx;
    else if (picked !== null) { routes[idx] = picked; picked = null; }
    else routes[idx] = null;
    render();
  });

  const toggle = document.querySelector('[data-patch-toggle]');
  toggle?.addEventListener('click', () => {
    enabled = !enabled;
    toggle.setAttribute('aria-pressed', String(enabled));
    toggle.textContent = enabled ? 'On' : 'Off';
    render();
  });
  render();
}

// ---------- faders ----------
document.querySelectorAll('.fader').forEach((f) => {
  const out = document.querySelector(`output[for="${f.id}"]`);
  const show = () => {
    const v = Number(f.value);
    out.textContent = v <= -60 ? 'Off' : `${v > 0 ? '+' : ''}${v.toFixed(1)} dB`;
  };
  f.addEventListener('input', show);
  f.addEventListener('dblclick', () => { f.value = 0; show(); });
  show();
});

// ---------- link and main switches ----------
document.querySelectorAll('.outs .pill-key').forEach((b) => {
  b.addEventListener('click', () => b.setAttribute('aria-pressed', String(b.getAttribute('aria-pressed') !== 'true')));
});
