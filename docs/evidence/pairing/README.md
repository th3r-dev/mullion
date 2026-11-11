# Pairing evidence (PR #21)

Captured on 2026-11-12 against a dev host built from this branch
(`mullion-host --port 47200 --web-port 0 --no-extend`), with the receiver page in
Chrome on the same Mac. The Mac's host name is shown as `[Mac name]` and the
stream itself is not shown: it is this Mac's desktop. The stream's numbers are
in `browser-checks.json`.

| File | What it shows |
|---|---|
| `1-start.png` | The receiver before connecting. |
| `2-code-prompt.png` | After Connect: the page asks for the code the Mac shows. |
| `3-wrong-code.png` | A wrong code: "That code is not the one on the Mac. 2 tries left." |
| `host-log.txt` | Host side: pairing requested, the code it showed, the wrong code, paired, sealed offer answered, ICE completed, capture started; then a reconnect with no code. |
| `browser-checks.json` | From the page: streaming after pairing (1920x1080), reconnect without a code, signed `/screens` 200, unsigned `/screens` and `/devices` 401, a plain unsealed offer 401. |
| `cargo-test.txt` | The pairing tests and the totals (68 passed). |
| `interop-vectors.json` | `node tools/pair-vectors.mjs <msg_b>`: pair.js's SPAKE2 message, key and confirmation for a fixed exchange; `pairing::tests::js_and_rust_agree` checks the same values against the Rust `spake2` crate. The sealed offer uses a fresh nonce on every run. |
| `store-permissions.txt` | `paired.json` is readable by this user only. |

The test pairings were removed from `paired.json` afterwards.
