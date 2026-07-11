//! Replay: stream framed payloads through parse + book, accumulating the
//! statistics and invariant results a verification report needs.
//!
//! This is pure computation (no I/O) so the CLI, the fixture integration
//! tests, and the eventual wasm demo all drive the exact same engine —
//! there is one definition of "the fixture replays cleanly".
//!
//! What counts as a violation (any of these makes a replay unclean):
//! - a payload that fails to decode ([`ParseError`]);
//! - a message the book must reject ([`BookError`]) — dangling references,
//!   overfills, duplicate order ids;
//! - a crossed book (best bid >= best ask) while its security is in
//!   continuous trading; pre-open and halted books legitimately cross, so
//!   those are out of scope by construction;
//! - a deep-consistency failure from [`Market::verify`]
//!   ([`ConsistencyError`]) — always a bug in this crate, never the feed;
//! - an add whose stock field disagrees with the Stock Directory entry for
//!   its locate (a mis-sliced offset would surface exactly here).
//!
//! Timestamp regressions are tracked but are observations, not violations:
//! the three book invariants don't depend on them.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::book::{BookError, ConsistencyError, Effect, Market};
use crate::parse::{self, EventCode, Message, ParseError, Stock};

/// Number of violations kept with full detail; beyond this only the
/// counters grow. Keeps a badly broken replay's memory bounded.
pub const MAX_DETAILED_VIOLATIONS: usize = 32;

/// One recorded violation, tagged with the 0-based message index it
/// occurred at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Violation {
    pub msg_index: u64,
    pub kind: ViolationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    Parse(ParseError),
    Book(BookError),
    /// The book crossed while the security was in continuous trading.
    CrossedInContinuous {
        locate: u16,
    },
    Consistency(ConsistencyError),
    /// An add's stock symbol disagreed with the directory for its locate.
    SymbolMismatch {
        locate: u16,
    },
}

/// Counters over one replay. All shares totals are u64 to survive a full
/// trading day.
#[derive(Debug, Clone)]
pub struct Stats {
    /// Total framed payloads fed.
    pub messages: u64,
    /// Message count per raw type byte.
    pub by_type: [u64; 256],
    pub orders_added: u64,
    pub shares_added: u64,
    /// "E" events / shares.
    pub exec_events: u64,
    pub exec_shares: u64,
    /// "C" events / shares, and how many were marked non-printable.
    pub exec_with_price_events: u64,
    pub exec_with_price_shares: u64,
    pub exec_nonprintable: u64,
    /// Orders that reached zero shares via "E"/"C" (fully filled).
    pub orders_filled: u64,
    /// "X" events / shares, and orders that died by cancelling to zero.
    pub cancel_events: u64,
    pub cancelled_shares: u64,
    pub orders_cancelled_out: u64,
    /// "D" events and the shares still resting when deleted.
    pub deletes: u64,
    pub deleted_shares: u64,
    /// "U" events.
    pub replaces: u64,
    /// "P" (non-cross trade) events / shares.
    pub trades: u64,
    pub trade_shares: u64,
    /// "Q" (cross trade) events / shares.
    pub cross_trades: u64,
    pub cross_shares: u64,
    /// Peak live orders across all books, and the message index reaching it.
    pub peak_live_orders: usize,
    pub peak_live_orders_at: u64,
    /// Peak live orders in a single book, with its locate.
    pub peak_book_orders: usize,
    pub peak_book_orders_locate: u16,
    /// Peak price levels on a single side of a single book.
    pub peak_side_levels: usize,
    pub peak_side_levels_locate: u16,
    /// Messages whose timestamp went backwards (observation, not violation).
    pub timestamp_regressions: u64,
    pub last_timestamp: u64,
    /// Violation counters (see module docs).
    pub parse_errors: u64,
    pub book_errors: u64,
    pub crossed_in_continuous: u64,
    pub consistency_failures: u64,
    pub symbol_mismatches: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            messages: 0,
            by_type: [0; 256],
            orders_added: 0,
            shares_added: 0,
            exec_events: 0,
            exec_shares: 0,
            exec_with_price_events: 0,
            exec_with_price_shares: 0,
            exec_nonprintable: 0,
            orders_filled: 0,
            cancel_events: 0,
            cancelled_shares: 0,
            orders_cancelled_out: 0,
            deletes: 0,
            deleted_shares: 0,
            replaces: 0,
            trades: 0,
            trade_shares: 0,
            cross_trades: 0,
            cross_shares: 0,
            peak_live_orders: 0,
            peak_live_orders_at: 0,
            peak_book_orders: 0,
            peak_book_orders_locate: 0,
            peak_side_levels: 0,
            peak_side_levels_locate: 0,
            timestamp_regressions: 0,
            last_timestamp: 0,
            parse_errors: 0,
            book_errors: 0,
            crossed_in_continuous: 0,
            consistency_failures: 0,
            symbol_mismatches: 0,
        }
    }
}

impl Stats {
    /// Total execution events ("E" + "C").
    pub fn executions(&self) -> u64 {
        self.exec_events + self.exec_with_price_events
    }

    /// Total violations of any kind.
    pub fn violations(&self) -> u64 {
        self.parse_errors
            + self.book_errors
            + self.crossed_in_continuous
            + self.consistency_failures
            + self.symbol_mismatches
    }
}

/// Per-locate activity counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolStats {
    pub adds: u64,
    pub exec_events: u64,
    pub exec_shares: u64,
    pub cancels: u64,
    pub deletes: u64,
    pub replaces: u64,
    pub trades: u64,
    pub trade_shares: u64,
    pub peak_live_orders: usize,
}

/// The replay engine: feed it framed payloads in stream order, then read
/// the stats, violations, and final market.
#[derive(Debug, Clone, Default)]
pub struct Replay {
    market: Market,
    stats: Stats,
    /// Locate -> symbol, learned from Stock Directory ("R") messages.
    symbols: BTreeMap<u16, Stock>,
    per_symbol: BTreeMap<u16, SymbolStats>,
    /// System events in stream order with their timestamps — the market
    /// phase story of the replay (at most a handful per day).
    system_events: Vec<(EventCode, u64)>,
    violations: Vec<Violation>,
    /// Run a deep [`Market::verify`] every N messages (0 = only at
    /// [`Replay::finish`]).
    verify_every: u64,
}

impl Replay {
    /// `verify_every`: run the deep consistency check every N messages;
    /// 0 verifies only in [`finish`](Self::finish).
    pub fn new(verify_every: u64) -> Self {
        Replay {
            verify_every,
            ..Replay::default()
        }
    }

    pub fn market(&self) -> &Market {
        &self.market
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// The first [`MAX_DETAILED_VIOLATIONS`] violations in stream order;
    /// `stats().violations()` has the true total.
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// Locate -> symbol directory learned from "R" messages, in locate
    /// order.
    pub fn symbols(&self) -> &BTreeMap<u16, Stock> {
        &self.symbols
    }

    /// Per-locate activity, in locate order.
    pub fn per_symbol(&self) -> &BTreeMap<u16, SymbolStats> {
        &self.per_symbol
    }

    /// System events seen, in stream order, with timestamps.
    pub fn system_events(&self) -> &[(EventCode, u64)] {
        &self.system_events
    }

    fn record(&mut self, msg_index: u64, kind: ViolationKind) {
        match kind {
            ViolationKind::Parse(_) => self.stats.parse_errors += 1,
            ViolationKind::Book(_) => self.stats.book_errors += 1,
            ViolationKind::CrossedInContinuous { .. } => self.stats.crossed_in_continuous += 1,
            ViolationKind::Consistency(_) => self.stats.consistency_failures += 1,
            ViolationKind::SymbolMismatch { .. } => self.stats.symbol_mismatches += 1,
        }
        if self.violations.len() < MAX_DETAILED_VIOLATIONS {
            self.violations.push(Violation { msg_index, kind });
        }
    }

    /// Checks the crossed-book invariant for one locate right now.
    fn check_crossed(&mut self, msg_index: u64, locate: u16) {
        if self.market.cross_violation(locate) {
            self.record(msg_index, ViolationKind::CrossedInContinuous { locate });
        }
    }

    /// Backstop for the reopening-cross grace: any production book still
    /// crossed and never re-armed since its last release is a persistent
    /// crossing — a real violation the arming suppression must not hide.
    /// Run while market hours are still in force (just before the message
    /// ending them is applied, and at end of stream).
    fn check_pending_crossings(&mut self, msg_index: u64) {
        let pending: Vec<u16> = self
            .market
            .books()
            .map(|(locate, _)| locate)
            .filter(|&locate| self.market.cross_check_pending(locate))
            .collect();
        for locate in pending {
            self.record(msg_index, ViolationKind::CrossedInContinuous { locate });
        }
    }

    /// Feeds one framed payload (no length prefix) in stream order.
    pub fn feed(&mut self, payload: &[u8]) {
        let idx = self.stats.messages;
        self.stats.messages += 1;
        if let Some(&ty) = payload.first() {
            self.stats.by_type[ty as usize] += 1;
        }

        let msg = match parse::decode(payload) {
            Ok(msg) => msg,
            Err(e) => {
                self.record(idx, ViolationKind::Parse(e));
                return;
            }
        };

        let header = *msg.header();
        if header.timestamp < self.stats.last_timestamp {
            self.stats.timestamp_regressions += 1;
        }
        self.stats.last_timestamp = header.timestamp;

        match &msg {
            Message::SystemEvent(m) => {
                // Market hours are about to end: report books that stayed
                // crossed from their last release to the close (the arming
                // suppression window closed without ever re-arming).
                if self.market.market_hours()
                    && matches!(
                        m.event_code,
                        EventCode::EndOfMarketHours
                            | EventCode::EndOfSystemHours
                            | EventCode::EndOfMessages
                    )
                {
                    self.check_pending_crossings(idx);
                }
            }
            Message::StockDirectory(m) => {
                self.symbols.insert(header.stock_locate, *m.stock);
            }
            Message::AddOrder(m) => {
                // A mis-decoded offset in either "R" or "A"/"F" would make
                // the add's symbol disagree with the directory.
                if let Some(dir) = self.symbols.get(&header.stock_locate)
                    && dir != m.stock
                {
                    self.record(
                        idx,
                        ViolationKind::SymbolMismatch {
                            locate: header.stock_locate,
                        },
                    );
                }
            }
            Message::Trade(m) => {
                self.stats.trades += 1;
                self.stats.trade_shares += u64::from(m.shares);
                let sym = self.per_symbol.entry(header.stock_locate).or_default();
                sym.trades += 1;
                sym.trade_shares += u64::from(m.shares);
            }
            Message::CrossTrade(m) => {
                self.stats.cross_trades += 1;
                self.stats.cross_shares += m.shares;
            }
            _ => {}
        }

        let effect = match self.market.apply(&msg) {
            Ok(effect) => effect,
            Err(e) => {
                self.record(idx, ViolationKind::Book(e));
                return;
            }
        };

        if let Message::SystemEvent(m) = &msg {
            self.system_events.push((m.event_code, header.timestamp));
        }

        match effect {
            Effect::None => {}
            Effect::Added { .. } => {
                if let Message::AddOrder(m) = &msg {
                    self.stats.orders_added += 1;
                    self.stats.shares_added += u64::from(m.shares);
                }
                self.per_symbol.entry(header.stock_locate).or_default().adds += 1;
            }
            Effect::Executed {
                executed,
                remaining,
                ..
            } => {
                match &msg {
                    Message::OrderExecuted(_) => {
                        self.stats.exec_events += 1;
                        self.stats.exec_shares += u64::from(executed);
                    }
                    Message::OrderExecutedWithPrice(m) => {
                        self.stats.exec_with_price_events += 1;
                        self.stats.exec_with_price_shares += u64::from(executed);
                        if m.printable == b'N' {
                            self.stats.exec_nonprintable += 1;
                        }
                    }
                    _ => {}
                }
                if remaining == 0 {
                    self.stats.orders_filled += 1;
                }
                let sym = self.per_symbol.entry(header.stock_locate).or_default();
                sym.exec_events += 1;
                sym.exec_shares += u64::from(executed);
            }
            Effect::Cancelled {
                cancelled,
                remaining,
                ..
            } => {
                self.stats.cancel_events += 1;
                self.stats.cancelled_shares += u64::from(cancelled);
                if remaining == 0 {
                    self.stats.orders_cancelled_out += 1;
                }
                self.per_symbol
                    .entry(header.stock_locate)
                    .or_default()
                    .cancels += 1;
            }
            Effect::Deleted { remaining, .. } => {
                self.stats.deletes += 1;
                self.stats.deleted_shares += u64::from(remaining);
                self.per_symbol
                    .entry(header.stock_locate)
                    .or_default()
                    .deletes += 1;
            }
            Effect::Replaced { .. } => {
                self.stats.replaces += 1;
                self.per_symbol
                    .entry(header.stock_locate)
                    .or_default()
                    .replaces += 1;
            }
        }

        // Crossed-book invariant. Book mutations can cross only the touched
        // book; phase transitions ("Q" system event, a stock resuming
        // trading) can put an already-crossed book in scope, so sweep those
        // moments too.
        match (&msg, effect) {
            (Message::SystemEvent(_), _) => {
                let locates: Vec<u16> = self.market.books().map(|(l, _)| l).collect();
                for locate in locates {
                    self.check_crossed(idx, locate);
                }
            }
            (Message::StockTradingAction(_), _) => {
                self.check_crossed(idx, header.stock_locate);
            }
            (_, Effect::None) => {}
            _ => self.check_crossed(idx, header.stock_locate),
        }

        // Peaks.
        if effect != Effect::None {
            let live = self.market.live_orders();
            if live > self.stats.peak_live_orders {
                self.stats.peak_live_orders = live;
                self.stats.peak_live_orders_at = idx;
            }
            if let Some(book) = self.market.book(header.stock_locate) {
                let count = book.order_count();
                let sym = self.per_symbol.entry(header.stock_locate).or_default();
                if count > sym.peak_live_orders {
                    sym.peak_live_orders = count;
                }
                if count > self.stats.peak_book_orders {
                    self.stats.peak_book_orders = count;
                    self.stats.peak_book_orders_locate = header.stock_locate;
                }
                for side in [crate::parse::Side::Buy, crate::parse::Side::Sell] {
                    let levels = book.level_count(side);
                    if levels > self.stats.peak_side_levels {
                        self.stats.peak_side_levels = levels;
                        self.stats.peak_side_levels_locate = header.stock_locate;
                    }
                }
            }
        }

        if self.verify_every > 0
            && self.stats.messages.is_multiple_of(self.verify_every)
            && let Err(e) = self.market.verify()
        {
            self.record(idx, ViolationKind::Consistency(e));
        }
    }

    /// Runs the final deep consistency check (and, if the stream ended with
    /// market hours still in force, the persistent-crossing backstop). Call
    /// once after the last [`feed`](Self::feed).
    pub fn finish(&mut self) {
        let idx = self.stats.messages.saturating_sub(1);
        if self.market.market_hours() {
            self.check_pending_crossings(idx);
        }
        if let Err(e) = self.market.verify() {
            self.record(idx, ViolationKind::Consistency(e));
        }
    }

    /// True when the replay saw no violations of any kind.
    pub fn clean(&self) -> bool {
        self.stats.violations() == 0
    }
}
