//! Thin wasm-bindgen wrapper around the `limitbook-core` replay engine.
//!
//! This crate contains no market logic: it owns the capture bytes, walks
//! the transport framing, and forwards each payload to
//! [`limitbook_core::replay::Replay`] — the exact engine `limitbook replay`
//! runs on the CLI. Everything the browser displays (levels, quotes,
//! counters) is read back out of that engine, so the demo and the CLI can
//! only disagree if this boundary layer is wrong.
//!
//! Snapshots cross the JS boundary as flat `f64` arrays (`Float64Array` on
//! the JS side) to keep per-frame reads allocation-light. Prices are raw
//! Price(4) ticks (1/10000 dollar) so the transfer is exact; formatting is
//! the caller's job.

use limitbook_core::frame::Messages;
use limitbook_core::parse::{Message, Price4, Side, decode, trim_padding};
use limitbook_core::replay::Replay;
use wasm_bindgen::prelude::*;

/// A replayable capture: framed ITCH 5.0 bytes plus the core replay engine.
#[wasm_bindgen]
pub struct Engine {
    data: Vec<u8>,
    /// Byte offset of the next unread length prefix in `data`.
    offset: usize,
    /// Set at end of stream or on the first framing error (framing cannot
    /// resynchronize past a bad frame).
    exhausted: bool,
    /// Whether `Replay::finish` has run (exactly once, after exhaustion).
    finished: bool,
    /// Timestamp of the most recently fed message (ns since midnight, 0
    /// before the first) — the feed's own wall clock, for display/pacing.
    clock: u64,
    /// Time-and-sales prints accumulated since the last `take_tape` call;
    /// see [`Engine::take_tape`] for the record layout.
    tape: Vec<f64>,
    /// Order reference the queue-position tracker is following, if any.
    /// Watching only appends to `watch_events` — it never feeds the engine,
    /// so replay state and CLI parity are untouched.
    watched: Option<u64>,
    /// Fate events for the watched order since the last `take_watch_events`
    /// call; see [`Engine::take_watch_events`] for the record layout.
    watch_events: Vec<f64>,
    replay: Replay,
}

/// Appends a time-and-sales record for an execution/trade payload (types
/// E/C/P) to `tape`. Prices and sides are taken from the message itself or
/// from the resting order it references — looked up in the book BEFORE the
/// message is fed, i.e. the true state at execution time. Non-printable "C"
/// executions are skipped (spec §1.4.2: excluded from time-and-sales).
fn record_print(replay: &Replay, tape: &mut Vec<f64>, payload: &[u8], clock: u64) {
    let Ok(msg) = decode(payload) else { return };
    let (locate, price, shares, resting_side) = match msg {
        Message::OrderExecuted(m) => {
            let Some(order) = replay.market().order(m.order_ref) else {
                return;
            };
            (
                m.header.stock_locate,
                order.price,
                u64::from(m.executed_shares),
                Some(order.side),
            )
        }
        Message::OrderExecutedWithPrice(m) => {
            if m.printable != b'Y' {
                return;
            }
            let side = replay.market().order(m.order_ref).map(|o| o.side);
            (
                m.header.stock_locate,
                m.execution_price,
                u64::from(m.executed_shares),
                side,
            )
        }
        // "P" trades hit non-displayed interest; their Side field is
        // always "B" per spec (uninformative), so the aggressor is unknown.
        Message::Trade(m) => (m.header.stock_locate, m.price, u64::from(m.shares), None),
        _ => return,
    };
    // A resting Sell was lifted by a buyer, and vice versa.
    let aggressor = match resting_side {
        Some(Side::Sell) => 1.0,
        Some(Side::Buy) => -1.0,
        None => 0.0,
    };
    tape.extend_from_slice(&[
        f64::from(locate),
        f64::from(price.0),
        shares as f64,
        aggressor,
        clock as f64,
    ]);
}

/// Appends a fate event to `events` when an order-mutation payload (types
/// E/C/X/D/U) references the watched order. Record layout (5 slots):
/// `(kind, qty, clock_ns, new_ref_hi, new_ref_lo)` where kind is 1 executed,
/// 2 cancelled, 3 deleted, 4 replaced; qty is the shares executed/cancelled
/// (the new total for a replace); the new-ref halves are nonzero only for a
/// replace and split the 64-bit successor reference so it crosses the f64
/// boundary exactly.
fn record_watch(watched: u64, events: &mut Vec<f64>, payload: &[u8], clock: u64) {
    let Ok(msg) = decode(payload) else { return };
    let (order_ref, kind, qty, new_ref) = match msg {
        Message::OrderExecuted(m) => (m.order_ref, 1.0, m.executed_shares, 0),
        Message::OrderExecutedWithPrice(m) => (m.order_ref, 1.0, m.executed_shares, 0),
        Message::OrderCancel(m) => (m.order_ref, 2.0, m.cancelled_shares, 0),
        Message::OrderDelete(m) => (m.order_ref, 3.0, 0, 0),
        Message::OrderReplace(m) => (m.original_order_ref, 4.0, m.shares, m.new_order_ref),
        _ => return,
    };
    if order_ref != watched {
        return;
    }
    events.extend_from_slice(&[
        kind,
        f64::from(qty),
        clock as f64,
        (new_ref >> 32) as f64,
        (new_ref & 0xffff_ffff) as f64,
    ]);
}

/// Timestamp from the uniform header: offset 5, 6 bytes big-endian, ns
/// since midnight (spec/itch50_spec.txt; every framed payload is at least
/// 11 bytes).
fn header_timestamp(payload: &[u8]) -> u64 {
    let mut ts = [0u8; 8];
    ts[2..8].copy_from_slice(&payload[5..11]);
    u64::from_be_bytes(ts)
}

#[wasm_bindgen]
impl Engine {
    /// Takes ownership of a raw (already gunzipped) capture. `verify_every`
    /// runs the deep book consistency check every N messages; 0 checks only
    /// at end of stream.
    #[wasm_bindgen(constructor)]
    pub fn new(data: Vec<u8>, verify_every: u32) -> Engine {
        Engine {
            data,
            offset: 0,
            exhausted: false,
            finished: false,
            clock: 0,
            tape: Vec::new(),
            watched: None,
            watch_events: Vec::new(),
            replay: Replay::new(u64::from(verify_every)),
        }
    }

    /// Feeds up to `max` framed messages through the engine and returns how
    /// many were processed. Returns 0 once the stream is exhausted (or a
    /// framing error made the remainder unreadable); `finish` runs then.
    pub fn step(&mut self, max: u32) -> u32 {
        if self.exhausted {
            return 0;
        }
        let mut it = Messages::new(&self.data[self.offset..]);
        let mut fed = 0u32;
        while fed < max {
            match it.next() {
                Some(Ok(payload)) => {
                    self.clock = header_timestamp(payload);
                    if matches!(payload[0], b'E' | b'C' | b'P') {
                        record_print(&self.replay, &mut self.tape, payload, self.clock);
                    }
                    if let Some(watched) = self.watched
                        && matches!(payload[0], b'E' | b'C' | b'X' | b'D' | b'U')
                    {
                        record_watch(watched, &mut self.watch_events, payload, self.clock);
                    }
                    self.replay.feed(payload);
                    fed += 1;
                }
                Some(Err(_)) | None => {
                    self.exhausted = true;
                    break;
                }
            }
        }
        self.offset += it.offset();
        if self.offset == self.data.len() {
            self.exhausted = true;
        }
        if self.exhausted && !self.finished {
            self.replay.finish();
            self.finished = true;
        }
        fed
    }

    /// True once the whole capture has been fed and `finish` has run.
    pub fn done(&self) -> bool {
        self.exhausted
    }

    /// Framed messages processed so far.
    pub fn messages(&self) -> f64 {
        self.replay.stats().messages as f64
    }

    /// The feed's own wall clock: timestamp of the most recently fed
    /// message, in ns since midnight ET (0 before the first message).
    /// Exact as f64 — a day is < 2^53 ns.
    pub fn clock_ns(&self) -> f64 {
        self.clock as f64
    }

    /// Drains time-and-sales prints accumulated since the last call, as a
    /// flat array of 5-element records:
    /// `(locate, price_ticks, shares, aggressor, clock_ns)` in feed order.
    /// `aggressor` is +1 when a resting Sell was hit (buyer-initiated), -1
    /// for a resting Buy, 0 unknown (non-displayed "P" trades). Prices are
    /// raw Price(4) ticks.
    pub fn take_tape(&mut self) -> Vec<f64> {
        core::mem::take(&mut self.tape)
    }

    /// Total invariant violations of any kind (0 on a clean replay).
    pub fn violations(&self) -> f64 {
        self.replay.stats().violations() as f64
    }

    /// Live orders across all books right now.
    pub fn live_orders(&self) -> u32 {
        self.replay.market().live_orders() as u32
    }

    /// Stock locates learned from the Stock Directory, in locate order.
    pub fn symbol_locates(&self) -> Vec<u16> {
        self.replay.symbols().keys().copied().collect()
    }

    /// Symbol for a locate (empty string if unknown).
    pub fn symbol_name(&self, locate: u16) -> String {
        match self.replay.symbols().get(&locate) {
            Some(stock) => String::from_utf8_lossy(trim_padding(stock)).into_owned(),
            None => String::new(),
        }
    }

    /// Locate for a symbol, or -1 if the directory has no such symbol.
    pub fn locate(&self, symbol: &str) -> i32 {
        self.replay
            .symbols()
            .iter()
            .find(|(_, stock)| trim_padding(*stock) == symbol.as_bytes())
            .map_or(-1, |(locate, _)| i32::from(*locate))
    }

    /// Top-`depth` levels each side of one book as a flat array:
    /// `[n_bid, n_ask, bid levels..., ask levels...]` where each level is
    /// `(price_ticks, shares, order_count)`, best first. Element 0/1 tell
    /// the caller how many levels of each side follow. Prices are raw
    /// Price(4) ticks. Empty book (or unknown locate) yields `[0, 0]`.
    pub fn snapshot(&self, locate: u16, depth: u32) -> Vec<f64> {
        let mut out = alloc_snapshot(depth);
        out.push(0.0);
        out.push(0.0);
        let Some(book) = self.replay.market().book(locate) else {
            return out;
        };
        for (i, side) in [Side::Buy, Side::Sell].into_iter().enumerate() {
            let mut n = 0.0;
            for (price, shares, orders) in book.side_levels(side).take(depth as usize) {
                out.push(f64::from(price.0));
                out.push(shares as f64);
                out.push(orders as f64);
                n += 1.0;
            }
            out[i] = n;
        }
        out
    }

    /// The FIFO queue at one price level, in time priority (first in line
    /// first), as flat `(order_ref, shares)` pairs — a `BigUint64Array` on
    /// the JS side, so 64-bit order reference numbers cross the boundary
    /// exactly and stay usable as identities. `bid` selects the side;
    /// prices are raw Price(4) ticks. Empty if the level does not exist.
    /// Read-only over already-computed book state.
    pub fn level_queue(&self, locate: u16, bid: bool, price_ticks: u32) -> Vec<u64> {
        let side = if bid { Side::Buy } else { Side::Sell };
        let mut out = Vec::new();
        for (order_ref, shares) in self
            .replay
            .market()
            .queue_at(locate, side, Price4(price_ticks))
        {
            out.push(order_ref);
            out.push(u64::from(shares));
        }
        out
    }

    /// A live order's current book position, for the tracking overlay:
    /// `[price_ticks, is_bid, shares, rank, shares_ahead, queue_len,
    /// level_shares]` (rank = orders ahead of it in its level's queue, 0 =
    /// front of the line). Empty if the reference is not live. Read-only.
    pub fn order_position(&self, order_ref: u64) -> Vec<f64> {
        let market = self.replay.market();
        let Some(order) = market.order(order_ref) else {
            return Vec::new();
        };
        let Some((rank, shares_ahead)) = market.queue_position(order_ref) else {
            return Vec::new();
        };
        let Some(book) = market.book(order.stock_locate) else {
            return Vec::new();
        };
        let queue_len = book.orders_at(order.side, order.price).count();
        let level_shares = book.shares_at(order.side, order.price).unwrap_or(0);
        vec![
            f64::from(order.price.0),
            if order.side == Side::Buy { 1.0 } else { 0.0 },
            f64::from(order.shares),
            rank as f64,
            shares_ahead as f64,
            queue_len as f64,
            level_shares as f64,
        ]
    }

    /// Starts recording fate events for one order reference (there is at
    /// most one watched order). Watching is presentation state only: it
    /// never mutates the book or alters replay output.
    pub fn watch(&mut self, order_ref: u64) {
        self.watched = Some(order_ref);
        self.watch_events.clear();
    }

    /// Stops watching and drops any undrained events.
    pub fn unwatch(&mut self) {
        self.watched = None;
        self.watch_events.clear();
    }

    /// Drains fate events for the watched order accumulated since the last
    /// call, as flat 5-element records — see [`record_watch`] for the
    /// layout.
    pub fn take_watch_events(&mut self) -> Vec<f64> {
        core::mem::take(&mut self.watch_events)
    }
}

fn alloc_snapshot(depth: u32) -> Vec<f64> {
    Vec::with_capacity(2 + 2 * 3 * depth as usize)
}
