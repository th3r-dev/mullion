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
    listeners: [],
  };

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
      status: state.status, error: state.error, hostName: state.hostName, hostIp: state.hostIp,
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
    if (!v || state.status !== 'streaming') return;
    state.presented++;
    if (meta && meta.presentedFrames && state.lastFrameTs) {
      // presentedFrames counts frames the UA presented; gaps are drops.
      const gap = meta.presentedFrames - state.lastFrameTs - 1;
      if (gap > 0) state.dropped += gap;
    }
    if (meta && meta.presentedFrames) state.lastFrameTs = meta.presentedFrames;
    const low = readStrip(v);
    if (low === null) { state.decodeFail++; }
    else if (state.offset !== null) {
      state.decoded++;
      const presentedMs = performance.timeOrigin + (meta && meta.expectedDisplayTime ? meta.expectedDisplayTime : now);
      const painted = unwrapMs(low, presentedMs + state.offset);
      const g2g = (presentedMs + state.offset) - painted;
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
      state.offset = best.offset; state.rtt = best.rtt;
      setStatus(state.status);
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
    const port = opts.port || 47100;
    disconnect();
    state.hostIp = hostIp; state.video = videoEl; state.error = null;
    state.g2g = []; state.samples = []; state.offset = null; state.presented = 0; state.decoded = 0; state.decodeFail = 0; state.dropped = 0; state.lastFrameTs = 0;
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
        body: JSON.stringify({ type: 'offer', sdp: pc.localDescription.sdp }), signal: ctrl.signal,
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
    await pc.setRemoteDescription({ type: 'answer', sdp: answer.sdp });
    setStatus('connected');
  }

  function disconnect() {
    if (state.dc) { try { state.dc.close(); } catch (e) { /* */ } }
    if (state.pc) { try { state.pc.close(); } catch (e) { /* */ } }
    if (state.video) { try { state.video.srcObject = null; } catch (e) { /* */ } }
    state.pc = null; state.dc = null;
    if (state.status !== 'error') setStatus('idle');
  }

  window.mullion = { connect, disconnect, stats: snapshot, onStatus: (fn) => state.listeners.push(fn), decodeStrip, unwrapMs };
})();
