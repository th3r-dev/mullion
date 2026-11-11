//! The probe page (TEST_PLAN "How latency is measured", SRS A-13). Served by
//! the host at `/probe`; opened full screen on the captured display. Every
//! animation frame it paints the strip from `mullion_proto::probe` with
//! `Date.now()`, plus a moving block so the encoder always has motion.

pub const PROBE_HTML: &str = r##"<!doctype html>
<meta charset="utf-8">
<title>Mullion probe</title>
<link rel="icon" href="/favicon.ico">
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
/// before the Dioxus app is installed. Same assets as the app.
/// `port` is the host's main (signaling) port; the page may have been loaded
/// from the web port (80), so it is told where offers go.
pub fn receiver_html(host_ip: &str, port: u16) -> String {
    let noble = include_str!("../../app/assets/vendor/noble.js");
    let pair = include_str!("../../app/assets/pair.js");
    let js = include_str!("../../app/assets/receiver.js");
    let ui_js = include_str!("../../app/assets/ui.js");
    let layout_js = include_str!("../../app/assets/layout.js");
    let css = include_str!("../../app/assets/ui.css");
    let version = mullion_proto::version::VERSION;
    // From the request's Host header: keep only what an address can hold, so
    // nothing else reaches the attribute.
    let host_ip: String = host_ip.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')).take(253).collect();
    let body = include_str!("../../app/assets/ui.html").replace(r#"<input id="ip""#, &format!(r#"<input id="ip" value="{host_ip}""#));
    format!(
        "<!doctype html>\n<meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1,viewport-fit=cover\">\n<meta name=\"color-scheme\" content=\"light dark\">\n<meta name=\"theme-color\" content=\"#111418\">\n<title>Mullion</title>\n{ICON_LINKS}\n<style>{css}</style>\n{body}\n<script>window.MULLION_HOST_PORT={port};window.MULLION_VERSION=\"{version}\";</script>\n<script>{noble}</script>\n<script>{pair}</script>\n<script>{js}</script>\n<script>{ui_js}</script>\n<script>{layout_js}</script>\n"
    )
}

/// `<head>` links for the icon set served under `/icons/`.
pub const ICON_LINKS: &str = r#"<link rel="icon" href="/favicon.ico" sizes="16x16 32x32 48x48">
<link rel="icon" type="image/svg+xml" href="/icons/favicon.svg">
<link rel="apple-touch-icon" href="/icons/apple-touch-icon.png">
<link rel="manifest" href="/icons/manifest.webmanifest">
<meta name="apple-mobile-web-app-capable" content="yes">
<meta name="mobile-web-app-capable" content="yes">
<meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">
<meta name="apple-mobile-web-app-title" content="Mullion">"#;

/// Static icon files, embedded in the binary.
pub fn icon_file(path: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match path {
        "/favicon.ico" | "/icons/favicon.ico" => (include_bytes!("../../app/assets/icons/favicon.ico"), "image/x-icon"),
        "/icons/favicon.svg" => (include_bytes!("../../app/assets/icons/favicon.svg"), "image/svg+xml"),
        "/icons/apple-touch-icon.png" => (include_bytes!("../../app/assets/icons/apple-touch-icon.png"), "image/png"),
        "/icons/icon-192.png" => (include_bytes!("../../app/assets/icons/icon-192.png"), "image/png"),
        "/icons/icon-512.png" => (include_bytes!("../../app/assets/icons/icon-512.png"), "image/png"),
        "/icons/icon-maskable-512.png" => (include_bytes!("../../app/assets/icons/icon-maskable-512.png"), "image/png"),
        "/icons/manifest.webmanifest" => (include_bytes!("../../app/assets/icons/manifest.webmanifest"), "application/manifest+json"),
        _ => return None,
    })
}
