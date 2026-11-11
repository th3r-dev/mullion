#!/usr/bin/env python3
"""Summarize a mullion-host timing log (FR-9): host stages, bitrate, and
glass-to-glass per receiver session.  usage: analyze-log.py logs/host-*.jsonl"""
import collections, json, sys


def pct(v, q):
    v = sorted(v)
    return v[min(len(v) - 1, max(0, int(q * len(v) + 0.999) - 1))] if v else float("nan")


def main(path):
    fr, pres, caps, pli, ev = [], [], [], [], []
    for line in open(path):
        r = json.loads(line)
        {"frame": fr, "presented": pres, "capabilities": caps, "pli": pli, "event": ev}.get(r["rec"], []).append(r)
    print(f"{path}: {len(fr)} frames, {len(pres)} presented reports, {len(caps)} receivers, {len(pli)} PLIs ({sum(p['granted'] for p in pli)} granted)")
    for e in ev:
        print("  event:", e["what"])
    if fr:
        secs = (fr[-1]["capture_ms"] - fr[0]["capture_ms"]) / 1000 or 1e-9
        by = sum(f["bytes"] for f in fr)
        enc = [f["encode_done_ms"] - f["capture_ms"] for f in fr]
        snd = [f["send_ms"] - f["encode_done_ms"] for f in fr]
        wb = collections.Counter()
        for f in fr:
            wb[int(f["capture_ms"] / 1000)] += f["bytes"]
        print(f"  host: {secs:.0f} s, {(len(fr)-1)/secs:.1f} fps, avg {by*8/secs/1e6:.2f} Mbps, peak 1 s window {max(wb.values())*8/1e6:.1f} Mbps, IDR {sum(f['idr'] for f in fr)}")
        print(f"  encode stage p50 {pct(enc,.5):.1f} p95 {pct(enc,.95):.1f} ms; encoded->sent p95 {pct(snd,.95):.2f} ms")
    ua = {}
    for c in caps:
        sid = c.get("session", 0)
        ua[sid] = c
        print(f"  receiver #{sid} {c.get('peer','')}: jitterBufferTarget {'yes' if c['jitter_buffer_target_supported'] else 'NO'} {c['jitter_buffer_target_ms']}  {c['user_agent'][:70]}")
    by_sess = collections.defaultdict(list)
    for x in pres:
        g = x["glass_to_glass_ms"]
        if -200 < g < 5000:
            by_sess[x.get("session", 0)].append(g)
    for sid, g in sorted(by_sess.items()):
        tag = f"#{sid}" if sid else "(untagged)"
        print(f"  g2g {tag}: n={len(g)} p50 {pct(g,.5):.1f} p95 {pct(g,.95):.1f} max {max(g):.1f} ms  {'PASS' if pct(g,.95) < 50 else 'over 50 ms'}")


if __name__ == "__main__":
    for p in sys.argv[1:]:
        main(p)
