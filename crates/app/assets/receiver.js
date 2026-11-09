// Mullion receiver (FR-4, FR-5, FR-9 receiver side). Runs inside the Dioxus
// WebView on Android/desktop and inside a plain browser page for smoke tests.
// Exposes window.mullion = { connect, disconnect, stats, onStatus }.
(function () {
  'use strict';
  const BLOCKS = 38, SYNC = 2, DATA = 32;
  const state = {
    pc: null, dc: null, video: null, hostIp: null,
    offset: null, rtt: null, samples: [],
    g2g: [], presented: 0, decoded: 0, decodeFail: 0, dropped: 0, lastFrameTs: 0,
    caps: null, status: 'idle', hostName: null, error: null,
    // Strip sampling: a GPU->CPU readback per row, expensive on tablets.
    // probe = host says frames carry the strip (null = old host, unknown).
    probe: null, stripMisses: 0, nextSampleAt: 0,
    listeners: [],
  };

  // --- device identity: stable id + editable name, kept in localStorage ---
  function deviceId() {
    let id = null;
    try { id = localStorage.getItem('mullion.deviceId'); } catch (e) { /* */ }
    if (!id) {
      id = (crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).slice(2) + Date.now().toString(36));
      try { localStorage.setItem('mullion.deviceId', id); } catch (e) { /* */ }
    }
    return id;
  }
  function guessName() {
    const ua = navigator.userAgent;
    const m = ua.match(/\(([^)]+)\)/); const inner = m ? m[1] : '';
    // Android gives "Linux; Android 14; SM-X710" -> SM-X710; desktop gets the OS.
    const parts = inner.split(';').map(x => x.trim());
    const model = parts.find(x => /^[A-Z]{1,4}[- ]?[A-Z0-9]{2,}/.test(x) && !/^Android|^Linux|^Win|^Mac|^X11|^Intel/.test(x));
    if (model) return model.replace(/ Build.*/, '');
    if (/Macintosh/.test(ua)) return 'Mac';
    if (/Windows/.test(ua)) return 'Windows PC';
    if (/Android/.test(ua)) return 'Android tablet';
    return 'Tablet';
  }
  function deviceName() {
    let n = null;
    try { n = localStorage.getItem('mullion.deviceName'); } catch (e) { /* */ }
    return n || guessName();
  }
  function setDeviceName(n) {
    try { localStorage.setItem('mullion.deviceName', String(n).slice(0, 32)); } catch (e) { /* */ }
  }

  function setStatus(s, extra) {
    state.status = s;
    if (extra && extra.error) state.error = extra.error;
    for (const l of state.listeners) { try { l(snapshot()); } catch (e) { /* ignore */ } }
  }

  function p(values, q) {
    if (!values.length) return null;
    const v = values.slice().sort((a, b) => a - b);
    const rank = Math.min(v.length, Math.max(1, Math.ceil(q * v.length)));
    return v[rank - 1];
  }

  function snapshot() {
    const recent = state.g2g.slice(-600);
    return {
      status: state.status, error: state.error, hostName: state.hostName, hostIp: state.hostIp, mode: state.mode || 'mirror',
      offsetMs: state.offset, rttMs: state.rtt,
      presented: state.presented, decoded: state.decoded, decodeFail: state.decodeFail, dropped: state.dropped,
      g2gP50: p(recent, 0.5), g2gP95: p(recent, 0.95), g2gLast: recent.length ? recent[recent.length - 1] : null,
      caps: state.caps,
      videoWidth: state.video ? state.video.videoWidth : 0, videoHeight: state.video ? state.video.videoHeight : 0,
    };
  }

  // --- strip decoding (mirror of mullion_proto::probe::decode) ---
  function pop16(x) { x = x - ((x >>> 1) & 0x5555); x = (x & 0x3333) + ((x >>> 2) & 0x3333); return (((x + (x >>> 4)) & 0x0F0F) * 0x0101) >>> 8 & 0x1F; }
  function decodeStrip(luma) {
    if (luma.length < BLOCKS) return { err: 'short' };
    const white = (luma[0] + luma[BLOCKS - 1]) / 2, black = (luma[1] + luma[BLOCKS - 2]) / 2;
    if (white - black < 64) return { err: 'nosync' };
    const mid = (white + black) / 2;
    const bit = i => luma[i] > mid ? 1 : 0;
    if (bit(0) !== 1 || bit(1) !== 0 || bit(BLOCKS - 2) !== 0 || bit(BLOCKS - 1) !== 1) return { err: 'nosync' };
    let v = 0;
    for (let i = 0; i < DATA; i++) v = ((v << 1) | bit(SYNC + i)) >>> 0;
    if (bit(SYNC + DATA) !== (pop16(v & 0xFFFF) & 1) || bit(SYNC + DATA + 1) !== (pop16(v >>> 16) & 1)) return { err: 'parity' };
    return { value: v };
  }
  function unwrapMs(low32, referenceMs) {
    const EP = 4294967296;
    const base = Math.floor(referenceMs / EP) * EP;
    const cands = [base - EP + low32, base + low32, base + EP + low32];
    let best = cands[0];
    for (const c of cands) if (Math.abs(c - referenceMs) < Math.abs(best - referenceMs)) best = c;
    return best;
  }

  // --- frame sampling ---
  const canvas = document.createElement('canvas');
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  function sampleRow(video, yFrac) {
    const w = video.videoWidth, h = video.videoHeight;
    if (!w || !h) return null;
    canvas.width = w; canvas.height = 1;
    ctx.drawImage(video, 0, Math.floor(h * yFrac), w, 1, 0, 0, w, 1);
    const px = ctx.getImageData(0, 0, w, 1).data;
    const luma = new Uint8Array(w);
    for (let x = 0; x < w; x++) luma[x] = (px[x * 4] * 77 + px[x * 4 + 1] * 150 + px[x * 4 + 2] * 29) >> 8;
    // Strip extent: first and last bright pixel (left sync and right tail are white).
    let x0 = -1, x1 = -1;
    for (let x = 0; x < w; x++) if (luma[x] > 140) { x0 = x; break; }
    for (let x = w - 1; x >= 0; x--) if (luma[x] > 140) { x1 = x; break; }
    if (x0 < 0 || x1 - x0 < BLOCKS * 2) return null;
    const bw = (x1 - x0 + 1) / BLOCKS;
    const out = new Uint8Array(BLOCKS);
    for (let i = 0; i < BLOCKS; i++) out[i] = luma[Math.min(w - 1, Math.floor(x0 + bw * (i + 0.5)))];
    return out;
  }
  function readStrip(video) {
    for (const y of [0.06, 0.03, 0.09, 0.12, 0.15]) {
      const row = sampleRow(video, y);
      if (!row) continue;
      const r = decodeStrip(row);
      if (r.value !== undefined) return r.value;
    }
    return null;
  }

  function onFrame(now, meta) {
    const v = state.video;
    if (!v || !state.pc) return;
    state.presented++;
    // presentedFrames counts frames the UA composited; callbacks we missed
    // (gap > 1) are frames presented without our sampling, not decode drops.
    if (meta && meta.presentedFrames) {
      if (state.lastFrameTs) { const gap = meta.presentedFrames - state.lastFrameTs - 1; if (gap > 0) state.dropped += gap; }
      state.lastFrameTs = meta.presentedFrames;
    }
    // No strip on the frames: skip the readback entirely. Unknown (old host):
    // after 30 misses in a row, look only once a second.
    if (state.probe === false || (state.stripMisses >= 30 && now < state.nextSampleAt)) { v.requestVideoFrameCallback(onFrame); return; }
    const low = readStrip(v);
    if (low === null) { state.decodeFail++; if (++state.stripMisses >= 30) state.nextSampleAt = now + 1000; }
    else state.stripMisses = 0;
    if (low !== null && state.offset !== null) {
      state.decoded++;
      const presentedMs = performance.timeOrigin + (meta && meta.expectedDisplayTime ? meta.expectedDisplayTime : now);
      const painted = unwrapMs(low, presentedMs + state.offset);
      const g2g = (presentedMs + state.offset) - painted;
      // A strip that passes parity by chance (no probe on screen) gives an
      // absurd value; only plausible latencies count.
      if (g2g < -200 || g2g > 5000) { state.decodeFail++; state.decoded--; v.requestVideoFrameCallback(onFrame); return; }
      state.g2g.push(g2g);
      if (state.g2g.length > 20000) state.g2g.splice(0, 10000);
      if (state.dc && state.dc.readyState === 'open' && (state.decoded % 2 === 0)) {
        state.dc.send(JSON.stringify({ type: 'presented', painted_host_ms: painted, presented_ms: presentedMs, offset_ms: state.offset }));
      }
    }
    v.requestVideoFrameCallback(onFrame);
  }

  // --- clock sync over the probe data channel ---
  function startClockSync() {
    const dc = state.dc;
    let sent = 0;
    const timer = setInterval(() => {
      if (dc.readyState !== 'open') { clearInterval(timer); return; }
      dc.send(JSON.stringify({ type: 'ping', t0: Date.now() }));
      if (++sent >= 20) clearInterval(timer);
    }, 50);
    // Re-sync every 10 s to track drift.
    setTimeout(() => { if (state.status === 'streaming') startClockSync(); }, 10000);
  }
  function onProbeMessage(ev) {
    let m; try { m = JSON.parse(ev.data); } catch (e) { return; }
    if (m.type === 'pong') {
      const t2 = Date.now();
      const s = { t0: m.t0, t1: m.t1, t2, rtt: t2 - m.t0, offset: m.t1 - (m.t0 + t2) / 2 };
      if (s.rtt < 0 || !isFinite(s.rtt)) return;
      state.samples.push(s);
      if (state.samples.length > 60) state.samples.shift();
      let best = state.samples[0];
      for (const x of state.samples) if (x.rtt < best.rtt) best = x;
      // Stats only; the HUD polls them. Re-announcing the status here made the
      // UI replay its "streaming" entrance (toolbar + toast) on every pong.
      state.offset = best.offset; state.rtt = best.rtt;
    }
  }

  // --- FR-5: jitter buffer target ---
  function applyLowLatency(pc) {
    const caps = { jitter_buffer_target_supported: false, jitter_buffer_target_ms: null, user_agent: navigator.userAgent };
    for (const r of pc.getReceivers()) {
      if (r.track && r.track.kind !== 'video') continue;
      if ('jitterBufferTarget' in r) {
        try { r.jitterBufferTarget = 0; caps.jitter_buffer_target_supported = true; caps.jitter_buffer_target_ms = r.jitterBufferTarget; } catch (e) { caps.jitter_buffer_target_error = String(e); }
      } else if ('playoutDelayHint' in r) {
        try { r.playoutDelayHint = 0; caps.jitter_buffer_target_supported = true; caps.jitter_buffer_target_ms = r.playoutDelayHint; caps.via = 'playoutDelayHint'; } catch (e) { caps.jitter_buffer_target_error = String(e); }
      }
    }
    state.caps = caps;
    return caps;
  }

  async function connect(hostIp, videoEl, opts) {
    opts = opts || {};
    const port = opts.port || portFor(hostIp);
    const mode = opts.mode === 'extend' ? 'extend' : 'mirror';
    disconnect();
    state.hostIp = hostIp; state.video = videoEl; state.error = null; state.mode = mode;
    state.g2g = []; state.samples = []; state.offset = null; state.presented = 0; state.decoded = 0; state.decodeFail = 0; state.dropped = 0; state.lastFrameTs = 0;
    state.probe = null; state.stripMisses = 0; state.nextSampleAt = 0;
    setStatus('connecting');
    const pc = new RTCPeerConnection({ iceServers: [] });
    state.pc = pc;
    pc.addTransceiver('video', { direction: 'recvonly' });
    const dc = pc.createDataChannel('probe', { ordered: true });
    state.dc = dc;
    dc.onopen = () => { startClockSync(); const caps = applyLowLatency(pc); dc.send(JSON.stringify(Object.assign({ type: 'capabilities' }, caps))); };
    dc.onmessage = onProbeMessage;
    pc.ontrack = (ev) => {
      videoEl.muted = true; videoEl.autoplay = true; videoEl.playsInline = true;
      videoEl.srcObject = ev.streams[0] || new MediaStream([ev.track]);
      videoEl.play().catch(e => setStatus('streaming', { error: 'autoplay blocked: ' + e }));
      setStatus('streaming');
      if (videoEl.requestVideoFrameCallback) videoEl.requestVideoFrameCallback(onFrame);
      else state.error = 'requestVideoFrameCallback unsupported';
    };
    pc.oniceconnectionstatechange = () => {
      if (pc.iceConnectionState === 'failed' || pc.iceConnectionState === 'disconnected' || pc.iceConnectionState === 'closed') {
        setStatus('lost', { error: 'Lost ' + (state.hostName || hostIp) + '.' });
      }
    };
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    // ice-lite host: our candidates must be in the SDP. Wait for gathering, max 1.5 s.
    await new Promise(res => {
      if (pc.iceGatheringState === 'complete') return res();
      const t = setTimeout(res, 1500);
      pc.onicegatheringstatechange = () => { if (pc.iceGatheringState === 'complete') { clearTimeout(t); res(); } };
    });
    let answer;
    try {
      const ctrl = new AbortController();
      const to = setTimeout(() => ctrl.abort(), 5000);
      const resp = await fetch(`http://${hostIp}:${port}/offer`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ type: 'offer', sdp: pc.localDescription.sdp, mode, device_id: deviceId(), device_name: opts.name || deviceName() }), signal: ctrl.signal,
      });
      clearTimeout(to);
      if (!resp.ok) throw new Error(`${resp.status} ${await resp.text()}`);
      answer = await resp.json();
    } catch (e) {
      setStatus('error', { error: `Can't reach ${hostIp}. ${e.name === 'AbortError' ? 'No answer in 5 s.' : e.message}` });
      disconnect();
      return;
    }
    state.hostName = answer.host_name || hostIp;
    state.probe = typeof answer.probe === 'boolean' ? answer.probe : null;
    await pc.setRemoteDescription({ type: 'answer', sdp: answer.sdp });
    if (state.status !== 'streaming') setStatus('connected');
  }

  function disconnect() {
    if (state.dc) { try { state.dc.close(); } catch (e) { /* */ } }
    if (state.pc) { try { state.pc.close(); } catch (e) { /* */ } }
    if (state.video) { try { state.video.srcObject = null; } catch (e) { /* */ } }
    state.pc = null; state.dc = null;
    if (state.status !== 'error') setStatus('idle');
  }

  // --- discovery: find Mullion hosts on this network ---
  // Browsers cannot do mDNS or UDP broadcast, so this asks GET /hello on likely
  // addresses: the host that served this page, hosts used before, then every
  // address in their /24 and in this device's own /24 (when the browser lets
  // us see it), then the usual home ranges if nothing else is known.
  const HOSTS_KEY = 'mullion.hosts';
  const isIPv4 = (s) => /^\d{1,3}(\.\d{1,3}){3}$/.test(s || '') && s.split('.').every((n) => +n <= 255);
  const isPrivate = (ip) => /^10\./.test(ip) || /^192\.168\./.test(ip) || /^172\.(1[6-9]|2\d|3[01])\./.test(ip);
  const prefix24 = (ip) => ip.split('.').slice(0, 3).join('.');
  // Port of each host we have heard from, so a host on a non-default port is
  // dialled where it answered.
  const knownPorts = new Map();
  function portFor(ip) {
    if (knownPorts.has(ip)) return knownPorts.get(ip);
    if (/^https?:$/.test(location.protocol) && location.hostname === ip) {
      // Served by this host: it says where offers go (the page may have come
      // from the web port, 80, which is not where they go).
      if (window.MULLION_HOST_PORT) return +window.MULLION_HOST_PORT;
      if (location.port) return +location.port;
    }
    return 47100;
  }
  // "10.0.0.5" or "10.0.0.5:47199" -> { ip, port }
  function parseTarget(text) {
    const m = /^\s*([^\s:]+)(?::(\d{1,5}))?\s*$/.exec(text || '');
    if (!m) return null;
    const port = m[2] ? +m[2] : null;
    if (port !== null && (port < 1 || port > 65535)) return null;
    if (port) knownPorts.set(m[1], port);
    return { ip: m[1], port: port || portFor(m[1]) };
  }
  // Hosts connected to before, newest first, as "ip" or "ip:port".
  const targetKey = (ip, port) => (!port || port === 47100 ? ip : `${ip}:${port}`);
  function recentTargets() {
    try { return (JSON.parse(localStorage.getItem(HOSTS_KEY) || '[]') || []).filter((t) => isIPv4(String(t).split(':')[0])); } catch (e) { return []; }
  }
  const recentHosts = () => [...new Set(recentTargets().map((t) => t.split(':')[0]))];
  function rememberHost(ip, port) {
    if (!isIPv4(ip)) return;
    const k = targetKey(ip, port);
    try { localStorage.setItem(HOSTS_KEY, JSON.stringify([k].concat(recentTargets().filter((x) => x !== k)).slice(0, 6))); } catch (e) { /* */ }
  }
  // Was this host (any of its addresses) the last one connected to?
  function isLastUsed(h) {
    const last = recentTargets()[0];
    return !!last && [h.ip].concat(h.ips || []).some((ip) => targetKey(ip, h.port) === last);
  }
  // This device's own IPv4 addresses from ICE host candidates. Chrome hides
  // them behind mDNS names unless the page has camera/mic permission, so this
  // often finds nothing; the sweep does not depend on it.
  function localIps(timeoutMs) {
    return new Promise((resolve) => {
      const out = new Set();
      let pc;
      try { pc = new RTCPeerConnection({ iceServers: [] }); } catch (e) { resolve([]); return; }
      const done = () => { try { pc.close(); } catch (e) { /* */ } resolve([...out]); };
      const t = setTimeout(done, timeoutMs || 600);
      pc.onicecandidate = (ev) => {
        if (!ev.candidate) { clearTimeout(t); done(); return; }
        const m = /(\d{1,3}(?:\.\d{1,3}){3})/.exec(ev.candidate.candidate || '');
        if (m && isPrivate(m[1])) out.add(m[1]);
      };
      pc.createDataChannel('x');
      pc.createOffer().then((o) => pc.setLocalDescription(o)).catch(() => { clearTimeout(t); done(); });
    });
  }
  async function hello(ip, port, timeoutMs, outer) {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), timeoutMs);
    const stop = () => ctrl.abort();
    if (outer) outer.addEventListener('abort', stop);
    const t0 = performance.now();
    try {
      const r = await fetch(`http://${ip}:${port}/hello`, { signal: ctrl.signal, cache: 'no-store', credentials: 'omit' });
      if (!r.ok) return null;
      const h = await r.json();
      if (!h || h.app !== 'mullion') return null;
      knownPorts.set(ip, port);
      return Object.assign({}, h, { ip, port, ms: Math.round(performance.now() - t0) });
    } catch (e) {
      return null;
    } finally {
      clearTimeout(t);
      if (outer) outer.removeEventListener('abort', stop);
    }
  }
  // opts: { port, seeds: [ip], signal, onFound(host), onProgress(done, total), fallback: true }
  // Resolves with every host found, one entry per host (a host answering on
  // two addresses is listed once, at the address that answered first).
  async function scan(opts) {
    opts = opts || {};
    const servedByHost = /^https?:$/.test(location.protocol) && isIPv4(location.hostname);
    const port = opts.port || (servedByHost ? portFor(location.hostname) : 47100);
    const signal = opts.signal;
    const found = new Map(), tried = new Set();
    let total = 0, done = 0, planned = 0;
    // Report against the work we expect (seeds plus a /24 per known network),
    // so a quick seed check does not read as "finished".
    const progress = () => { if (opts.onProgress) opts.onProgress(done, Math.max(total, planned)); };
    const report = (h) => {
      const k = h.name + ':' + h.port;
      if (found.has(k)) return;
      found.set(k, h);
      if (opts.onFound) opts.onFound(h);
    };
    async function sweep(ips, concurrency, timeoutMs) {
      const queue = ips.filter((ip) => isIPv4(ip) && !tried.has(ip));
      queue.forEach((ip) => tried.add(ip));
      total += queue.length; progress();
      let i = 0;
      const worker = async () => {
        while (i < queue.length && !(signal && signal.aborted)) {
          const h = await hello(queue[i++], port, timeoutMs, signal);
          done++; progress();
          if (h) report(h);
        }
      };
      await Promise.all(Array.from({ length: Math.min(concurrency, queue.length) }, worker));
    }
    const uniq = (a) => [...new Set(a)];
    const seeds = uniq([].concat(servedByHost ? [location.hostname] : [], opts.seeds || [], recentHosts()));
    const seedNets = uniq(seeds.filter(isPrivate).map(prefix24)).length;
    planned = seeds.length + 254 * Math.max(1, seedNets);
    // Known addresses first: usually the answer in well under a second.
    await sweep(seeds, 8, 1500);
    const local = await localIps(600);
    const prefixes = [];
    const addPrefixes = (ips) => ips.filter(isPrivate).map(prefix24).forEach((p) => { if (!prefixes.includes(p)) prefixes.push(p); });
    addPrefixes([...found.values()].flatMap((h) => [h.ip].concat(h.ips || [])));
    addPrefixes(seeds);
    addPrefixes(local);
    if (!prefixes.length && opts.fallback !== false) prefixes.push('192.168.1', '192.168.0', '10.0.0');
    // Now the networks are known, so is the work: re-plan before sweeping.
    planned = total + 254 * prefixes.length;
    progress();
    // Whole /24s, nearest first; a host found on the way may add its other networks.
    for (let n = 0; n < prefixes.length && !(signal && signal.aborted); n++) {
      const ips = [];
      for (let x = 1; x < 255; x++) ips.push(prefixes[n] + '.' + x);
      await sweep(ips, 48, 900);
      addPrefixes([...found.values()].flatMap((h) => h.ips || []));
    }
    return [...found.values()];
  }

  window.mullion = { connect, disconnect, stats: snapshot, onStatus: (fn) => state.listeners.push(fn), decodeStrip, unwrapMs, deviceId, deviceName, setDeviceName,
    scan, hello, recentHosts, rememberHost, isLastUsed, portFor, parseTarget,
    // UI work without a host: feed a fake stream and status through the real paths.
    _simulate(status, videoEl, hostName) {
      if (videoEl && !videoEl.srcObject) {
        const c = document.createElement('canvas'); c.width = 1920; c.height = 1080; const g = c.getContext('2d');
        let t = 0; (function paint() { t += 0.02; g.fillStyle = '#1b2430'; g.fillRect(0, 0, 1920, 1080); g.fillStyle = '#5B93FF'; g.fillRect(760 + Math.sin(t) * 500, 400, 400, 280); g.fillStyle = '#fff'; g.font = '600 64px system-ui'; g.fillText('Extended desktop', 120, 160); requestAnimationFrame(paint); })();
        videoEl.srcObject = c.captureStream(60); videoEl.play().catch(() => {});
        state.video = videoEl;
      }
      state.hostName = hostName || 'DAISY-PC'; state.hostIp = '127.0.0.1';
      setStatus(status);
    } };
})();
