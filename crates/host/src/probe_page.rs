//! The probe page (TEST_PLAN "How latency is measured", SRS A-13). Served by
//! the host at `/probe`; opened full screen on the captured display. Every
//! animation frame it paints the strip from `mullion_proto::probe` with
//! `Date.now()`, plus a moving block so the encoder always has motion.

pub const PROBE_HTML: &str = r##"<!doctype html>
<meta charset="utf-8">
<title>Mullion probe</title>
<style>
  html,body{margin:0;height:100%;background:#808080;overflow:hidden;font:14px system-ui}
  canvas{display:block;width:100vw;height:100vh}
  #hud{position:fixed;left:12px;bottom:12px;color:#fff;background:rgba(17,20,24,.86);padding:6px 10px;border-radius:6px}
</style>
<canvas id="c"></canvas>
<div id="hud">Mullion probe. Press F for full screen. Strip: 38 blocks, host ms.</div>
<script>
const BLOCKS=38, SYNC=2, DATA=32;
const c=document.getElementById('c'), g=c.getContext('2d',{alpha:false});
function fit(){c.width=innerWidth*devicePixelRatio|0;c.height=innerHeight*devicePixelRatio|0}
addEventListener('resize',fit);fit();
addEventListener('keydown',e=>{if(e.key==='f'||e.key==='F')document.documentElement.requestFullscreen()});
function encode(ms){
  const v=ms>>>0; const out=new Uint8Array(BLOCKS); out[0]=1; out[1]=0;
  for(let i=0;i<DATA;i++) out[SYNC+i]=(v>>>(31-i))&1;
  const pop=x=>{x=x-((x>>>1)&0x55555555);x=(x&0x33333333)+((x>>>2)&0x33333333);return(((x+(x>>>4))&0x0F0F0F0F)*0x01010101)>>>24};
  out[SYNC+DATA]=pop(v&0xFFFF)&1; out[SYNC+DATA+1]=pop(v>>>16)&1; out[BLOCKS-2]=0; out[BLOCKS-1]=1; return out;
}
let t0=performance.now(), frames=0;
function paint(){
  const W=c.width,H=c.height;
  // Strip across the top 12% of the screen: wide blocks survive scaling and H.264.
  const stripH=Math.max(24,H*0.12|0), bw=W/BLOCKS;
  const bits=encode(Date.now());
  for(let i=0;i<BLOCKS;i++){g.fillStyle=bits[i]?'#fff':'#000';g.fillRect(i*bw|0,0,Math.ceil(bw)+1,stripH)}
  // Motion below the strip so the encoder never idles (NFR-2, NFR-4).
  const t=(performance.now()-t0)/1000;
  g.fillStyle='#808080'; g.fillRect(0,stripH,W,H-stripH);
  for(let k=0;k<6;k++){
    const x=(W*0.5)+Math.cos(t*2+k)*W*0.35, y=stripH+(H-stripH)*0.5+Math.sin(t*3+k*1.3)*(H-stripH)*0.35;
    g.fillStyle=['#1F5FD6','#D9532B','#138A77','#8A4CCB','#ECEDEE','#111418'][k];
    g.fillRect(x-80,y-80,160,160);
  }
  g.fillStyle='#fff';g.font=`${H*0.06|0}px system-ui`;g.fillText(new Date().toISOString().slice(11,23),W*0.04,H*0.95);
  frames++;
  requestAnimationFrame(paint);
}
requestAnimationFrame(paint);
</script>
"##;

/// The browser receiver, served at `/receiver` so a tablet can test with Chrome
/// before the Dioxus app is installed. Same JS as the app uses.
pub fn receiver_html(host_ip: &str) -> String {
    let js = include_str!("../../app/assets/receiver.js");
    format!(r##"<!doctype html>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Mullion receiver</title>
<style>
  html,body{{margin:0;background:#0F1113;color:#ECEDEE;font:15px system-ui;height:100%}}
  #bar{{display:flex;gap:8px;align-items:center;padding:8px 12px;background:#111418}}
  input,select{{font:inherit;padding:6px 8px;border-radius:6px;border:1px solid #2A2E33;background:#0F1113;color:#ECEDEE}} input{{width:160px}}
  button{{font:inherit;padding:6px 12px;border-radius:6px;border:0;background:#5B93FF;color:#0F1113;font-weight:600}}
  video{{display:block;width:100vw;height:calc(100vh - 44px);background:#000;object-fit:contain}}
  #hud{{position:fixed;right:12px;bottom:12px;background:rgba(17,20,24,.86);padding:6px 10px;border-radius:6px;font-size:12px;font-variant-numeric:tabular-nums;white-space:pre}}
  .fs #bar{{display:none}} .fs video{{height:100vh}}
</style>
<div id="bar"><input id="ip" value="{host_ip}"><input id="name" placeholder="This device's name" style="width:140px">
<select id="mode"><option value="mirror">Mirror</option><option value="extend">Extend</option></select>
<button id="go">Connect</button><button id="full">Full screen</button><span id="st">idle</span></div>
<video id="v" muted autoplay playsinline></video>
<div id="hud"></div>
<script>{js}</script>
<script>
  const ip=document.getElementById('ip'), st=document.getElementById('st'), hud=document.getElementById('hud');
  const modeEl=document.getElementById('mode'); modeEl.value=localStorage.getItem('mullion.mode')||'mirror';
  const nameEl=document.getElementById('name'); nameEl.value=mullion.deviceName();
  document.getElementById('go').onclick=()=>{{localStorage.setItem('mullion.mode',modeEl.value);mullion.setDeviceName(nameEl.value);mullion.connect(ip.value,document.getElementById('v'),{{mode:modeEl.value,name:nameEl.value}});}};
  modeEl.onchange=()=>{{ if(mullion.stats().status!=='idle') document.getElementById('go').click(); }};
  document.getElementById('full').onclick=()=>{{document.body.classList.add('fs');document.documentElement.requestFullscreen&&document.documentElement.requestFullscreen()}};
  mullion.onStatus(s=>{{st.textContent=s.status+(s.error?' — '+s.error:'')+(s.hostName?' · '+s.hostName:'');}});
  setInterval(()=>{{const s=mullion.stats();const f=x=>x==null?'–':x.toFixed(1);hud.textContent=
`${{s.mode}} ${{s.status}} ${{s.videoWidth}}x${{s.videoHeight}}  offset ${{f(s.offsetMs)}} rtt ${{f(s.rttMs)}}
presented ${{s.presented}} decoded ${{s.decoded}} fail ${{s.decodeFail}} dropped ${{s.dropped}}
g2g last ${{f(s.g2gLast)}} p50 ${{f(s.g2gP50)}} p95 ${{f(s.g2gP95)}} ms
jbt ${{s.caps?(s.caps.jitter_buffer_target_supported?'yes '+s.caps.jitter_buffer_target_ms:'no'):'–'}}`;}},250);
</script>
"##)
}
