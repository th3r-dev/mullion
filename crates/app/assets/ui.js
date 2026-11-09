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
  ipEl.value = ipEl.value || localStorage.getItem('mullion.host') || '';
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
    const ip = ipEl.value.trim();
    if (!ip) { hint.textContent = 'Enter the IP shown by Mullion on the Mac.'; hint.classList.add('err'); ipEl.focus(); return; }
    localStorage.setItem('mullion.host', ip);
    mullion.setDeviceName(nameEl.value);
    nameOut.textContent = nameEl.value || mullion.deviceName();
    hint.textContent = `Connecting to ${ip}`; hint.classList.remove('err');
    mullion.connect(ip, video, { mode, name: nameEl.value });
  }
  dcBtn.addEventListener('click', () => mullion.disconnect());
  modeBtn.addEventListener('click', () => { setMode(mode === 'extend' ? 'mirror' : 'extend', true); connect(); });
  fsBtn.addEventListener('click', () => { const d = document.documentElement; if (!document.fullscreenElement) (d.requestFullscreen || d.webkitRequestFullscreen).call(d); else (document.exitFullscreen || document.webkitExitFullscreen).call(document); });
  function syncFullscreen() {
    const on = !!(document.fullscreenElement || document.webkitFullscreenElement);
    fsLabel.textContent = on ? 'Exit full screen' : 'Full screen';
    fsBtn.title = fsLabel.textContent;
    setIcon(fsIcon, on ? 'exit-fullscreen' : 'fullscreen');
  }
  // Layout map (layout.js): from the toolbar while streaming, or from the
  // setup card with the IP typed in.
  screensBtn.addEventListener('click', () => {
    const ip = mullion.stats().hostIp || ipEl.value.trim();
    if (ip) window.mullionLayout.open(ip);
  });
  screensSetupBtn.addEventListener('click', () => {
    const ip = ipEl.value.trim();
    if (!ip) { hint.textContent = 'Enter the IP shown by Mullion on the Mac.'; hint.classList.add('err'); ipEl.focus(); return; }
    hint.classList.remove('err'); hint.textContent = '';
    localStorage.setItem('mullion.host', ip);
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

  // Shared with layout.js so every surface moves on the same spring.
  window.mullionUI = { spring, toast };
})();
