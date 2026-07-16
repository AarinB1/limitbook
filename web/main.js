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
const DEPTH_LEVELS = 180; // price levels per side fed to the depth chart
const DEPTH_WINDOW = 0.0025; // depth-chart half-window as a fraction of mid

const $ = (id) => document.getElementById(id);
const MONO = getComputedStyle(document.documentElement)
  .getPropertyValue("--mono")
  .trim();
const COLOR = {};
for (const key of ["surface-2", "edge", "ink", "ink-2", "ink-3", "bid", "ask",
  "bid-ink", "ask-ink", "accent", "accent-ink", "flash"]) {
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

  // Queue inspector state. The panel shows one price level's FIFO queue:
  // by default the best bid ("top", auto-follows), or a level pinned by
  // clicking the ladder ("pin"), or the level of a tracked order ("order").
  let queueSel = { mode: "top" };
  // Tracked resting order, or null. ref is a BigInt (order reference
  // numbers are u64 and cross the wasm boundary exactly); pos is the last
  // order_position readback; fate is set once the order leaves the book.
  // Order refs are deterministic in the stream, so tracking survives seeks.
  let tracked = null;

  function freshEngine(withPrime = true) {
    engine?.free();
    engine = new Engine(bytes, VERIFY_EVERY);
    for (const arr of tape.values()) arr.length = 0;
    for (const arr of spark.values()) arr.length = 0;
    sparkLastSample = -Infinity;
    tapeDirty = true;
    if (tracked) {
      // Re-arm the watch on the fresh engine and re-derive the order's
      // state from the stream (a backward seek can resurrect it).
      engine.watch(tracked.ref);
      tracked.last = null;
      tracked.fate = null;
      tracked.pos = null;
    }
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
      resetQueueState(); // orders belong to one book; tracking doesn't cross
      resetDepthState();
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
      resetDepthState();
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
    drainWatchEvents();
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
    } else if (e.key === "Escape") {
      resetQueueState(); // untrack + back to following the top of book
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
        // not repaints; new levels grow in from the center line unless motion is reduced.
        const target = Math.min(barMax, Math.max(2, (shares / smoothMax) * barMax));
        const shown = barLen.get(key);
        const initialLen = EASE === 1 ? target : 2;
        const len = shown === undefined ? initialLen : shown + (target - shown) * EASE;
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
    // Geometry for click hit-testing (click a row = inspect its queue).
    ladderLayout = { top, rowH, mid, snap, nBid, nAsk };
    // Pinned-level cue: a hairline bracket on the inspected row.
    if (queueSel.mode !== "top") {
      const lvl = inspectedLevel();
      if (lvl) {
        const base = lvl.bid ? bidBase : askBase;
        const n = lvl.bid ? nBid : nAsk;
        for (let row = 0; row < Math.min(n, DEPTH); row++) {
          if (snap[base + 3 * row] === lvl.price) {
            const y0 = top + row * rowH;
            ctx.strokeStyle = COLOR["accent-ink"];
            ctx.globalAlpha = 0.8;
            ctx.strokeRect(
              lvl.bid ? 8.5 : mid + 3.5, y0 + 1.5, mid - 11.5, rowH - 3);
            ctx.globalAlpha = 1;
            break;
          }
        }
      }
    }
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

  // --- queue panel: price-time priority, made visible -----------------------

  const QUEUE_HINT =
    "click a ladder price to inspect its queue · click an order to track " +
    "its place in line · esc resets";
  let ladderLayout = null; // last ladder draw geometry, for click hit-tests
  let queueHits = []; // row hit boxes from the last queue draw: {y0, y1, ref}
  let queueNoteHtml = null; // cache so the DOM is only touched on change

  function resetQueueState() {
    queueSel = { mode: "top" };
    if (tracked) engine.unwatch();
    tracked = null;
  }

  function track(ref) {
    tracked = { ref, last: null, fate: null, pos: null, lastLevel: null,
      replaced: false };
    engine.watch(ref);
    queueSel = { mode: "order" };
  }

  function untrack() {
    engine.unwatch();
    tracked = null;
    if (queueSel.mode === "order") queueSel = { mode: "top" };
  }

  function fmtRef(ref) {
    return `…${String(ref % 100000n).padStart(5, "0")}`;
  }

  // Drains fate events for the tracked order (5-slot records, see
  // Engine::take_watch_events) and re-reads its position. The engine
  // follows replaces to the successor ref on its side; mirror that here.
  function drainWatchEvents() {
    if (!tracked) return;
    const ev = engine.take_watch_events();
    for (let i = 0; i < ev.length; i += 5) {
      const kind = ev[i];
      if (kind === 4) {
        tracked.ref = (BigInt(ev[i + 3]) << 32n) | BigInt(ev[i + 4]);
        tracked.replaced = true;
      }
      tracked.last = { kind, qty: ev[i + 1], t: ev[i + 2] };
    }
    const pos = engine.order_position(tracked.ref);
    if (pos.length) {
      tracked.pos = pos;
      tracked.fate = null;
      tracked.lastLevel = { bid: pos[1] === 1, price: pos[0] };
    } else {
      tracked.pos = null;
      // Only a witnessed event assigns a fate; with none the order simply
      // isn't in the book at this position (e.g. after a backward seek).
      if (!tracked.fate && tracked.last) {
        tracked.fate = {
          label: tracked.last.kind === 1 ? "FILLED" : "CANCELLED",
          t: tracked.last.t,
        };
      }
    }
  }

  /// The level the queue panel shows right now.
  function inspectedLevel() {
    if (queueSel.mode === "pin") return { bid: queueSel.bid, price: queueSel.price };
    if (queueSel.mode === "order" && tracked && tracked.lastLevel) {
      return tracked.lastLevel;
    }
    const snap = engine.snapshot(selected, 1);
    if (snap[0] === 0) return null;
    return { bid: true, price: snap[2] };
  }

  function queueNote() {
    if (!tracked) return QUEUE_HINT;
    const short = fmtRef(tracked.ref);
    if (tracked.fate) {
      return `<b>${short} ${tracked.fate.label}</b> at ${fmtClock(tracked.fate.t)} — esc resets`;
    }
    if (!tracked.pos) return `tracking ${short} — not in book at this position`;
    const [, , , rank, ahead, len] = tracked.pos;
    if (rank === 0) return `tracking <b>${short} · FRONT OF QUEUE</b> · first of ${len} in line`;
    return (
      `tracking <b>${short}</b> · #${rank + 1} of ${len} · ` +
      `${ahead.toLocaleString()} sh ahead` +
      (tracked.replaced ? " · priority reset by replace" : "")
    );
  }

  function drawQueue() {
    const [ctx, W, H] = sizeCanvas($("queue"));
    ctx.clearRect(0, 0, W, H);
    queueHits = [];

    const note = queueNote();
    if (note !== queueNoteHtml) {
      queueNoteHtml = note;
      $("queue-note").innerHTML = note;
    }

    const level = inspectedLevel();
    if (!level) {
      $("queue-level").textContent = "–";
      return;
    }
    const flat = engine.level_queue(selected, level.bid, level.price);
    const n = flat.length / 2;
    let total = 0;
    for (let i = 0; i < n; i++) total += Number(flat[2 * i + 1]);
    $("queue-level").textContent =
      `${queueSel.mode === "top" ? "top · " : ""}${level.bid ? "BID" : "ASK"} ` +
      `${fmtPrice(level.price)} · ${n} × ${total.toLocaleString()} sh`;

    const pad = 14;
    if (n === 0) {
      ctx.font = `400 11.5px ${MONO}`;
      ctx.fillStyle = COLOR["ink-3"];
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText("no resting orders at this level", W / 2, 56);
      return;
    }
    const barColor = level.bid ? COLOR.bid : COLOR.ask;
    const inkColor = level.bid ? COLOR["bid-ink"] : COLOR["ask-ink"];

    // The line itself: one segment per order, width ∝ shares, front of the
    // queue at the left. Gaps keep individual orders readable as segments.
    const barY = 10;
    const barH = 16;
    const gap = n > 48 ? 0.5 : 1;
    const usable = W - 2 * pad - gap * (n - 1);
    let x = pad;
    for (let i = 0; i < n; i++) {
      const isTracked = tracked && !tracked.fate && flat[2 * i] === tracked.ref;
      const w = Math.max(1, (Number(flat[2 * i + 1]) / total) * usable);
      ctx.fillStyle = isTracked ? COLOR.accent : barColor;
      ctx.globalAlpha = isTracked ? 0.95 : i === 0 ? 0.6 : 0.32;
      ctx.fillRect(x, barY, w, barH);
      x += w + gap;
    }
    ctx.globalAlpha = 1;

    // Column headings, ladder-style.
    const headY = barY + barH + 16;
    ctx.textBaseline = "middle";
    ctx.font = `600 10px ${MONO}`;
    ctx.fillStyle = COLOR["ink-3"];
    ctx.textAlign = "left";
    ctx.fillText("#", pad, headY);
    ctx.fillText("ORDER", pad + 26, headY);
    ctx.textAlign = "right";
    ctx.fillText("SIZE", W - pad - 64, headY);
    ctx.fillText("CUM", W - pad, headY);

    // Rows in time priority with cumulative shares. When the tracked order
    // sits deeper than the window, scroll it into view and elide around it.
    const rowsTop = headY + 14;
    const rowH = 21;
    const visible = Math.max(2, Math.floor((H - rowsTop - 6) / rowH));
    const trackedRank =
      tracked && !tracked.fate && tracked.pos && queueSel.mode === "order"
        ? tracked.pos[3]
        : -1;
    let start = 0;
    if (trackedRank >= visible - 2) {
      start = Math.min(Math.max(0, n - visible + 1),
        trackedRank - Math.floor(visible / 2));
    }
    let end = Math.min(n, start + visible - (start > 0 ? 1 : 0));
    if (end < n) end = Math.min(n, end) - 1; // reserve a slot for the tail
    let cum = 0;
    for (let i = 0; i < start; i++) cum += Number(flat[2 * i + 1]);
    let y = rowsTop + rowH / 2;
    ctx.font = `400 10.5px ${MONO}`;
    if (start > 0) {
      ctx.fillStyle = COLOR["ink-3"];
      ctx.textAlign = "left";
      ctx.fillText(`⋯ ${start} ahead · ${cum.toLocaleString()} sh`, pad, y);
      y += rowH;
    }
    for (let i = start; i < end; i++) {
      const ref = flat[2 * i];
      const shares = Number(flat[2 * i + 1]);
      cum += shares;
      const isTracked = tracked && !tracked.fate && ref === tracked.ref;
      if (isTracked) {
        ctx.globalAlpha = 0.14;
        ctx.fillStyle = COLOR.accent;
        ctx.fillRect(6, y - rowH / 2 + 1, W - 12, rowH - 2);
        ctx.globalAlpha = 1;
      }
      ctx.font = `${i === 0 ? "700" : "400"} 11.5px ${MONO}`;
      ctx.textAlign = "left";
      ctx.fillStyle = i === 0 ? inkColor : COLOR["ink-3"];
      ctx.fillText(String(i + 1).padStart(2, "0"), pad, y);
      ctx.fillStyle = isTracked ? COLOR["accent-ink"] : COLOR.ink;
      ctx.fillText(fmtRef(ref), pad + 26, y);
      ctx.textAlign = "right";
      ctx.fillStyle = COLOR["ink-2"];
      ctx.fillText(shares.toLocaleString(), W - pad - 64, y);
      ctx.fillStyle = COLOR["ink-3"];
      ctx.fillText(cum.toLocaleString(), W - pad, y);
      queueHits.push({ y0: y - rowH / 2, y1: y + rowH / 2, ref });
      y += rowH;
    }
    if (end < n) {
      ctx.font = `400 10.5px ${MONO}`;
      ctx.fillStyle = COLOR["ink-3"];
      ctx.textAlign = "left";
      ctx.fillText(
        `⋯ ${n - end} behind · ${(total - cum).toLocaleString()} sh`, pad, y);
    }
  }

  // --- cumulative depth chart ------------------------------------------------

  let depthMid = 0; // eased center of the price window (ticks)
  let depthHalf = 0; // eased half-width of the price window (ticks)
  let depthMax = 0; // eased cumulative-shares scale
  let depthHover = null; // hover x in CSS px, or null
  let depthLabelText = null;

  function resetDepthState() {
    depthMid = 0;
    depthHalf = 0;
    depthMax = 0;
  }

  function drawDepth() {
    const [ctx, W, H] = sizeCanvas($("depth"));
    ctx.clearRect(0, 0, W, H);
    const snap = engine.snapshot(selected, DEPTH_LEVELS);
    const nBid = snap[0];
    const nAsk = snap[1];
    if (nBid === 0 || nAsk === 0) {
      if (depthLabelText !== "–") $("depth-label").textContent = depthLabelText = "–";
      return;
    }
    const bb = snap[2];
    const ba = snap[2 + 3 * nBid];
    const mid = (bb + ba) / 2;
    // Price window around the mid, eased so the frame doesn't jump; the
    // scale eases the same way the ladder's does (exact under reduced
    // motion, where SCALE_EASE is 1).
    const targetHalf = Math.max(mid * DEPTH_WINDOW, (ba - bb) * 2, 400);
    depthMid = depthMid > 0 ? depthMid + (mid - depthMid) * SCALE_EASE : mid;
    depthHalf = depthHalf > 0 ? depthHalf + (targetHalf - depthHalf) * SCALE_EASE : targetHalf;
    const lo = depthMid - depthHalf;
    const hi = depthMid + depthHalf;

    // Step curves: cumulative resting shares from the touch outward, per
    // side, clipped to the window. The curve only extends flat to the
    // window edge when the book is truly exhausted inside it — a curve cut
    // off by the DEPTH_LEVELS cap stops at the last known level instead of
    // fabricating depth.
    const curve = (base, count, ask) => {
      const pts = [];
      let cum = 0;
      for (let i = 0; i < count; i++) {
        const price = snap[base + 3 * i];
        if (ask ? price > hi : price < lo) {
          pts.push([ask ? hi : lo, cum]);
          return pts;
        }
        cum += snap[base + 3 * i + 1];
        pts.push([price, cum]);
      }
      if (count < DEPTH_LEVELS && pts.length) pts.push([ask ? hi : lo, cum]);
      return pts;
    };
    const bids = curve(2, nBid, false);
    const asks = curve(2 + 3 * nBid, nAsk, true);
    const worst = Math.max(bids[bids.length - 1][1], asks[asks.length - 1][1], 1);
    depthMax = depthMax > 0 ? depthMax + (worst - depthMax) * SCALE_EASE : worst;
    const yMax = Math.max(depthMax, worst) * 1.06; // never clip the curve

    const padX = 14;
    const top = 18;
    const bottom = 18;
    const x = (p) => padX + ((p - lo) / (hi - lo)) * (W - 2 * padX);
    const y = (c) => top + (1 - c / yMax) * (H - top - bottom);

    // Mid marker first, under the curves.
    ctx.fillStyle = COLOR.edge;
    ctx.fillRect(x(mid), top - 6, 1, H - top - bottom + 12);

    const side = (pts, color, ink) => {
      const path = new Path2D();
      let curY = y(0);
      let lastX = x(pts[0][0]);
      path.moveTo(lastX, curY);
      for (const [p, c] of pts) {
        lastX = x(p);
        path.lineTo(lastX, curY);
        curY = y(c);
        path.lineTo(lastX, curY);
      }
      const fill = new Path2D(path);
      fill.lineTo(lastX, y(0));
      fill.closePath();
      ctx.globalAlpha = 0.13;
      ctx.fillStyle = color;
      ctx.fill(fill);
      ctx.globalAlpha = 1;
      ctx.strokeStyle = ink;
      ctx.lineWidth = 1.6;
      ctx.lineJoin = "round";
      ctx.stroke(path);
    };
    side(bids, COLOR.bid, COLOR["bid-ink"]);
    side(asks, COLOR.ask, COLOR["ask-ink"]);

    // Direct side labels (identity is never color-alone) + price scale.
    ctx.textBaseline = "middle";
    ctx.font = `600 10px ${MONO}`;
    ctx.textAlign = "left";
    ctx.fillStyle = COLOR["bid-ink"];
    ctx.fillText("BID", padX, 9);
    ctx.fillStyle = COLOR["ink-3"];
    ctx.fillText(fmtPrice(Math.round(lo)), padX, H - 8);
    ctx.textAlign = "right";
    ctx.fillStyle = COLOR["ask-ink"];
    ctx.fillText("ASK", W - padX, 9);
    ctx.fillStyle = COLOR["ink-3"];
    ctx.fillText(fmtPrice(Math.round(hi)), W - padX, H - 8);
    ctx.textAlign = "center";
    ctx.fillText(`mid ${fmtPrice(Math.round(mid))}`, x(mid), H - 8);

    // Hover readout: price + cumulative shares at the cursor.
    if (depthHover !== null && depthHover >= padX && depthHover <= W - padX) {
      const price = lo + ((depthHover - padX) / (W - 2 * padX)) * (hi - lo);
      const bidSide = price <= mid;
      const pts = bidSide ? bids : asks;
      let cumAt = 0;
      for (const [p, c] of pts) {
        if (bidSide ? p >= price : p <= price) cumAt = c;
        else break;
      }
      ctx.fillStyle = COLOR.edge;
      ctx.fillRect(depthHover, top - 4, 1, H - top - bottom + 8);
      ctx.font = `400 10.5px ${MONO}`;
      ctx.fillStyle = COLOR.ink;
      ctx.textAlign = depthHover > W / 2 ? "right" : "left";
      const tx = depthHover + (depthHover > W / 2 ? -6 : 6);
      const inSpread = price > bb && price < ba;
      ctx.fillText(
        inSpread
          ? `${fmtPrice(Math.round(price))} · inside spread`
          : `${fmtPrice(Math.round(price))} · ${fmtCount(cumAt)} sh`,
        tx, 9);
    }

    const label = `${SYMBOLS.find((s) => locates[s] === selected)} · mid ±` +
      `${((depthHalf / depthMid) * 100).toFixed(2)}%`;
    if (label !== depthLabelText) {
      depthLabelText = label;
      $("depth-label").textContent = label;
    }
  }

  // --- queue + depth interaction ---------------------------------------------

  $("ladder").addEventListener("click", (e) => {
    if (!ladderLayout) return;
    const { top, rowH, mid, snap, nBid, nAsk } = ladderLayout;
    const row = Math.floor((e.offsetY - top) / rowH);
    if (row < 0 || row >= DEPTH) return;
    const bid = e.offsetX < mid;
    if (row >= (bid ? nBid : nAsk)) return;
    const price = snap[(bid ? 2 : 2 + 3 * nBid) + 3 * row];
    if (queueSel.mode === "pin" && queueSel.bid === bid && queueSel.price === price) {
      queueSel = { mode: "top" }; // clicking the pinned row unpins
    } else {
      queueSel = { mode: "pin", bid, price };
    }
  });

  $("queue").addEventListener("click", (e) => {
    const hit = queueHits.find((h) => e.offsetY >= h.y0 && e.offsetY < h.y1);
    if (!hit) return;
    if (tracked && hit.ref === tracked.ref) untrack();
    else track(hit.ref);
  });

  $("depth").addEventListener("mousemove", (e) => (depthHover = e.offsetX));
  $("depth").addEventListener("mouseleave", () => (depthHover = null));

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

    drainWatchEvents();
    renderStats();
    renderTape();
    drawLadder();
    drawSpark();
    drawQueue();
    drawDepth();
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
  // Queue-view hooks for the headless end-to-end check: the displayed FIFO
  // queue read straight from the engine, and the inspector's state.
  window.__limitbook_queue = (bid, price) =>
    Array.from(engine.level_queue(selected, bid, price), (v) => v.toString());
  window.__limitbook_inspect = () => {
    const level = inspectedLevel();
    return {
      mode: queueSel.mode,
      level: level ? { bid: level.bid, price: level.price } : null,
      tracked: tracked
        ? {
            ref: tracked.ref.toString(),
            rank: tracked.pos ? tracked.pos[3] : -1,
            fate: tracked.fate ? tracked.fate.label : null,
          }
        : null,
      rows: queueHits.map((h) => h.ref.toString()),
    };
  };
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
