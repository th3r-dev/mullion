#!/usr/bin/env python3
"""Tiny Chrome DevTools client (no deps) for the loopback test.

  cdp.py eval  <js>            evaluate JS in the first mullion tab, print JSON result
  cdp.py open  <url>           navigate the first tab (or open one) to url
  cdp.py shot  <file.png>      screenshot the mullion tab
Port: $CDP_PORT or 9333.
"""
import base64, json, os, socket, struct, sys, urllib.parse, urllib.request

PORT = int(os.environ.get("CDP_PORT", "9333"))


def tabs():
    return json.load(urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json"))


def pick():
    for t in tabs():
        if t.get("type") == "page" and ("mullion" in t["url"] or "receiver" in t["url"] or "47100" in t["url"]):
            return t
    for t in tabs():
        if t.get("type") == "page" and not t["url"].startswith("chrome"):
            return t
    return None


class WS:
    def __init__(self, url):
        u = urllib.parse.urlparse(url)
        self.s = socket.create_connection((u.hostname, u.port))
        key = base64.b64encode(os.urandom(16)).decode()
        self.s.send(f"GET {u.path} HTTP/1.1\r\nHost: {u.hostname}:{u.port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n".encode())
        r = b""
        while b"\r\n\r\n" not in r:
            r += self.s.recv(4096)
        self.n = 0

    def send(self, method, params=None):
        self.n += 1
        d = json.dumps({"id": self.n, "method": method, "params": params or {}}).encode()
        h = bytes([0x81]); l = len(d)
        if l < 126: h += bytes([0x80 | l])
        elif l < 65536: h += bytes([0x80 | 126]) + struct.pack(">H", l)
        else: h += bytes([0x80 | 127]) + struct.pack(">Q", l)
        m = os.urandom(4)
        self.s.send(h + m + bytes(b ^ m[i % 4] for i, b in enumerate(d)))
        want = self.n
        while True:
            r = self.recv()
            if r.get("id") == want:
                return r

    def rd(self, n):
        b = b""
        while len(b) < n:
            c = self.s.recv(n - len(b))
            if not c: raise EOFError
            b += c
        return b

    def recv(self):
        h = self.rd(2); l = h[1] & 0x7F
        if l == 126: l = struct.unpack(">H", self.rd(2))[0]
        elif l == 127: l = struct.unpack(">Q", self.rd(8))[0]
        return json.loads(self.rd(l))


def main():
    cmd, arg = sys.argv[1], sys.argv[2]
    t = pick()
    if cmd == "open":
        if t is None:
            urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/new?{urllib.parse.quote(arg, safe='')}", data=b"")
            return
        ws = WS(t["webSocketDebuggerUrl"]); ws.send("Page.navigate", {"url": arg}); return
    if t is None:
        sys.exit("no tab")
    ws = WS(t["webSocketDebuggerUrl"])
    if cmd == "eval":
        r = ws.send("Runtime.evaluate", {"expression": arg, "returnByValue": True, "awaitPromise": True})
        res = r.get("result", {}).get("result", {})
        print(json.dumps(res.get("value", res), indent=None))
    elif cmd == "shot":
        r = ws.send("Page.captureScreenshot", {"format": "png"})
        open(arg, "wb").write(base64.b64decode(r["result"]["data"]))
        print(arg)


if __name__ == "__main__":
    main()
