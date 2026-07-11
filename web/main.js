// Browser driver for the limitbook wasm engine: loads the committed fixture,
// feeds it through limitbook-core (via the wasm-bindgen glue) in per-frame
// batches, and renders a live order-book ladder on a canvas.
//
// Correctness anchor: on completion the final AAPL and TSLA quotes are
// compared against the values `limitbook replay` prints for this fixture.

import init, { Engine } from "./pkg/limitbook_wasm.js";

const FIXTURE_URL = "../tests/fixtures/itch50_20191230.itch.gz";
const BATCH = 2000; // messages fed per animation frame (pacing, not a limit)
const DEPTH = 12; // price levels rendered per side
const VERIFY_EVERY = 1000; // deep-consistency cadence; mirrors the CLI default

// What `limitbook replay` reports at the end of this fixture (prices in
// Price(4) ticks). The verdict banner compares the browser against these.
const EXPECTED = {
  messages: 94385,
  quotes: { AAPL: [2894100, 2895000], TSLA: [4310000, 4315000] },
};

const $ = (id) => document.getElementById(id);
const canvas = $("ladder");
const ctx = canvas.getContext("2d");
ctx.scale(2, 2); // canvas is 2x its CSS size for crisp text
const W = canvas.width / 2;
const H = canvas.height / 2;

const CSS = getComputedStyle(document.documentElement);
const COLOR = {
  panel: CSS.getPropertyValue("--panel").trim(),
  ink: CSS.getPropertyValue("--ink").trim(),
  muted: CSS.getPropertyValue("--ink-muted").trim(),
  bid: CSS.getPropertyValue("--bid").trim(),
  ask: CSS.getPropertyValue("--ask").trim(),
};

function fmtPrice(ticks) {
  const whole = Math.floor(ticks / 10000);
  const frac = ticks % 10000;
  return `${whole}.${String(frac).padStart(4, "0")}`;
}

function fmtCount(n) {
  if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
  if (n >= 1e4) return (n / 1e3).toFixed(1) + "k";
  return String(Math.round(n));
}

async function loadFixture() {
  const resp = await fetch(FIXTURE_URL);
  if (!resp.ok) throw new Error(`fetch fixture: HTTP ${resp.status}`);
  let bytes = new Uint8Array(await resp.arrayBuffer());
  // Gunzip unless a Content-Encoding-aware server already inflated it.
  if (bytes[0] === 0x1f && bytes[1] === 0x8b) {
    const stream = new Blob([bytes])
      .stream()
      .pipeThrough(new DecompressionStream("gzip"));
    bytes = new Uint8Array(await new Response(stream).arrayBuffer());
  }
  return bytes;
}

// --- ladder rendering ------------------------------------------------------

function drawLadder(snap, symbol) {
  ctx.clearRect(0, 0, W, H);
  const nBid = snap[0];
  const nAsk = snap[1];
  const bidBase = 2;
  const askBase = 2 + 3 * nBid;

  const top = 46;
  const rowH = (H - top - 10) / DEPTH;
  const mid = W / 2;
  const barMax = mid - 130; // room for price+size text on each row

  let maxShares = 1;
  for (let i = 0; i < nBid; i++)
    maxShares = Math.max(maxShares, snap[bidBase + 3 * i + 1]);
  for (let i = 0; i < nAsk; i++)
    maxShares = Math.max(maxShares, snap[askBase + 3 * i + 1]);

  ctx.font = "12px ui-monospace, Menlo, Consolas, monospace";
  ctx.textBaseline = "middle";

  // Side headers + spread readout.
  ctx.fillStyle = COLOR.muted;
  ctx.textAlign = "left";
  ctx.fillText(`BIDS (${nBid} lvls)`, 14, 18);
  ctx.textAlign = "right";
  ctx.fillText(`ASKS (${nAsk} lvls)`, W - 14, 18);
  ctx.textAlign = "center";
  ctx.font = "600 14px ui-monospace, Menlo, Consolas, monospace";
  ctx.fillStyle = COLOR.ink;
  if (nBid > 0 && nAsk > 0) {
    const bb = snap[bidBase];
    const ba = snap[askBase];
    ctx.fillText(
      `${symbol}  ${fmtPrice(bb)} / ${fmtPrice(ba)}  Δ ${fmtPrice(ba - bb)}`,
      mid,
      18,
    );
  } else {
    ctx.fillText(`${symbol}  (book empty)`, mid, 18);
  }
  ctx.font = "12px ui-monospace, Menlo, Consolas, monospace";

  for (let row = 0; row < DEPTH; row++) {
    const y = top + row * rowH + rowH / 2;

    if (row < nBid) {
      const price = snap[bidBase + 3 * row];
      const shares = snap[bidBase + 3 * row + 1];
      const orders = snap[bidBase + 3 * row + 2];
      const w = Math.max(2, (shares / maxShares) * barMax);
      ctx.globalAlpha = 0.32;
      ctx.fillStyle = COLOR.bid;
      ctx.fillRect(mid - 68 - w, y - rowH / 2 + 2, w, rowH - 4);
      ctx.globalAlpha = 1;
      ctx.fillRect(mid - 68 - w, y - rowH / 2 + 2, 3, rowH - 4);
      ctx.fillStyle = COLOR.ink;
      ctx.textAlign = "right";
      ctx.fillText(fmtPrice(price), mid - 8, y);
      ctx.fillStyle = COLOR.muted;
      ctx.fillText(`${shares} ×${orders}`, mid - 74 - w, y);
    }

    if (row < nAsk) {
      const price = snap[askBase + 3 * row];
      const shares = snap[askBase + 3 * row + 1];
      const orders = snap[askBase + 3 * row + 2];
      const w = Math.max(2, (shares / maxShares) * barMax);
      ctx.globalAlpha = 0.32;
      ctx.fillStyle = COLOR.ask;
      ctx.fillRect(mid + 68, y - rowH / 2 + 2, w, rowH - 4);
      ctx.globalAlpha = 1;
      ctx.fillRect(mid + 65 + w, y - rowH / 2 + 2, 3, rowH - 4);
      ctx.fillStyle = COLOR.ink;
      ctx.textAlign = "left";
      ctx.fillText(fmtPrice(price), mid + 8, y);
      ctx.fillStyle = COLOR.muted;
      ctx.fillText(`${shares} ×${orders}`, mid + 74 + w, y);
    }
  }
}

// --- main ------------------------------------------------------------------

async function main() {
  await init();
  const bytes = await loadFixture();
  const engine = new Engine(bytes, VERIFY_EVERY);
  $("status").textContent =
    `Fixture loaded (${(bytes.length / 1048576).toFixed(1)} MB raw). Ready.`;

  // Symbol picker needs the Stock Directory, which sits at the head of the
  // stream — feed a small prefix to learn it (13 "R" messages, all before
  // any order flow in this fixture).
  const t0 = performance.now();
  const warmup = engine.step(50);
  let stepMs = performance.now() - t0;
  let stepMsgs = warmup;

  const select = $("symbol");
  for (const locate of engine.symbol_locates()) {
    const opt = document.createElement("option");
    opt.value = locate;
    opt.textContent = engine.symbol_name(locate);
    select.appendChild(opt);
  }
  const tsla = engine.locate("TSLA");
  if (tsla >= 0) select.value = tsla;
  select.disabled = false;

  let playing = false;
  let playMs = 0;
  let lastFrame = 0;

  const button = $("playpause");
  button.disabled = false;
  button.onclick = () => {
    playing = !playing;
    button.textContent = playing ? "Pause" : "Play";
    lastFrame = performance.now();
  };

  function refreshStats() {
    $("s-msgs").textContent =
      `${engine.messages().toLocaleString()} / ${EXPECTED.messages.toLocaleString()}`;
    $("s-engine-rate").textContent =
      stepMs > 0 ? fmtCount(stepMsgs / (stepMs / 1000)) : "–";
    $("s-replay-rate").textContent =
      playMs > 0 ? fmtCount(engine.messages() / (playMs / 1000)) : "–";
    $("s-live").textContent = engine.live_orders().toLocaleString();
    $("s-viol").textContent = engine.violations().toLocaleString();

    const snap = engine.snapshot(Number(select.value), DEPTH);
    if (snap[0] > 0 && snap[1] > 0) {
      const bb = snap[2];
      const ba = snap[2 + 3 * snap[0]];
      $("s-quote").textContent = `${fmtPrice(bb)} / ${fmtPrice(ba)}`;
      $("s-spread").textContent = fmtPrice(ba - bb);
    } else {
      $("s-quote").textContent = "–";
      $("s-spread").textContent = "–";
    }
    drawLadder(snap, engine.symbol_name(Number(select.value)));
  }

  function verdict() {
    const checks = [];
    const msgs = engine.messages();
    checks.push([`messages ${msgs} = ${EXPECTED.messages}`, msgs === EXPECTED.messages]);
    checks.push([`violations ${engine.violations()} = 0`, engine.violations() === 0]);
    for (const [sym, [bid, ask]] of Object.entries(EXPECTED.quotes)) {
      const snap = engine.snapshot(engine.locate(sym), 1);
      const gotBid = snap[0] > 0 ? snap[2] : NaN;
      const gotAsk = snap[1] > 0 ? snap[2 + 3 * snap[0]] : NaN;
      checks.push([
        `${sym} ${fmtPrice(gotBid)}/${fmtPrice(gotAsk)} = CLI ${fmtPrice(bid)}/${fmtPrice(ask)}`,
        gotBid === bid && gotAsk === ask,
      ]);
    }
    const pass = checks.every(([, ok]) => ok);
    const el = $("verdict");
    el.style.display = "block";
    el.className = pass ? "pass" : "fail";
    el.innerHTML =
      `<strong>${pass ? "PASS" : "FAIL"}</strong> — browser vs CLI replay<br>` +
      checks
        .map(([txt, ok]) => `${ok ? "✓" : "✗"} ${txt}`)
        .join("<br>");
    // Machine-readable hook for the headless end-to-end check.
    window.__limitbook_verdict = { pass, checks };
  }

  function frame(now) {
    if (playing && !engine.done()) {
      playMs += now - lastFrame;
      const t = performance.now();
      const fed = engine.step(BATCH);
      stepMs += performance.now() - t;
      stepMsgs += fed;
      refreshStats();
      if (engine.done()) {
        playing = false;
        button.textContent = "Play";
        button.disabled = true;
        $("status").textContent = "Replay complete.";
        verdict();
      }
    }
    lastFrame = now;
    requestAnimationFrame(frame);
  }

  select.onchange = refreshStats;
  refreshStats();
  requestAnimationFrame(frame);
  window.__limitbook_ready = true;
}

main().catch((e) => {
  $("status").textContent = `Error: ${e.message ?? e}`;
  console.error(e);
});
