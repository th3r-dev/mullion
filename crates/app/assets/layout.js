// Mullion layout map: every screen of the host on a grid, the host fixed in
// the middle, the rest dragged into place. Depends on ui.js (window.mullionUI
// for the spring) and talks to the host's GET /screens and POST /screens/layout.
//
// Motion follows BRAND.md "Dragging a device tile on the layout map": 1:1 with
// the pointer keeping the grab offset; on release, project the landing point
// with deceleration 0.998, snap to the nearest open cell, spring damping 0.8,
// response 0.3, handing over the release velocity; rubber-band past the map
// bounds with constant 0.55.
(function () {
  'use strict';
  const { spring } = window.mullionUI;
  const $ = (id) => document.getElementById(id);
  const sheet = $('sheet'), panel = $('panel'), scrim = $('scrim'), board = $('board'), wrap = $('boardWrap');
  const subEl = $('sheetSub'), hintEl = $('sheetHint'), doneBtn = $('sheetDone');

  const DECEL = 0.998, RUBBER = 0.55, GAP = 10;
  // The outer ring only offers somewhere to drop; it gets less room than real cells.
  const RING = 0.6;
  let hostIp = null, view = null, viewKey = '', geo = null;
  let drag = null, busy = false, pollTimer = 0, opened = false, lastFocus = null;
  const tiles = new Map();
  const slots = [];
  const grid = document.createElement('div');
  grid.className = 'grid';
  board.appendChild(grid);

  // --- sheet: rises and fades in on one spring; the scrim follows it ---
  const sheetP = spring({ from: 0, damping: 1, response: 0.35, on: (p) => {
    panel.style.opacity = p;
    panel.style.transform = `translateY(${(1 - p) * 24}px) scale(${0.97 + 0.03 * p})`;
    scrim.style.opacity = p;
    sheet.style.visibility = p < 0.01 ? 'hidden' : 'visible';
    sheet.style.pointerEvents = p > 0.5 ? 'auto' : 'none';
  }});

  const baseUrl = (ip) => `http://${ip}:${mullion.portFor(ip)}`;

  function open(ip) {
    hostIp = ip; lastFocus = document.activeElement; opened = true;
    sheet.setAttribute('aria-hidden', 'false');
    hint('');
    subEl.textContent = `Reading the screens of ${ip}`;
    sheetP.to(1);
    load(true);
    clearInterval(pollTimer);
    pollTimer = setInterval(() => { if (!drag && !busy) load(false); }, 2000);
    // Focus the dialog itself: keyboard users tab on to the tiles and Done.
    setTimeout(() => panel.focus({ preventScroll: true }), 60);
  }
  function close() {
    if (!opened) return;
    opened = false;
    clearInterval(pollTimer);
    if (drag) cancelDrag();
    sheet.setAttribute('aria-hidden', 'true');
    sheetP.to(0);
    if (lastFocus && lastFocus.focus) lastFocus.focus({ preventScroll: true });
  }
  doneBtn.addEventListener('click', close);
  scrim.addEventListener('click', close);
  document.addEventListener('keydown', (e) => { if (opened && e.key === 'Escape') close(); });

  // The Mac lets only paired devices see or move its screens.
  function needPairing() {
    if (!opened) return;
    subEl.textContent = '';
    hint(`Connect to ${hostIp} first. The Mac shows a code to pair this device, then its screens open here.`, true);
  }
  function hint(text, err) { hintEl.textContent = text; hintEl.classList.toggle('err', !!err); }

  async function load(first) {
    try {
      const r = await mullionFetch(baseUrl(hostIp) + '/screens', { cache: 'no-store', headers: mullion.authHeaders(hostIp, 'GET', '/screens') });
      if (r.status === 401) { needPairing(); return; }
      if (!r.ok) throw new Error((await r.text()).trim() || r.status);
      const v = await r.json();
      if (!opened || drag || busy) return;
      apply(v, first);
    } catch (e) {
      if (!opened) return;
      if (first || !view) { subEl.textContent = ''; hint(`Can't reach ${hostIp}. Check that Mullion is running on the Mac.`, true); }
    }
  }

  // --- model helpers ---
  const key = (c, r) => c + ',' + r;
  const hostOf = (v) => v.screens.find((s) => s.kind === 'host');
  function where(c, r) {
    const h = c > 0 ? 'right of' : c < 0 ? 'left of' : '';
    const v = r < 0 ? 'above' : r > 0 ? 'below' : '';
    if (h && v) return `${v} and ${h.replace(' of', '')} of`;
    return h || v;
  }
  function deviceColor(s, v) {
    const devices = v.screens.filter((x) => x.kind === 'device').map((x) => x.display_id).sort((a, b) => a - b);
    return `var(--device-${(devices.indexOf(s.display_id) % 3) + 1})`;
  }
  function label(s, v) {
    const me = s.device_id && window.mullion && s.device_id === mullion.deviceId();
    const size = `${s.width}×${s.height}`;
    if (s.kind === 'host') return { name: s.name || v.host_name || 'This Mac', meta: size, me: false };
    if (s.removes_in_secs != null) return { name: s.name, meta: `Leaves in ${s.removes_in_secs} s`, me, dim: true };
    if (s.kind === 'device') return { name: s.name, meta: me ? `This device · ${size}` : size, me };
    return { name: s.name, meta: size, me: false };
  }

  // --- geometry: cells around the host sized to fit the panel, plus a slim
  // outer ring one cell beyond the screens so there is always room to drop ---
  function layoutGeo(v) {
    const cols = Math.max(1, ...v.screens.map((s) => Math.abs(s.col) + 1));
    const rows = Math.max(1, ...v.screens.map((s) => Math.abs(s.row) + 1));
    const R = Math.min(cols, v.max_reach), Rr = Math.min(rows, v.max_reach);
    const maxW = Math.max(...v.screens.map((s) => s.width)), maxH = Math.max(...v.screens.map((s) => s.height));
    const ratio = Math.min(0.9, Math.max(0.45, maxH / maxW));
    const availW = Math.max(240, wrap.clientWidth), availH = Math.max(200, Math.min(window.innerHeight - 190, 640));
    const spanW = 2 * R - 1 + 2 * RING, spanH = 2 * Rr - 1 + 2 * RING;
    let cellW = Math.min(220, availW / spanW);
    let cellH = cellW * ratio;
    if (cellH * spanH > availH) { cellH = availH / spanH; cellW = cellH / ratio; }
    const scale = Math.min((cellW - GAP) / maxW, (cellH - GAP) / maxH);
    return { R, Rr, cellW, cellH, scale, W: cellW * spanW, H: cellH * spanH };
  }
  // One axis: where track k starts and how long it is (ring tracks are slim).
  function track(k, R, cell) {
    const len = Math.abs(k) === R ? cell * RING : cell;
    const start = k <= -R ? 0 : cell * RING + (k + R - 1) * cell;
    return { start, len };
  }
  // A tile sits centred in its cell; in the ring it hugs the edge facing the host.
  function axisPos(k, R, cell, size) {
    const { start, len } = track(k, R, cell);
    if (Math.abs(k) === R) return k > 0 ? start + GAP / 2 : start + len - size - GAP / 2;
    return start + (len - size) / 2;
  }
  function topLeft(g, t, c, r) { return { x: axisPos(c, g.R, g.cellW, t.w), y: axisPos(r, g.Rr, g.cellH, t.h) }; }

  // --- tiles ---
  function makeTile(s) {
    const el = document.createElement('div');
    el.className = 'tile';
    el.innerHTML = '<span class="tdot" aria-hidden="true"></span><span class="tname"></span><span class="tmeta"></span>';
    const t = { id: s.display_id, el, s, w: 0, h: 0, ready: false, dim: false, col: s.col, row: s.row };
    const paint = () => {
      if (!t.ready) return;
      const sc = (0.92 + 0.08 * t.appear.value) * (1 + 0.04 * t.lift.value);
      el.style.transform = `translate3d(${t.x.value}px, ${t.y.value}px, 0) scale(${sc})`;
      el.style.opacity = Math.max(0, Math.min(1, t.appear.value)) * (t.dim ? 0.55 : 1);
    };
    t.x = spring({ from: 0, damping: 0.8, response: 0.3, on: paint });
    t.y = spring({ from: 0, damping: 0.8, response: 0.3, on: paint });
    t.lift = spring({ from: 0, damping: 1, response: 0.25, on: paint });
    t.appear = spring({ from: 0, damping: 1, response: 0.3, on: paint });
    t.paint = paint;
    el.addEventListener('pointerdown', (e) => startDrag(t, e));
    el.addEventListener('keydown', (e) => keyMove(t, e));
    board.appendChild(el);
    return t;
  }

  function apply(v, first) {
    const k = JSON.stringify(v);
    const sameData = k === viewKey;
    viewKey = k; view = v;
    const host = hostOf(v);
    subEl.textContent = !v.can_arrange
      ? `${v.host_name || hostIp} can't move screens on this OS yet.`
      : v.screens.length < 2
        ? `Only ${host ? host.name : 'the host'} so far. Connect a device in Extend to add its screen.`
        : `Drag a screen to move it. ${host ? host.name : 'The host'} stays in the middle.`;
    if (sameData && !first && geo) return;
    render(first);
  }

  function render(first) {
    const v = view;
    const g = layoutGeo(v);
    const resized = !geo || g.cellW !== geo.cellW || g.cellH !== geo.cellH;
    geo = g;
    board.style.width = g.W + 'px';
    board.style.height = g.H + 'px';
    // Dots on the corners of the full-size cells only.
    grid.style.left = g.cellW * RING + 'px'; grid.style.top = g.cellH * RING + 'px';
    grid.style.width = g.cellW * (2 * g.R - 1) + 'px'; grid.style.height = g.cellH * (2 * g.Rr - 1) + 'px';
    grid.style.backgroundSize = `${g.cellW}px ${g.cellH}px`;
    grid.style.backgroundPosition = `${-g.cellW / 2}px ${-g.cellH / 2}px`;
    const seen = new Set();
    for (const s of v.screens) {
      seen.add(s.display_id);
      let t = tiles.get(s.display_id);
      const fresh = !t;
      if (!t) { t = makeTile(s); tiles.set(s.display_id, t); }
      t.s = s; t.col = s.col; t.row = s.row;
      const L = label(s, v);
      t.dim = !!L.dim;
      t.el.className = `tile ${s.kind}` + (L.me ? ' me' : '');
      t.el.style.setProperty('--dev', s.kind === 'device' ? deviceColor(s, v) : 'var(--line)');
      t.el.querySelector('.tname').textContent = L.name;
      t.el.querySelector('.tmeta').textContent = L.meta;
      t.el.title = `${L.name} · ${L.meta}`;
      const movable = v.can_arrange && s.kind !== 'host';
      t.el.tabIndex = movable ? 0 : -1;
      t.el.setAttribute('role', movable ? 'button' : 'img');
      const pos = s.kind === 'host' ? 'in the middle' : `${where(s.col, s.row)} ${hostOf(v).name}`;
      t.el.setAttribute('aria-label', `${L.name}, ${pos}${movable ? '. Arrow keys move it.' : ''}`);
      t.w = Math.round(s.width * g.scale); t.h = Math.round(s.height * g.scale);
      t.el.style.width = t.w + 'px'; t.el.style.height = t.h + 'px';
      t.el.classList.toggle('compact', t.w < 104);
      const p = topLeft(g, t, s.col, s.row);
      if (fresh || (first && resized)) {
        t.x.set(p.x); t.y.set(p.y); t.ready = true;
        t.appear.to(1); t.paint();
      } else {
        t.x.to(p.x); t.y.to(p.y); t.paint();
      }
    }
    for (const [id, t] of tiles) {
      if (seen.has(id)) continue;
      tiles.delete(id);
      t.appear.to(0);
      setTimeout(() => t.el.remove(), 320);
    }
  }

  // --- where a tile may go: open cells that touch a screen edge-on, or a
  // non-host screen to swap with; the whole desk must stay linked ---
  function linked(cells) {
    const set = new Set(cells.map(([c, r]) => key(c, r)));
    const seen = new Set(['0,0']), queue = [[0, 0]];
    while (queue.length) {
      const [c, r] = queue.shift();
      for (let dc = -1; dc <= 1; dc++) for (let dr = -1; dr <= 1; dr++) {
        const k = key(c + dc, r + dr);
        if (set.has(k) && !seen.has(k)) { seen.add(k); queue.push([c + dc, r + dr]); }
      }
    }
    return cells.every(([c, r]) => seen.has(key(c, r)));
  }
  function candidates(t) {
    const reach = view.max_reach;
    const others = [...tiles.values()].filter((o) => o !== t);
    const at = new Map(others.map((o) => [key(o.col, o.row), o]));
    const out = [{ col: t.col, row: t.row, swap: null }];
    for (let c = -reach; c <= reach; c++) for (let r = -reach; r <= reach; r++) {
      if ((c === 0 && r === 0) || (c === t.col && r === t.row)) continue;
      const o = at.get(key(c, r));
      if (o && o.s.kind === 'host') continue;
      const final = others.filter((x) => x.s.kind !== 'host').map((x) => (x === o ? [t.col, t.row] : [x.col, x.row]));
      const occupied = new Set(final.map(([a, b]) => key(a, b)).concat('0,0'));
      const edgeOn = [[1, 0], [-1, 0], [0, 1], [0, -1]].some(([dc, dr]) => occupied.has(key(c + dc, r + dr)));
      if (!edgeOn) continue;
      if (!linked(final.concat([[c, r]]))) continue;
      out.push({ col: c, row: r, swap: o || null });
    }
    return out;
  }

  function showSlots(t, list) {
    hideSlots();
    for (const k of list) {
      const el = document.createElement('div');
      el.className = 'slot' + (k.swap ? ' swap' : '') + (k.col === t.col && k.row === t.row ? ' home' : '');
      const p = topLeft(geo, t, k.col, k.row);
      el.style.width = t.w + 'px'; el.style.height = t.h + 'px';
      el.style.transform = `translate3d(${p.x}px, ${p.y}px, 0)`;
      board.insertBefore(el, board.firstChild);
      slots.push({ el, k });
    }
    requestAnimationFrame(() => board.classList.add('dragging'));
  }
  function hideSlots() {
    board.classList.remove('dragging');
    const old = slots.splice(0);
    setTimeout(() => old.forEach((s) => s.el.remove()), 180);
  }
  function nearest(list, t, cx, cy) {
    let best = list[0], bd = Infinity;
    for (const k of list) {
      const p = topLeft(geo, t, k.col, k.row), m = { x: p.x + t.w / 2, y: p.y + t.h / 2 };
      const d = (m.x - cx) ** 2 + (m.y - cy) ** 2;
      if (d < bd) { bd = d; best = k; }
    }
    return best;
  }
  function markHot(k) { for (const s of slots) s.el.classList.toggle('hot', s.k === k && !s.el.classList.contains('home')); }

  // --- drag ---
  const rubber = (over, dim) => (1 - 1 / ((over * RUBBER) / dim + 1)) * dim;
  function band(v, min, max, dim) {
    if (v < min) return min - rubber(min - v, dim);
    if (v > max) return max + rubber(v - max, dim);
    return v;
  }
  function startDrag(t, e) {
    if (!view || !view.can_arrange || t.s.kind === 'host' || busy || drag) return;
    if (e.button !== undefined && e.button !== 0) return;
    e.preventDefault();
    t.el.setPointerCapture(e.pointerId);
    const b = board.getBoundingClientRect();
    const list = candidates(t);
    drag = { t, pid: e.pointerId, gx: e.clientX - b.left - t.x.value, gy: e.clientY - b.top - t.y.value, list, samples: [], hot: list[0], from: { col: t.col, row: t.row } };
    t.el.classList.add('dragging');
    t.lift.to(1);
    showSlots(t, list);
    hint('');
  }
  function moveDrag(e) {
    if (!drag || e.pointerId !== drag.pid) return;
    const { t } = drag;
    const b = board.getBoundingClientRect();
    const x = band(e.clientX - b.left - drag.gx, 0, geo.W - t.w, geo.cellW);
    const y = band(e.clientY - b.top - drag.gy, 0, geo.H - t.h, geo.cellH);
    t.x.set(x); t.y.set(y);
    const now = performance.now();
    drag.samples.push({ t: now, x, y });
    while (drag.samples.length > 2 && now - drag.samples[0].t > 100) drag.samples.shift();
    const hot = nearest(drag.list, t, x + t.w / 2, y + t.h / 2);
    if (hot !== drag.hot) { drag.hot = hot; markHot(hot); }
  }
  function velocity(samples) {
    if (samples.length < 2) return { vx: 0, vy: 0 };
    const a = samples[0], b = samples[samples.length - 1], dt = (b.t - a.t) / 1000;
    if (dt <= 0) return { vx: 0, vy: 0 };
    return { vx: (b.x - a.x) / dt, vy: (b.y - a.y) / dt };
  }
  function endDrag(e) {
    if (!drag || e.pointerId !== drag.pid) return;
    const d = drag; drag = null;
    const { t } = d;
    const { vx, vy } = velocity(d.samples);
    // Where a flick would come to rest, then the open cell nearest to that.
    const project = (v) => (v / 1000) * DECEL / (1 - DECEL);
    const k = nearest(d.list, t, t.x.value + t.w / 2 + project(vx), t.y.value + t.h / 2 + project(vy));
    t.el.classList.remove('dragging');
    t.lift.to(0);
    hideSlots();
    land(t, k, d.from, vx, vy);
  }
  function cancelDrag() {
    const d = drag; drag = null;
    if (!d) return;
    d.t.el.classList.remove('dragging'); d.t.lift.to(0); hideSlots();
    const p = topLeft(geo, d.t, d.from.col, d.from.row);
    d.t.x.to(p.x); d.t.y.to(p.y);
  }
  board.addEventListener('pointermove', moveDrag);
  board.addEventListener('pointerup', endDrag);
  board.addEventListener('pointercancel', (e) => { if (drag && e.pointerId === drag.pid) cancelDrag(); });
  board.addEventListener('lostpointercapture', (e) => { if (drag && e.pointerId === drag.pid) cancelDrag(); });

  function keyMove(t, e) {
    const dir = { ArrowRight: [1, 0], ArrowLeft: [-1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!dir || !view || !view.can_arrange || t.s.kind === 'host' || busy || drag) return;
    e.preventDefault();
    const k = candidates(t).find((x) => x.col === t.col + dir[0] && x.row === t.row + dir[1]);
    if (!k) { hint(`${t.s.name} can't go there; it would not touch the other screens.`); return; }
    land(t, k, { col: t.col, row: t.row }, 0, 0);
  }

  // Spring the tile (and a swapped one) into place, then ask the host.
  async function land(t, k, from, vx, vy) {
    const p = topLeft(geo, t, k.col, k.row);
    t.x.to(p.x, vx); t.y.to(p.y, vy);
    if (k.col === from.col && k.row === from.row) return;
    const cells = [{ display_id: t.id, col: k.col, row: k.row }];
    t.col = k.col; t.row = k.row;
    if (k.swap) {
      const o = k.swap;
      const q = topLeft(geo, o, from.col, from.row);
      o.x.to(q.x); o.y.to(q.y);
      o.col = from.col; o.row = from.row;
      cells.push({ display_id: o.id, col: from.col, row: from.row });
    }
    busy = true;
    board.setAttribute('aria-busy', 'true');
    try {
      const body = JSON.stringify({ cells });
      const r = await mullionFetch(baseUrl(hostIp) + '/screens/layout', {
        method: 'POST', headers: Object.assign({ 'Content-Type': 'application/json' }, mullion.authHeaders(hostIp, 'POST', '/screens/layout', body)), body,
      });
      const text = await r.text();
      if (!r.ok) throw new Error(text.trim() || `HTTP ${r.status}`);
      const v = JSON.parse(text);
      busy = false;
      apply(v, false);
      const host = hostOf(v);
      hint(`Moved ${t.s.name} ${where(k.col, k.row)} ${host ? host.name : 'the host'}${k.swap ? `, swapped with ${k.swap.s.name}` : ''}.`);
    } catch (e) {
      busy = false;
      // Put both tiles back where the host still has them.
      t.col = from.col; t.row = from.row;
      const back = topLeft(geo, t, from.col, from.row);
      t.x.to(back.x); t.y.to(back.y);
      if (k.swap) { const o = k.swap; o.col = k.col; o.row = k.row; const q = topLeft(geo, o, k.col, k.row); o.x.to(q.x); o.y.to(q.y); }
      // A network failure is a TypeError; anything else is the host's own reason (409).
      const why = e instanceof TypeError ? `Can't reach ${hostIp}.` : String(e.message || e);
      hint(`${why} ${t.s.name} stays where it was.`, true);
    } finally {
      board.removeAttribute('aria-busy');
    }
  }

  let resizeTimer = 0;
  window.addEventListener('resize', () => { clearTimeout(resizeTimer); resizeTimer = setTimeout(() => { if (opened && view && !drag) render(false); }, 120); });

  window.mullionLayout = { open, close, get view() { return view; } };

  // /receiver#screens (the Mac's menu bar: Arrange Screens…) opens the map
  // of the host that served the page.
  if (location.hash === '#screens' && /^https?:$/.test(location.protocol) && location.hostname) {
    setTimeout(() => open(location.hostname), 250);
  }
})();
