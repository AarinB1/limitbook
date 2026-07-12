// Browser driver for the limitbook wasm engine.
//
// The engine (limitbook-core, the same crate the CLI runs) owns all market
// logic; this file is transport + presentation: it downloads the committed
// mid-day fixture, paces it through the wasm boundary — either in feed time
// (1×..1000×, driven by the ITCH timestamps themselves) or as fast as the
// engine goes (MAX) — and renders the book. Every number on screen is read
// back out of the engine; the throughput counter only counts wall time spent
// inside `engine.step` calls, so it is a measurement, not an estimate.
//
// Correctness anchor: on completion the final state is compared against what
// `limitbook replay` prints for this fixture (see EXPECTED); CI asserts the
// same equality in crates/limitbook-wasm/tests/fixture_via_engine.rs.

import init, { Engine } from "./pkg/limitbook_wasm.js";

// Served next to index.html on GitHub Pages; out of tests/fixtures/ when the
// repo root is served locally (see web/build.sh).
const FIXTURE_URLS = [
  "./itch50_20191230_midday.itch.gz",
  "../tests/fixtures/itch50_20191230_midday.itch.gz",
];

// Ground truth from `limitbook replay` on this fixture (prices in Price(4)
// ticks). tools/regen_midday_fixture.sh documents the slice.
const EXPECTED = {
  messages: 157824,
  liveOrders: 1411,
  quotes: {
    AAPL: [2910300, 2910500],
    SPY: [3217000, 3217100],
    TSLA: [4189400, 4190500],
  },
};
const SYMBOLS = ["AAPL", "TSLA", "SPY"];
const T0 = 12 * 3600 * 1e9; // slice window in feed time (ns since midnight)
const T1 = T0 + 20 * 60 * 1e9;

const DEPTH = 14; // price levels rendered per side
const VERIFY_EVERY = 1000; // deep-consistency cadence; mirrors the CLI default
const FRAME_BUDGET_MS = 9; // max time spent feeding per animation frame
const SPEEDS = [
  { label: "1×", mult: 1 },
  { label: "10×", mult: 10 },
  { label: "100×", mult: 100 },
  { label: "1000×", mult: 1000 },
  { label: "MAX", mult: Infinity },
];
const TAPE_KEEP = 200; // prints retained per symbol
const SPARK_EVERY = 100; // sample mid price at most every N messages

const $ = (id) => document.getElementById(id);
const MONO = getComputedStyle(document.documentElement)
  .getPropertyValue("--mono")
  .trim();
const COLOR = {};
for (const key of ["surface-2", "edge", "ink", "ink-2", "ink-3", "bid", "ask",
  "bid-ink", "ask-ink", "accent", "flash"]) {
  COLOR[key] = getComputedStyle(document.documentElement)
    .getPropertyValue(`--${key}`)
    .trim();
}

// --- formatting --------------------------------------------------------------

function fmtPrice(ticks) {
  if (!Number.isFinite(ticks)) return "–";
  const whole = Math.floor(ticks / 10000);
  const frac = ticks % 10000;
  // Displayed quotes are whole cents; only hidden-liquidity prints use the
  // sub-penny digits.
  return frac % 100 === 0
    ? `${whole}.${String(frac / 100).padStart(2, "0")}`
    : `${whole}.${String(frac).padStart(4, "0")}`;
}

function fmtCount(n) {
  if (!Number.isFinite(n)) return "–";
  if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "k";
  return String(Math.round(n));
}

function fmtClock(ns, ms = true) {
  if (!ns) return "–";
  const s = Math.floor(ns / 1e9);
  const base = [s / 3600, (s / 60) % 60, s % 60]
    .map((v) => String(Math.floor(v)).padStart(2, "0"))
    .join(":");
  if (!ms) return base;
  return `${base}.${String(Math.floor((ns % 1e9) / 1e6)).padStart(3, "0")}`;
}

// --- fixture loading ----------------------------------------------------------

async function loadFixture() {
  let resp = null;
  for (const url of FIXTURE_URLS) {
    resp = await fetch(url).catch(() => null);
    if (resp?.ok) break;
  }
  if (!resp?.ok) throw new Error("fixture not found");
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

// --- DPR-aware canvases ---------------------------------------------------------

function sizeCanvas(canvas) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
    canvas.width = w * dpr;
    canvas.height = h * dpr;
  }
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return [ctx, w, h];
}

// --- main -----------------------------------------------------------------------

async function main() {
  await init();
  $("status").textContent = "Downloading capture…";
  const bytes = await loadFixture();

  let engine = null;
  // Wall time inside engine.step (ms) and messages fed — the throughput
  // measurement. Survives seeks/rebuilds: it is "everything this tab asked
  // the engine to do".
  let stepMs = 0;
  let stepMsgs = 0;

  function feed(max) {
    const t = performance.now();
    const fed = engine.step(max);
    stepMs += performance.now() - t;
    stepMsgs += fed;
    return fed;
  }

  // Feeds the administrative preamble (directory, trading actions — all
  // timestamped before the window) so books/locates exist before first paint.
  function prime() {
    while (!engine.done() && engine.clock_ns() < T0 && engine.messages() < 5000) {
      feed(25);
    }
  }

  const tape = new Map(); // locate -> [{t, price, shares, aggr}...]
  const spark = new Map(); // locate -> [{m, t, mid}...]
  let sparkLastSample = -Infinity;
  let tapeDirty = true;

  function freshEngine(withPrime = true) {
    engine?.free();
    engine = new Engine(bytes, VERIFY_EVERY);
    for (const arr of tape.values()) arr.length = 0;
    for (const arr of spark.values()) arr.length = 0;
    sparkLastSample = -Infinity;
    tapeDirty = true;
    $("verdict").style.display = "none";
    delete window.__limitbook_verdict;
    if (withPrime) prime();
  }

  freshEngine();
  const locates = Object.fromEntries(SYMBOLS.map((s) => [s, engine.locate(s)]));
  for (const s of SYMBOLS) {
    tape.set(locates[s], []);
    spark.set(locates[s], []);
  }
  let selected = locates.AAPL;

  // --- transport state ----------------------------------------------------------

  let playing = false;
  let speedIdx = 0;
  let simClock = engine.clock_ns(); // feed-time cursor for paced replay
  let lastFrame = performance.now();
  let pace = 0; // EWMA of messages/second consumed at the current speed
  let pendingSeek = -1;
  let scrubbing = false;
  let finishedOnce = false;

  const playBtn = $("playpause");
  const scrubber = $("scrubber");

  function setPlaying(p) {
    if (p && engine.done()) pendingSeek = 0; // replay from the top
    playing = p;
    playBtn.textContent = playing ? "❚❚ Pause" : engine.done() ? "↻ Replay" : "▶ Play";
    simClock = engine.clock_ns();
    lastFrame = performance.now();
    pace = 0;
  }

  playBtn.disabled = false;
  playBtn.onclick = () => setPlaying(!playing);

  // Speed segmented control.
  const speedBox = $("speed");
  SPEEDS.forEach((s, i) => {
    const b = document.createElement("button");
    b.textContent = s.label;
    b.setAttribute("aria-pressed", String(i === speedIdx));
    b.onclick = () => {
      speedIdx = i;
      simClock = engine.clock_ns();
      pace = 0;
      [...speedBox.children].forEach((c, j) =>
        c.setAttribute("aria-pressed", String(j === i)),
      );
    };
    speedBox.appendChild(b);
  });

  // Symbol tabs.
  const tabs = $("tabs");
  for (const s of SYMBOLS) {
    const b = document.createElement("button");
    b.className = "tab";
    b.innerHTML = `<span class="sym">${s}</span><span class="mid">–</span>`;
    b.setAttribute("aria-pressed", String(locates[s] === selected));
    b.onclick = () => {
      selected = locates[s];
      tapeDirty = true;
      resetLadderState();
      [...tabs.children].forEach((c, j) =>
        c.setAttribute("aria-pressed", String(locates[SYMBOLS[j]] === selected)),
      );
    };
    tabs.appendChild(b);
  }

  // Scrubber: coalesce drag events into one seek per frame.
  scrubber.disabled = false;
  scrubber.addEventListener("input", () => {
    pendingSeek = Number(scrubber.value);
  });
  scrubber.addEventListener("pointerdown", () => (scrubbing = true));
  scrubber.addEventListener("pointerup", () => (scrubbing = false));

  function seekTo(target) {
    const rebuilt = target < engine.messages();
    if (rebuilt) {
      freshEngine(false);
      resetLadderState();
    }
    // Drop mid-price history past the new position, then fast-forward in
    // chunks, sampling as we go so the chart shows the path just skipped.
    for (const arr of spark.values()) {
      while (arr.length && arr[arr.length - 1].m > target) arr.pop();
    }
    while (engine.messages() < target && !engine.done()) {
      feed(Math.min(target - engine.messages(), 2000));
      sampleSpark();
    }
    drainTape();
    sparkLastSample = -Infinity; // force a sample at the new position
    sampleSpark();
    simClock = engine.clock_ns();
    lastFrame = performance.now();
    if (engine.done()) onDone();
    else {
      finishedOnce = false;
      $("status").textContent = READY_STATUS;
    }
    playBtn.textContent = playing ? "❚❚ Pause" : engine.done() ? "↻ Replay" : "▶ Play";
  }

  window.addEventListener("keydown", (e) => {
    if (e.target.tagName === "INPUT") return;
    if (e.code === "Space") {
      e.preventDefault();
      setPlaying(!playing);
    } else if (e.key >= "1" && e.key <= "5") {
      speedBox.children[Number(e.key) - 1].click();
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      const delta = (e.key === "ArrowLeft" ? -1 : 1) * Math.round(EXPECTED.messages / 20);
      pendingSeek = Math.max(0, Math.min(EXPECTED.messages, engine.messages() + delta));
    }
  });

  // --- tape + spark ---------------------------------------------------------------

  function drainTape() {
    const flat = engine.take_tape();
    for (let i = 0; i < flat.length; i += 5) {
      const arr = tape.get(flat[i]);
      if (!arr) continue;
      arr.push({ price: flat[i + 1], shares: flat[i + 2], aggr: flat[i + 3], t: flat[i + 4] });
      if (arr.length > TAPE_KEEP) arr.splice(0, arr.length - TAPE_KEEP);
      tapeDirty = true;
    }
  }

  function midOf(locate) {
    const snap = engine.snapshot(locate, 1);
    if (snap[0] === 0 || snap[1] === 0) return NaN;
    return (snap[2] + snap[2 + 3 * snap[0]]) / 2;
  }

  function sampleSpark() {
    if (engine.messages() - sparkLastSample < SPARK_EVERY) return;
    sparkLastSample = engine.messages();
    for (const s of SYMBOLS) {
      const mid = midOf(locates[s]);
      if (Number.isFinite(mid)) {
        spark.get(locates[s]).push({ m: engine.messages(), t: engine.clock_ns(), mid });
      }
    }
  }

  // --- rendering ---------------------------------------------------------------------

  const flashAt = new Map(); // "side:price" -> performance.now() of last change
  let prevShares = new Map();
  const barLen = new Map(); // "side:price" -> eased on-screen bar length (px)
  let smoothMax = 0; // eased share scale so the ladder doesn't re-scale in jumps
  // Honors prefers-reduced-motion (index.html zeroes --flash for it).
  const EASE = COLOR.flash === "transparent" ? 1 : 0.35;
  const SCALE_EASE = EASE === 1 ? 1 : EASE * 0.6;

  function resetLadderState() {
    flashAt.clear();
    prevShares = new Map();
    barLen.clear();
    smoothMax = 0;
  }

  function drawLadder() {
    const [ctx, W, H] = sizeCanvas($("ladder"));
    ctx.clearRect(0, 0, W, H);
    const snap = engine.snapshot(selected, DEPTH);
    const nBid = snap[0];
    const nAsk = snap[1];
    const bidBase = 2;
    const askBase = 2 + 3 * nBid;
    const now = performance.now();

    const top = 26;
    const rowH = (H - top - 8) / DEPTH;
    const mid = W / 2;
    const priceGap = 14; // price columns hug the center line
    const barInner = 96; // bars start here and grow outward
    const sizeCol = 64; // size figures live at the outer edge
    const barMax = mid - barInner - sizeCol - 18;

    ctx.textBaseline = "middle";
    ctx.font = `600 10px ${MONO}`;
    ctx.fillStyle = COLOR["ink-3"];
    ctx.textAlign = "left";
    ctx.fillText("SIZE", 14, 13);
    ctx.textAlign = "right";
    ctx.fillText("BID", mid - priceGap, 13);
    ctx.textAlign = "left";
    ctx.fillText("ASK", mid + priceGap, 13);
    ctx.textAlign = "right";
    ctx.fillText("SIZE", W - 14, 13);

    // Center rule.
    ctx.fillStyle = COLOR.edge;
    ctx.fillRect(mid - 0.5, top, 1, H - top - 8);

    let maxShares = 1;
    for (let i = 0; i < nBid; i++) maxShares = Math.max(maxShares, snap[bidBase + 3 * i + 1]);
    for (let i = 0; i < nAsk; i++) maxShares = Math.max(maxShares, snap[askBase + 3 * i + 1]);
    smoothMax = smoothMax > 0 ? smoothMax + (maxShares - smoothMax) * SCALE_EASE : maxShares;

    const nextShares = new Map();
    const side = (n, base, dir, barColor, inkColor) => {
      for (let row = 0; row < Math.min(n, DEPTH); row++) {
        const price = snap[base + 3 * row];
        const shares = snap[base + 3 * row + 1];
        const orders = snap[base + 3 * row + 2];
        const y = top + row * rowH + rowH / 2;
        const key = `${dir}:${price}`;
        nextShares.set(key, shares);
        const prev = prevShares.get(key);
        if (prev !== undefined && prev !== shares) flashAt.set(key, now);

        // Row flash on size change (fades over 280 ms).
        const f = flashAt.get(key);
        if (f !== undefined && now - f < 280 && COLOR.flash !== "transparent") {
          ctx.globalAlpha = 0.09 * (1 - (now - f) / 280);
          ctx.fillStyle = COLOR.flash;
          const x0 = dir < 0 ? 8 : mid + 4;
          ctx.fillRect(x0, y - rowH / 2 + 1, mid - 12, rowH - 2);
          ctx.globalAlpha = 1;
        }

        // Depth bar from the center outward, rounded outer end + solid cap.
        // The length eases toward its target so updates read as movement,
        // not repaints; new levels grow in from the center line.
        const target = Math.min(barMax, Math.max(2, (shares / smoothMax) * barMax));
        const shown = barLen.get(key);
        const len = shown === undefined ? 2 : shown + (target - shown) * EASE;
        barLen.set(key, len);
        const xInner = mid + dir * barInner;
        const xOuter = xInner + dir * len;
        const barY = y - rowH / 2 + 3;
        const barH = rowH - 6;
        ctx.fillStyle = barColor;
        ctx.globalAlpha = row === 0 ? 0.42 : 0.26;
        ctx.beginPath();
        if (ctx.roundRect) {
          ctx.roundRect(
            Math.min(xInner, xOuter),
            barY,
            Math.abs(xOuter - xInner),
            barH,
            dir < 0 ? [3, 0, 0, 3] : [0, 3, 3, 0],
          );
        } else {
          ctx.rect(Math.min(xInner, xOuter), barY, Math.abs(xOuter - xInner), barH);
        }
        ctx.fill();
        ctx.globalAlpha = 1;
        ctx.fillRect(dir < 0 ? xOuter : xOuter - 2, barY, 2, barH);

        // Price (center columns) and size (outer edge).
        ctx.font = `${row === 0 ? "700" : "400"} 12.5px ${MONO}`;
        ctx.fillStyle = row === 0 ? inkColor : COLOR.ink;
        ctx.textAlign = dir < 0 ? "right" : "left";
        ctx.fillText(fmtPrice(price), mid + dir * priceGap, y);
        ctx.font = `400 12px ${MONO}`;
        ctx.fillStyle = COLOR["ink-2"];
        ctx.textAlign = dir < 0 ? "left" : "right";
        ctx.fillText(shares.toLocaleString(), dir < 0 ? 14 : W - 14, y);
        // Order count, faint, riding the bar's inner end.
        ctx.font = `400 10px ${MONO}`;
        ctx.fillStyle = COLOR["ink-3"];
        ctx.textAlign = dir < 0 ? "right" : "left";
        ctx.fillText(`×${orders}`, xInner - dir * 4, y);
      }
    };
    side(nBid, bidBase, -1, COLOR.bid, COLOR["bid-ink"]);
    side(nAsk, askBase, +1, COLOR.ask, COLOR["ask-ink"]);
    prevShares = nextShares;
    for (const k of barLen.keys()) if (!nextShares.has(k)) barLen.delete(k);
    if (flashAt.size > 400) {
      for (const [k, t] of flashAt) if (now - t > 400) flashAt.delete(k);
    }
  }

  function drawSpark() {
    const [ctx, W, H] = sizeCanvas($("spark"));
    ctx.clearRect(0, 0, W, H);
    const pts = spark.get(selected);
    if (!pts || pts.length < 2) return;
    let lo = Infinity;
    let hi = -Infinity;
    for (const p of pts) {
      lo = Math.min(lo, p.mid);
      hi = Math.max(hi, p.mid);
    }
    if (hi - lo < 1) hi = lo + 1;
    const x = (t) => 12 + ((t - T0) / (T1 - T0)) * (W - 24);
    const y = (v) => 8 + (1 - (v - lo) / (hi - lo)) * (H - 20);

    // Playhead marks "now" in feed time.
    ctx.fillStyle = COLOR.edge;
    ctx.fillRect(x(engine.clock_ns()), 4, 1, H - 8);

    ctx.strokeStyle = COLOR.accent;
    ctx.lineWidth = 1.6;
    ctx.lineJoin = "round";
    ctx.beginPath();
    pts.forEach((p, i) => (i ? ctx.lineTo(x(p.t), y(p.mid)) : ctx.moveTo(x(p.t), y(p.mid))));
    ctx.stroke();
    const last = pts[pts.length - 1];
    ctx.fillStyle = COLOR.accent;
    ctx.beginPath();
    ctx.arc(x(last.t), y(last.mid), 2.6, 0, Math.PI * 2);
    ctx.fill();

    // Range annotation, top-left.
    ctx.font = `400 10px ${MONO}`;
    ctx.fillStyle = COLOR["ink-3"];
    ctx.textAlign = "left";
    ctx.textBaseline = "top";
    ctx.fillText(`mid ${fmtPrice(Math.round(lo))} – ${fmtPrice(Math.round(hi))}`, 14, 4);
  }

  function renderTape() {
    if (!tapeDirty) return;
    tapeDirty = false;
    const arr = tape.get(selected) ?? [];
    const rows = [];
    for (let i = arr.length - 1; i >= 0 && rows.length < 21; i--) {
      const e = arr[i];
      const cls = e.aggr > 0 ? "up" : e.aggr < 0 ? "dn" : "";
      const mark = e.aggr > 0 ? "▲" : e.aggr < 0 ? "▼" : "·";
      rows.push(
        `<div class="row ${cls}"><span class="t">${fmtClock(e.t)}</span>` +
          `<span class="p">${fmtPrice(e.price)}</span>` +
          `<span class="s">${e.shares.toLocaleString()}</span>` +
          `<span class="a">${mark}</span></div>`,
      );
    }
    $("tape").innerHTML = rows.join("");
    $("tape-sym").textContent = SYMBOLS.find((s) => locates[s] === selected);
  }

  function renderStats() {
    const msgs = engine.messages();
    $("s-msgs").innerHTML =
      `${msgs.toLocaleString()} <small>/ ${EXPECTED.messages.toLocaleString()}</small>`;
    $("s-engine-rate").innerHTML =
      `${stepMs > 0 ? fmtCount(stepMsgs / (stepMs / 1000)) : "–"}<small> msg/s</small>`;
    $("s-replay-rate").textContent = playing ? `${fmtCount(pace)} msg/s` : "paused";
    $("s-live").textContent = engine.live_orders().toLocaleString();
    const viol = engine.violations();
    $("s-viol").innerHTML = viol === 0 ? `0 <span class="ok">✓</span>` : String(viol);
    $("s-verify").innerHTML =
      `${viol === 0 ? "passing" : "failing"} <small>deep verify /1k msgs</small>`;
    $("clock").innerHTML =
      `<span class="microlabel">feed clock</span> <b>${fmtClock(engine.clock_ns())}</b> ET`;

    if (!scrubbing) {
      scrubber.value = msgs;
      scrubber.style.setProperty("--progress", `${(msgs / EXPECTED.messages) * 100}%`);
    }

    // Quote header for the selected book.
    const snap = engine.snapshot(selected, 1);
    if (snap[0] > 0 && snap[1] > 0) {
      const bb = snap[2];
      const ba = snap[2 + 3 * snap[0]];
      $("q-bid").textContent = fmtPrice(bb);
      $("q-ask").textContent = fmtPrice(ba);
      $("q-spread").textContent = fmtPrice(ba - bb);
    } else {
      $("q-bid").textContent = "–";
      $("q-ask").textContent = "–";
      $("q-spread").textContent = "–";
    }

    // Tab mid prices.
    SYMBOLS.forEach((s, i) => {
      const mid = midOf(locates[s]);
      tabs.children[i].querySelector(".mid").textContent = Number.isFinite(mid)
        ? fmtPrice(Math.round(mid))
        : "–";
    });
  }

  // --- verdict --------------------------------------------------------------------

  function onDone() {
    if (finishedOnce) return;
    finishedOnce = true;
    playing = false;
    playBtn.textContent = "↻ Replay";
    drainTape();

    const checks = [];
    const msgs = engine.messages();
    checks.push([`messages ${msgs.toLocaleString()} = ${EXPECTED.messages.toLocaleString()}`,
      msgs === EXPECTED.messages]);
    checks.push([`invariant violations ${engine.violations()} = 0`, engine.violations() === 0]);
    checks.push([`live orders ${engine.live_orders().toLocaleString()} = ${EXPECTED.liveOrders.toLocaleString()}`,
      engine.live_orders() === EXPECTED.liveOrders]);
    for (const [sym, [bid, ask]] of Object.entries(EXPECTED.quotes)) {
      const snap = engine.snapshot(locates[sym], 1);
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
      `<div class="title"><span class="${pass ? "pass-mark" : "fail-mark"}">` +
      `${pass ? "PASS" : "FAIL"}</span> — browser final state vs \`limitbook replay\`</div>` +
      `<div class="checks">` +
      checks.map(([txt, ok]) => `${ok ? "✓" : "✗"} ${txt}`).join("<br>") +
      `</div>`;
    // Machine-readable hook for the headless end-to-end check.
    window.__limitbook_verdict = { pass, checks };
    $("status").textContent = "Replay complete.";
  }

  // --- frame loop --------------------------------------------------------------------

  function frame(now) {
    if (pendingSeek >= 0) {
      const target = pendingSeek;
      pendingSeek = -1;
      seekTo(target);
    }

    if (playing && !engine.done()) {
      const frameStart = performance.now();
      const mult = SPEEDS[speedIdx].mult;
      // Clamp the frame delta so returning from a background tab doesn't
      // teleport the paced replay forward.
      const dt = Math.min(Math.max(now - lastFrame, 1), 100);
      let fed = 0;
      if (mult === Infinity) {
        while (!engine.done() && performance.now() - frameStart < FRAME_BUDGET_MS) {
          fed += feed(32768);
        }
      } else {
        simClock = Math.max(simClock, engine.clock_ns()) + dt * 1e6 * mult;
        // One message at a time: the loop must stop the moment the feed
        // clock passes the pacing cursor, or the overshoot compounds into
        // free speedup frame after frame.
        while (!engine.done() && engine.clock_ns() < simClock) {
          fed += feed(1);
          if ((fed & 127) === 0 && performance.now() - frameStart > FRAME_BUDGET_MS) {
            simClock = engine.clock_ns(); // can't keep up: resync, stay smooth
            break;
          }
        }
      }
      pace = pace * 0.85 + (fed / (dt / 1000)) * 0.15;
      drainTape();
      sampleSpark();
      if (engine.done()) onDone();
    }
    lastFrame = now;

    renderStats();
    renderTape();
    drawLadder();
    drawSpark();
    requestAnimationFrame(frame);
  }

  const READY_STATUS =
    `Capture loaded — ${(bytes.length / 1048576).toFixed(1)} MB of raw ITCH, ` +
    `${EXPECTED.messages.toLocaleString()} messages. Space to pause; MAX to unleash the engine.`;
  $("status").textContent = READY_STATUS;
  window.__limitbook_stats = () => ({
    messages: engine.messages(),
    violations: engine.violations(),
    stepMs,
    stepMsgs,
    engineRate: stepMs > 0 ? stepMsgs / (stepMs / 1000) : 0,
  });
  window.__limitbook_seek = (m) => {
    pendingSeek = m;
  };
  window.__limitbook_setSpeed = (i) => speedBox.children[i].click();
  window.__limitbook_ready = true;

  // Open mid-window so the first paint is a fully built book, then run at
  // feed speed — the demo starts alive.
  pendingSeek = Math.round(EXPECTED.messages * 0.35);
  setPlaying(true);
  requestAnimationFrame(frame);
}

main().catch((e) => {
  $("status").textContent = `Error: ${e.message ?? e}`;
  console.error(e);
});
