// Mullion receiver UI behaviour. Depends on receiver.js (window.mullion).
// A tiny interruptible spring drives every transition: re-targeting mid-flight
// starts from the current value and velocity, never from the target.
(function () {
  'use strict';
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');

  // --- spring: damping ratio + response (seconds), Apple's two parameters ---
  function spring(opts) {
    const s = { v: opts.from || 0, vel: 0, target: opts.from || 0, zeta: opts.damping ?? 1, resp: opts.response ?? 0.35, on: opts.on, raf: 0, last: 0 };
    const omega = () => (2 * Math.PI) / s.resp;
    s.on(s.v); // paint the initial state so nothing flashes before the first frame
    function step(t) {
      const dt = Math.min(0.064, (t - s.last) / 1000 || 0.016); s.last = t;
      const w = omega(), z = s.zeta;
      // semi-implicit Euler on a damped harmonic oscillator
      const a = -2 * z * w * s.vel - w * w * (s.v - s.target);
      s.vel += a * dt; s.v += s.vel * dt;
      const done = Math.abs(s.vel) < 0.01 && Math.abs(s.v - s.target) < 0.001;
      if (done) { s.v = s.target; s.vel = 0; }
      s.on(s.v);
      s.raf = done ? 0 : requestAnimationFrame(step);
    }
    return {
      to(target, velocity) {
        s.target = target;
        if (velocity !== undefined) s.vel = velocity;
        if (reduced.matches) { s.v = target; s.vel = 0; s.on(s.v); return; }
        if (!s.raf) { s.last = performance.now(); s.raf = requestAnimationFrame(step); }
      },
      set(v) { s.v = s.target = v; s.vel = 0; s.on(v); },
      get value() { return s.v; },
    };
  }

  // --- elements ---
  const $ = (id) => document.getElementById(id);
  const stage = $('stage'), video = $('v'), setup = $('setup'), card = $('card');
  const toolbar = $('toolbar'), status = $('status'), hud = $('hud');
  const ipEl = $('ip'), nameEl = $('name'), goBtn = $('go'), hint = $('hint');
  const segMirror = $('segMirror'), segExtend = $('segExtend'), thumb = $('thumb');
  const nameOut = $('nameOut'), modeBtn = $('modeBtn'), fsBtn = $('fsBtn'), dcBtn = $('dcBtn');
  const screensBtn = $('screensBtn'), screensSetupBtn = $('screensSetupBtn');
  const modeIcon = $('modeIcon'), modeLabel = $('modeLabel'), fsIcon = $('fsIcon'), fsLabel = $('fsLabel');
  // Buttons hold an icon plus a label span; change the parts, never textContent.
  const setIcon = (use, name) => use.setAttribute('href', '#i-' + name);

  let mode = localStorage.getItem('mullion.mode') || 'mirror';
  // Viewer mode: the page is in Mullion.app's "Connect to Another Computer…"
  // window, served by this Mac's own host. That host is not a target: it is
  // left out of Nearby and refused, so the Mac never streams to itself.
  const viewer = location.hash === '#viewer';
  const selfPort = +window.MULLION_HOST_PORT || 0;
  const selfIps = new Set(['127.0.0.1', 'localhost']);
  const isSelf = (ip, port) => viewer && port === selfPort && selfIps.has(ip);
  const selfKnown = viewer && selfPort
    ? mullion.hello(location.hostname, selfPort, 1500).then((h) => { if (h) (h.ips || []).forEach((ip) => selfIps.add(ip)); })
    : Promise.resolve();
  // The host prefills the address the page came from; in viewer mode that is this Mac.
  if (viewer) ipEl.value = '';
  ipEl.value = ipEl.value || localStorage.getItem('mullion.host') || '';
  selfKnown.then(() => { const t = mullion.parseTarget(ipEl.value); if (t && isSelf(t.ip, t.port)) ipEl.value = ''; });
  nameEl.value = mullion.deviceName();

  // Segmented control: thumb slides with a critically damped spring; the
  // press itself already moved it (response), the spring just settles.
  const thumbX = spring({ from: mode === 'extend' ? 1 : 0, damping: 1, response: 0.3, on: (x) => { thumb.style.transform = `translateX(${x * 100}%)`; } });
  function setMode(m, animate) {
    mode = m; localStorage.setItem('mullion.mode', m);
    segMirror.setAttribute('aria-pressed', m === 'mirror'); segExtend.setAttribute('aria-pressed', m === 'extend');
    modeLabel.textContent = m === 'extend' ? 'Extend' : 'Mirror';
    setIcon(modeIcon, m);
    animate ? thumbX.to(m === 'extend' ? 1 : 0) : thumbX.set(m === 'extend' ? 1 : 0);
  }
  setMode(mode, false);
  segMirror.addEventListener('pointerdown', () => setMode('mirror', true));
  segExtend.addEventListener('pointerdown', () => setMode('extend', true));

  // Primary button: press feedback on pointer-down, commit on up (with cancel-by-drag-away).
  let pressing = false;
  goBtn.addEventListener('pointerdown', (e) => { pressing = true; goBtn.classList.add('pressed'); goBtn.setPointerCapture(e.pointerId); });
  goBtn.addEventListener('pointermove', (e) => { if (!pressing) return; const r = goBtn.getBoundingClientRect(); const inside = e.clientX > r.left - 10 && e.clientX < r.right + 10 && e.clientY > r.top - 10 && e.clientY < r.bottom + 10; goBtn.classList.toggle('pressed', inside); });
  goBtn.addEventListener('pointerup', () => { const commit = goBtn.classList.contains('pressed'); pressing = false; goBtn.classList.remove('pressed'); if (commit) connect(); });
  goBtn.addEventListener('pointercancel', () => { pressing = false; goBtn.classList.remove('pressed'); });
  goBtn.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); connect(); } });
  ipEl.addEventListener('keydown', (e) => { if (e.key === 'Enter') connect(); });

  // Setup card <-> stage: the card materializes out (scale, blur, opacity)
  // and comes back the same way. One spring drives the three properties so
  // they stay in phase; interrupting a transition re-targets from the live value.
  const cardP = spring({ from: 1, damping: 1, response: 0.35, on: (p) => {
    // p = 1 card fully present, 0 gone
    card.style.opacity = p;
    card.style.transform = `scale(${0.96 + 0.04 * p})`;
    card.style.filter = reduced.matches ? '' : `blur(${(1 - p) * 6}px)`;
    setup.style.opacity = p;
    setup.style.pointerEvents = p > 0.5 ? 'auto' : 'none';
    setup.style.visibility = p < 0.01 ? 'hidden' : 'visible';
  }});
  const toolbarY = spring({ from: -1.6, damping: 1, response: 0.3, on: (y) => {
    toolbar.style.transform = `translate(-50%, ${y * 100}%)`;
    toolbar.style.opacity = Math.max(0, Math.min(1, 1 + y / 1.6));
    toolbar.style.pointerEvents = y > -0.5 ? 'auto' : 'none';
  }});
  const statusY = spring({ from: -2.6, damping: 1, response: 0.3, on: (y) => {
    status.style.transform = `translate(-50%, ${y * 100}%)`;
    status.style.opacity = Math.max(0, Math.min(1, 1 + y / 1.6));
  }});

  let statusTimer = 0;
  function toast(text, sticky) {
    status.querySelector('.text').textContent = text;
    statusY.to(0);
    clearTimeout(statusTimer);
    if (!sticky) statusTimer = setTimeout(() => statusY.to(-2.6), 1600);
  }

  // Toolbar auto-hide over the video; any pointer activity brings it back.
  let hideTimer = 0;
  function showChrome() {
    if (!stage.classList.contains('live')) return;
    toolbarY.to(0);
    hud.classList.remove('dim');
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => { toolbarY.to(-1.6); hud.classList.add('dim'); }, 2200);
  }
  ['pointermove', 'pointerdown', 'touchstart'].forEach((ev) => stage.addEventListener(ev, showChrome, { passive: true }));
  toolbar.addEventListener('pointermove', () => { clearTimeout(hideTimer); hideTimer = setTimeout(() => { toolbarY.to(-1.6); hud.classList.add('dim'); }, 2200); });

  function connect() {
    const target = mullion.parseTarget(ipEl.value);
    if (!target) { hint.textContent = 'Enter the address shown by Mullion.'; hint.classList.add('err'); ipEl.focus(); return; }
    if (isSelf(target.ip, target.port)) { hint.textContent = "That's this Mac. Enter the address of another computer."; hint.classList.add('err'); ipEl.focus(); return; }
    const ip = target.ip;
    localStorage.setItem('mullion.host', ipEl.value.trim());
    mullion.setDeviceName(nameEl.value);
    nameOut.textContent = nameEl.value || mullion.deviceName();
    hint.textContent = `Connecting to ${ip}`; hint.classList.remove('err');
    mullion.connect(ip, video, { mode, name: nameEl.value, port: target.port });
  }
  // --- Nearby: Mullion hosts found on this network (receiver.js scan) ---
  const hostsEl = $('hosts'), nearbyStatus = $('nearbyStatus'), nearbyEmpty = $('nearbyEmpty');
  const scanBtn = $('scanBtn'), scanBar = $('scanBar'), scanFill = $('scanFill');
  const rows = new Map(); // "name:port" -> { li, btn, host, stale }
  let scanCtl = null, lastScan = 0, stagger = 0, fillMax = 0;
  const fill = spring({ from: 0, damping: 1, response: 0.4, on: (f) => { scanFill.style.transform = `scaleX(${f})`; } });
  scanBar.classList.add('idle');

  const plural = (n, one, many) => (n === 1 ? `1 ${one}` : `${n} ${many}`);
  function hostMeta(h) {
    const parts = [h.ip];
    if (h.platform) parts.push(h.platform);
    if (h.receivers > 0) parts.push(plural(h.receivers, 'device', 'devices'));
    if (h.extend === false) parts.push('Mirror only');
    return parts.join(' · ');
  }
  function paintRow(r) {
    const h = r.host;
    r.btn.querySelector('.host-name').textContent = h.name;
    r.btn.querySelector('.host-meta').textContent = hostMeta(h);
    r.btn.querySelector('.tag').hidden = !mullion.isLastUsed(h);
    r.btn.title = `Connect to ${h.name}`;
    r.btn.setAttribute('aria-label', `${h.name}, ${hostMeta(h)}. Connect.`);
  }
  function showHost(h) {
    if ([h.ip].concat(h.ips || []).some((ip) => isSelf(ip, h.port))) return;
    const k = h.name + ':' + h.port;
    const have = rows.get(k);
    if (have) { have.host = h; have.stale = false; paintRow(have); return; }
    const li = document.createElement('li');
    const btn = document.createElement('button');
    btn.type = 'button'; btn.className = 'host';
    btn.innerHTML = '<span class="host-icon"><svg class="i" aria-hidden="true"><use href="#i-computer"/></svg></span>'
      + '<span class="host-text"><span class="host-name"></span><span class="host-meta"></span></span>'
      + '<span class="tag" hidden>Last used</span><svg class="i go" aria-hidden="true"><use href="#i-chevron"/></svg>';
    li.appendChild(btn);
    const r = { li, btn, host: h, stale: false };
    // Materialize on a spring, a beat after the row before it.
    r.appear = spring({ from: 0, damping: 1, response: 0.3, on: (p) => { btn.style.opacity = p; btn.style.transform = `translateY(${(1 - p) * 8}px) scale(${0.98 + 0.02 * p})`; } });
    btn.addEventListener('pointerdown', () => btn.classList.add('pressed'));
    ['pointerup', 'pointercancel', 'pointerleave'].forEach((ev) => btn.addEventListener(ev, () => btn.classList.remove('pressed')));
    btn.addEventListener('click', () => choose(r.host));
    rows.set(k, r);
    paintRow(r);
    // The host used last time goes first; the rest in the order they answered.
    if (!btn.querySelector('.tag').hidden) hostsEl.prepend(li); else hostsEl.appendChild(li);
    setTimeout(() => r.appear.to(1), 45 * stagger++);
    nearbyEmpty.textContent = '';
  }
  function choose(h) {
    // Show the port only when it is not the default, so the field stays familiar.
    ipEl.value = h.port === 47100 ? h.ip : `${h.ip}:${h.port}`;
    if (h.extend === false && mode === 'extend') setMode('mirror', true);
    connect();
  }
  function stopScan() {
    if (!scanCtl) return;
    scanCtl.abort(); scanCtl = null;
    scanBtn.disabled = false; scanBar.classList.add('idle');
    nearbyStatus.textContent = rows.size ? plural(rows.size, 'found', 'found') : '';
  }
  async function runScan() {
    stopScan();
    const ctl = new AbortController(); scanCtl = ctl;
    for (const r of rows.values()) r.stale = true;
    stagger = 0; fillMax = 0; fill.set(0);
    scanBar.classList.remove('idle'); scanBtn.disabled = true;
    nearbyEmpty.textContent = '';
    nearbyStatus.textContent = 'Looking on this network';
    const typed = mullion.parseTarget(ipEl.value);
    await mullion.scan({
      signal: ctl.signal,
      // parseTarget remembered a typed port for that address only; the rest
      // of the network is asked on the default port.
      seeds: typed ? [typed.ip] : [],
      onFound: (h) => { if (scanCtl !== ctl) return; showHost(h); nearbyStatus.textContent = `Looking on this network · ${plural(rows.size, 'found', 'found')}`; },
      // The sweep can grow as it learns new networks; the bar only moves forward.
      onProgress: (d, t) => { if (scanCtl !== ctl || !t) return; const f = d / t; if (f > fillMax) { fillMax = f; fill.to(f); } },
    });
    if (scanCtl !== ctl) return;
    scanCtl = null; lastScan = Date.now();
    scanBtn.disabled = false;
    fill.to(1); setTimeout(() => scanBar.classList.add('idle'), 260);
    for (const [k, r] of rows) {
      if (!r.stale) continue;
      rows.delete(k); r.appear.to(0); setTimeout(() => r.li.remove(), 260);
    }
    // Count the rows shown: in viewer mode the scan also finds this Mac, which is not listed.
    nearbyStatus.textContent = rows.size ? plural(rows.size, 'found', 'found') : '';
    if (!rows.size) nearbyEmpty.textContent = 'No computer found on this network. Open Mullion on it, or enter the address it shows.';
  }
  scanBtn.addEventListener('click', runScan);

  dcBtn.addEventListener('click', () => mullion.leave());
  modeBtn.addEventListener('click', () => { setMode(mode === 'extend' ? 'mirror' : 'extend', true); connect(); });
  // In the desktop app (Windows viewer) the page's fullscreen only fills the web
  // view; main.rs provides mullionNative to make the window itself full screen.
  let nativeFs = false;
  function setNativeFullscreen(on) { nativeFs = on; window.mullionNative.fullscreen(on); syncFullscreen(); }
  fsBtn.addEventListener('click', () => {
    if (window.mullionNative && window.mullionNative.fullscreen) { setNativeFullscreen(!nativeFs); return; }
    const d = document.documentElement;
    if (!document.fullscreenElement) (d.requestFullscreen || d.webkitRequestFullscreen).call(d); else (document.exitFullscreen || document.webkitExitFullscreen).call(document);
  });
  // Esc leaves full screen, as it does in a browser.
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && nativeFs) setNativeFullscreen(false); });
  function syncFullscreen() {
    const on = nativeFs || !!(document.fullscreenElement || document.webkitFullscreenElement);
    fsLabel.textContent = on ? 'Exit full screen' : 'Full screen';
    fsBtn.title = fsLabel.textContent;
    setIcon(fsIcon, on ? 'exit-fullscreen' : 'fullscreen');
  }
  // Layout map (layout.js): from the toolbar while streaming, or from the
  // setup card with the address entered.
  screensBtn.addEventListener('click', () => {
    const ip = mullion.stats().hostIp || ipEl.value.trim();
    if (ip) window.mullionLayout.open(ip);
  });
  screensSetupBtn.addEventListener('click', () => {
    const target = mullion.parseTarget(ipEl.value);
    const ip = target && target.ip;
    if (!ip) { hint.textContent = 'Enter the address shown by Mullion.'; hint.classList.add('err'); ipEl.focus(); return; }
    hint.classList.remove('err'); hint.textContent = '';
    localStorage.setItem('mullion.host', ipEl.value.trim());
    window.mullionLayout.open(ip);
  });
  document.addEventListener('fullscreenchange', syncFullscreen);
  document.addEventListener('webkitfullscreenchange', syncFullscreen);

  // React to transitions only; a repeated status must not replay the entrance.
  let lastStatus = null;
  mullion.onStatus((s) => {
    const host = s.hostName || s.hostIp || 'the Mac';
    if (s.status === lastStatus && s.status !== 'error') return;
    lastStatus = s.status;
    // Scanning competes with the stream for Wi-Fi; stop while connected and
    // look again on the way back if the last look is stale.
    if (s.status === 'connecting') stopScan();
    if (s.status === 'streaming') mullion.rememberHost(s.hostIp, mullion.portFor(s.hostIp));
    if ((s.status === 'idle' || s.status === 'lost' || s.status === 'error') && !scanCtl && Date.now() - lastScan > 30000) setTimeout(runScan, 400);
    switch (s.status) {
      case 'connecting': hint.textContent = `Connecting to ${s.hostIp}`; break;
      case 'connected': hint.textContent = `Connected to ${host}`; break;
      case 'streaming':
        stage.classList.add('live');
        hud.classList.add('on');
        cardP.to(0);
        toast(`Showing ${host}`);
        showChrome();
        break;
      case 'lost':
        stage.classList.remove('live');
        hud.classList.remove('on');
        cardP.to(1);
        toolbarY.to(-1.6);
        hint.textContent = `Lost ${host}. Tap Connect to try again.`; hint.classList.add('err');
        toast(`Lost ${host}`);
        break;
      case 'error':
        stage.classList.remove('live');
        hud.classList.remove('on');
        cardP.to(1);
        hint.textContent = s.error || `Can't reach ${s.hostIp}.`; hint.classList.add('err');
        break;
      case 'idle':
        stage.classList.remove('live');
        hud.classList.remove('on');
        cardP.to(1);
        toolbarY.to(-1.6);
        if (!hint.classList.contains('err')) hint.textContent = '';
        break;
    }
  });

  // HUD: numbers only, dims after 2 s without a pointer like the cursor label.
  const f = (x) => (x == null ? '–' : x.toFixed(1));
  setInterval(() => {
    const s = mullion.stats();
    hud.innerHTML = `${s.mode} · ${s.videoWidth}×${s.videoHeight}\n<b>g2g</b> p50 ${f(s.g2gP50)}  p95 ${f(s.g2gP95)} ms\nframes ${s.presented}  rtt ${f(s.rttMs)}  jbt ${s.caps ? (s.caps.jitter_buffer_target_supported ? s.caps.jitter_buffer_target_ms : 'no') : '–'}`;
  }, 250);

  // Autoconnect from ?host=..&auto=1 (loopback test).
  const q = new URLSearchParams(location.search);
  if (q.get('host')) ipEl.value = q.get('host');
  if (q.get('auto') === '1' && ipEl.value) connect();
  else selfKnown.then(runScan);

  // Version, bottom left: the host's when it served this page, the app's in the tablet app.
  if (window.MULLION_VERSION) $('version').textContent = 'Mullion ' + window.MULLION_VERSION;

  // Shared with layout.js so every surface moves on the same spring.
  window.mullionUI = { spring, toast };
})();
