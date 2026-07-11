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
use limitbook_core::parse::{Side, trim_padding};
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
    replay: Replay,
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
}

fn alloc_snapshot(depth: u32) -> Vec<f64> {
    Vec::with_capacity(2 + 2 * 3 * depth as usize)
}
