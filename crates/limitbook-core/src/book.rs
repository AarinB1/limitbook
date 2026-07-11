//! Limit order book reconstruction from decoded ITCH 5.0 messages.
//!
//! [`Market`] tracks every live displayable order (keyed by the day-unique
//! order reference number) and one price-level [`Book`] per stock locate.
//! Levels keep their orders in arrival order, so the book models full
//! price-time priority: new orders join the back of their price level, and
//! a replace ("U") re-enters at the back — queue priority resets — while a
//! partial cancel ("X") keeps its place.
//!
//! Message semantics follow `spec/itch50_spec.txt` §1.3–§1.4:
//!
//! - "A"/"F" add a new order;
//! - "E"/"C" execute against the resting order — effects are cumulative and
//!   the book impact of both is the same deduction ("C" merely reports a
//!   different execution price for the tape);
//! - "X" partially cancels; "D" removes outright;
//! - "U" retires the original reference number and adds the new one with
//!   the new shares and price, inheriting side and stock from the original;
//! - per §1.4, whenever remaining shares reach zero the order is dead and
//!   is removed from the book.
//!
//! Every mutation is validated in full before any state changes ([`apply`]
//! is atomic: on `Err` the market is untouched). The CLAUDE.md invariants
//! are enforced structurally: executions and cancels beyond resting shares
//! and mutations of unknown orders are typed [`BookError`]s, and the
//! crossed-book invariant is checked via [`Market::cross_violation`].
//! [`Market::verify`] re-derives all cached aggregates from first
//! principles for use as a deep self-check during replay and tests.
//!
//! # Crossed-book invariant scope
//!
//! "Best bid < best ask" holds only while a security is genuinely in
//! continuous trading, and the feed makes the boundary subtle. Validated
//! against the full 2019-12-30 sample day (268.7M messages):
//!
//! - Pre-open and halted books legitimately cross (quotation-only periods
//!   accumulate crossing displayable orders for the reopening auction), so
//!   scope requires market hours plus a Trading state.
//! - Nasdaq serializes a reopening cross over many messages and emits the
//!   "T" (released for trading) action *mid-unwind* — observed ordering:
//!   non-printable "C" executions at the cross price, the "Q" cross print,
//!   the "H"/"T" release, then more "C" executions, microseconds apart. The
//!   displayed book legitimately stays crossed until the unwind finishes,
//!   so any trading-state transition disarms the invariant and it re-arms
//!   at the first uncrossed sighting. A book that never re-arms is real
//!   trouble and is surfaced by [`Market::cross_check_pending`] (the replay
//!   engine reports any still pending when market hours end). Every one of
//!   the 664 crossings the naive scope flagged on the sample day sits in
//!   such a window; zero survive with arming, and zero occur elsewhere.
//! - Stock Directory entries with Authenticity "T" (spec §1.2.1) are Nasdaq
//!   test instruments (e.g. ZJZZT); market invariants do not bind them and
//!   they are exempt.
//!
//! [`apply`]: Market::apply

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};

use hashbrown::{HashMap, HashSet};

use crate::parse::{EventCode, Message, Price4, Side, TradingState};

/// A live resting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Order {
    pub stock_locate: u16,
    pub side: Side,
    pub price: Price4,
    /// Remaining (display) shares; always > 0 for a live order.
    pub shares: u32,
}

/// One price level: orders in time priority plus the cached share total.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Level {
    /// Sum of remaining shares of the queued orders (u64: a level can hold
    /// many u32-sized orders).
    shares: u64,
    /// Order reference numbers in time priority (front = first in line).
    queue: VecDeque<u64>,
}

/// Bid and ask price levels for one stock locate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Book {
    /// Best bid = highest key.
    bids: BTreeMap<Price4, Level>,
    /// Best ask = lowest key.
    asks: BTreeMap<Price4, Level>,
    /// Cached count of live orders in this book (verified by
    /// [`Market::verify`]).
    order_count: usize,
}

impl Book {
    fn levels(&self, side: Side) -> &BTreeMap<Price4, Level> {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    fn levels_mut(&mut self, side: Side) -> &mut BTreeMap<Price4, Level> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    /// Highest bid as (price, total shares at that level).
    pub fn best_bid(&self) -> Option<(Price4, u64)> {
        self.bids.last_key_value().map(|(p, l)| (*p, l.shares))
    }

    /// Lowest ask as (price, total shares at that level).
    pub fn best_ask(&self) -> Option<(Price4, u64)> {
        self.asks.first_key_value().map(|(p, l)| (*p, l.shares))
    }

    /// Number of price levels on one side.
    pub fn level_count(&self, side: Side) -> usize {
        self.levels(side).len()
    }

    /// Live orders resting in this book.
    pub fn order_count(&self) -> usize {
        self.order_count
    }

    /// Total shares at `price` on `side`, if that level exists.
    pub fn shares_at(&self, side: Side, price: Price4) -> Option<u64> {
        self.levels(side).get(&price).map(|l| l.shares)
    }

    /// Order reference numbers at `price` on `side` in time priority
    /// (first in line first). Empty if the level does not exist.
    pub fn orders_at(&self, side: Side, price: Price4) -> impl Iterator<Item = u64> + '_ {
        self.levels(side)
            .get(&price)
            .into_iter()
            .flat_map(|l| l.queue.iter().copied())
    }

    /// Price levels on `side` as (price, total shares, order count), from
    /// best to worst (descending bids, ascending asks).
    pub fn side_levels(&self, side: Side) -> impl Iterator<Item = (Price4, u64, usize)> + '_ {
        let levels = self.levels(side);
        let fwd = matches!(side, Side::Sell);
        let mut iter = levels.iter();
        core::iter::from_fn(move || {
            let (p, l) = if fwd { iter.next() } else { iter.next_back() }?;
            Some((*p, l.shares, l.queue.len()))
        })
    }
}

/// What a successfully applied message did to the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// No book mutation (system events, directory, trades, informational).
    None,
    /// "A"/"F": the order joined the back of its price level.
    Added { order_ref: u64 },
    /// "E"/"C": `executed` shares came off; `remaining == 0` means the
    /// order died and left the book.
    Executed {
        order_ref: u64,
        executed: u32,
        remaining: u32,
    },
    /// "X": `cancelled` shares came off; `remaining == 0` means the order
    /// died and left the book (spec §1.4: at zero display shares the order
    /// is dead).
    Cancelled {
        order_ref: u64,
        cancelled: u32,
        remaining: u32,
    },
    /// "D": the order left the book with `remaining` shares still unfilled.
    Deleted { order_ref: u64, remaining: u32 },
    /// "U": `original` retired (with `prior_remaining` shares), `new` added
    /// at the back of its level — queue priority reset.
    Replaced {
        original: u64,
        new: u64,
        prior_remaining: u32,
    },
}

/// A message that cannot be applied to the current book state. `apply`
/// returns these without mutating anything. On a well-formed feed none of
/// these occur; each one is a real data or logic problem ("dangling
/// reference" = `UnknownOrder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookError {
    /// "E"/"C"/"X"/"D"/"U" referenced an order reference number that is not
    /// live (never added, already dead, or already replaced).
    UnknownOrder { order_ref: u64 },
    /// "A"/"F" (or the new side of "U") reused a live order reference
    /// number; reference numbers are day-unique.
    DuplicateOrder { order_ref: u64 },
    /// An add or replace carried zero shares — a displayable order cannot
    /// rest with no size.
    ZeroShares { order_ref: u64 },
    /// An add or replace carried price zero — no displayable order rests
    /// at 0.0000.
    ZeroPrice { order_ref: u64 },
    /// An execution or cancel for zero shares — meaningless and treated as
    /// corrupt data.
    ZeroQuantity { order_ref: u64 },
    /// "E"/"C" executed more shares than the order has resting — violates
    /// the invariant that executed shares never exceed the resting order's
    /// shares.
    Overfill {
        order_ref: u64,
        resting: u32,
        requested: u32,
    },
    /// "X" cancelled more shares than the order has resting.
    Overcancel {
        order_ref: u64,
        resting: u32,
        requested: u32,
    },
    /// The message's stock locate disagrees with the locate the order was
    /// added under — the reference number matched a different security.
    LocateMismatch {
        order_ref: u64,
        order_locate: u16,
        message_locate: u16,
    },
}

/// An inconsistency found by [`Market::verify`]: a cached aggregate
/// disagrees with the ground truth re-derived from the order map and level
/// queues. Any of these is a bug in this module, not in the feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsistencyError {
    /// A price level exists with no queued orders.
    EmptyLevel {
        locate: u16,
        side: Side,
        price: Price4,
    },
    /// A level's cached share total differs from the sum over its queue.
    LevelShareMismatch {
        locate: u16,
        side: Side,
        price: Price4,
        cached: u64,
        actual: u64,
    },
    /// A queued order reference number has no entry in the order map.
    QueuedOrderMissing { order_ref: u64 },
    /// A queued order's stored (locate, side, price) disagrees with the
    /// level it is queued in.
    QueuedOrderMisfiled { order_ref: u64 },
    /// A live order has zero remaining shares (should have been removed).
    ZeroShareOrder { order_ref: u64 },
    /// An order reference number appears more than once across all queues.
    DuplicateQueued { order_ref: u64 },
    /// A book's cached order count differs from its queued total.
    BookCountMismatch {
        locate: u16,
        cached: usize,
        actual: usize,
    },
    /// Orders exist in the order map that are queued in no level.
    UnqueuedOrders {
        count: usize,
        example_order_ref: u64,
    },
}

/// All books plus the order index and the trading-phase state needed to
/// scope the crossed-book invariant.
///
/// `PartialEq` compares full book state; tests use it to prove that a
/// rejected message left the market untouched.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Market {
    /// Every live order, keyed by day-unique order reference number.
    orders: HashMap<u64, Order>,
    /// Books keyed by stock locate. BTreeMap so all reporting iteration is
    /// deterministic (the hash maps here are never iterated for output).
    books: BTreeMap<u16, Book>,
    /// True between the "Q" and "M"/"E"/"C" system events (spec §1.1).
    market_hours: bool,
    /// Last Stock Trading Action state per locate. A security absent from
    /// the pre-opening spin is treated as halted (spec §1.2.2).
    trading_state: BTreeMap<u16, TradingState>,
    /// Locates whose Stock Directory entry carries Authenticity "T" — Nasdaq
    /// test instruments (spec §1.2.1), exempt from market invariants.
    test_securities: BTreeSet<u16>,
    /// Locates whose cross invariant is armed: the book has been observed
    /// uncrossed in continuous trading since the stock's last trading-state
    /// transition. Nasdaq releases a halted stock ("T" action) while the
    /// reopening cross is still being serialized onto the feed (observed on
    /// the 2019-12-30 sample day: C executions -> Q cross print -> H "T"
    /// action -> further C executions, all within microseconds), so a book
    /// can legitimately sit crossed briefly after release. Arming starts
    /// enforcement at the first uncrossed sighting; a book that NEVER
    /// re-arms before market close is surfaced by
    /// [`Market::cross_check_pending`].
    armed: BTreeSet<u16>,
}

impl Market {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one decoded message. Order-flow messages mutate the book;
    /// "S"/"H"/"R" update trading-phase and scope state; everything else is
    /// a no-op. Atomic: on `Err` no state changed.
    pub fn apply(&mut self, msg: &Message<'_>) -> Result<Effect, BookError> {
        let effect = match msg {
            Message::SystemEvent(m) => {
                match m.event_code {
                    EventCode::StartOfMarketHours => self.market_hours = true,
                    // "M" ends market hours; "E"/"C" end the day outright.
                    EventCode::EndOfMarketHours
                    | EventCode::EndOfSystemHours
                    | EventCode::EndOfMessages => {
                        self.market_hours = false;
                        self.armed.clear();
                    }
                    _ => {}
                }
                // Phase changes move every book in or out of cross-invariant
                // scope; re-evaluate arming across the board (a handful of
                // system events per day).
                let locates: alloc::vec::Vec<u16> = self.books.keys().copied().collect();
                for locate in locates {
                    self.refresh_armed(locate);
                }
                Effect::None
            }
            Message::StockDirectory(m) => {
                // Authenticity "T" marks Nasdaq test instruments
                // (spec §1.2.1); market invariants do not apply to them.
                if m.authenticity == b'T' {
                    self.test_securities.insert(m.header.stock_locate);
                }
                Effect::None
            }
            Message::StockTradingAction(m) => {
                let locate = m.header.stock_locate;
                self.trading_state.insert(locate, m.trading_state);
                // Any trading-state transition disarms the cross invariant:
                // Nasdaq releases a stock for trading ("T") while the
                // reopening cross is still being serialized, so the book may
                // legitimately stay crossed for a moment after release. The
                // invariant re-arms at the first uncrossed sighting below.
                self.armed.remove(&locate);
                self.refresh_armed(locate);
                Effect::None
            }
            Message::AddOrder(m) => {
                self.validate_new(m.order_ref, m.shares, m.price)?;
                self.insert_order(
                    m.header.stock_locate,
                    m.order_ref,
                    m.side,
                    m.shares,
                    m.price,
                );
                Effect::Added {
                    order_ref: m.order_ref,
                }
            }
            Message::OrderExecuted(m) => self.reduce(
                m.header.stock_locate,
                m.order_ref,
                m.executed_shares,
                Reduction::Execute,
            )?,
            Message::OrderExecutedWithPrice(m) => self.reduce(
                m.header.stock_locate,
                m.order_ref,
                m.executed_shares,
                Reduction::Execute,
            )?,
            Message::OrderCancel(m) => self.reduce(
                m.header.stock_locate,
                m.order_ref,
                m.cancelled_shares,
                Reduction::Cancel,
            )?,
            Message::OrderDelete(m) => {
                let order = self.validate_live(m.header.stock_locate, m.order_ref)?;
                let remaining = order.shares;
                self.remove_order(m.order_ref);
                Effect::Deleted {
                    order_ref: m.order_ref,
                    remaining,
                }
            }
            Message::OrderReplace(m) => {
                // Validate everything before touching state (atomicity).
                let original = self.validate_live(m.header.stock_locate, m.original_order_ref)?;
                let side = original.side;
                let prior_remaining = original.shares;
                self.validate_new(m.new_order_ref, m.shares, m.price)?;
                self.remove_order(m.original_order_ref);
                // Re-inserting at the back of the (new) price level is the
                // §1.4.5 queue-priority reset.
                self.insert_order(
                    m.header.stock_locate,
                    m.new_order_ref,
                    side,
                    m.shares,
                    m.price,
                );
                Effect::Replaced {
                    original: m.original_order_ref,
                    new: m.new_order_ref,
                    prior_remaining,
                }
            }
            // Trades (P/Q/B) and informational messages do not change the
            // displayed book (spec §1.5: "Trade Messages do not affect the
            // book").
            _ => Effect::None,
        };
        if effect != Effect::None {
            // A book mutation may have uncrossed the book; re-arm eagerly so
            // real crossings are caught from the next event onward.
            self.refresh_armed(msg.header().stock_locate);
        }
        Ok(effect)
    }

    /// Arms the cross invariant for a locate the moment its book is seen
    /// uncrossed while in continuous trading. Called after every event that
    /// could change scope or book shape.
    fn refresh_armed(&mut self, locate: u16) {
        if self.market_hours
            && self.trading_state(locate) == TradingState::Trading
            && !self.crossed(locate)
        {
            self.armed.insert(locate);
        }
    }

    /// Checks an add/replace's new order: unused reference number, nonzero
    /// shares, nonzero price.
    fn validate_new(&self, order_ref: u64, shares: u32, price: Price4) -> Result<(), BookError> {
        if shares == 0 {
            return Err(BookError::ZeroShares { order_ref });
        }
        if price.0 == 0 {
            return Err(BookError::ZeroPrice { order_ref });
        }
        if self.orders.contains_key(&order_ref) {
            return Err(BookError::DuplicateOrder { order_ref });
        }
        Ok(())
    }

    /// Checks that `order_ref` is live and belongs to the message's locate.
    fn validate_live(&self, message_locate: u16, order_ref: u64) -> Result<Order, BookError> {
        let order = *self
            .orders
            .get(&order_ref)
            .ok_or(BookError::UnknownOrder { order_ref })?;
        if order.stock_locate != message_locate {
            return Err(BookError::LocateMismatch {
                order_ref,
                order_locate: order.stock_locate,
                message_locate,
            });
        }
        Ok(order)
    }

    /// Inserts a validated order at the back of its price level.
    fn insert_order(
        &mut self,
        locate: u16,
        order_ref: u64,
        side: Side,
        shares: u32,
        price: Price4,
    ) {
        self.orders.insert(
            order_ref,
            Order {
                stock_locate: locate,
                side,
                price,
                shares,
            },
        );
        let book = self.books.entry(locate).or_default();
        let level = book.levels_mut(side).entry(price).or_default();
        level.queue.push_back(order_ref);
        level.shares += u64::from(shares);
        book.order_count += 1;
    }

    /// Removes a live order from the order map and its level queue,
    /// dropping the level when it empties. The order must exist.
    fn remove_order(&mut self, order_ref: u64) {
        let Some(order) = self.orders.remove(&order_ref) else {
            debug_assert!(false, "remove_order on non-live order {order_ref}");
            return;
        };
        let Some(book) = self.books.get_mut(&order.stock_locate) else {
            debug_assert!(false, "order {order_ref} has no book");
            return;
        };
        let mut dequeued = false;
        {
            let levels = book.levels_mut(order.side);
            let Some(level) = levels.get_mut(&order.price) else {
                debug_assert!(false, "order {order_ref} has no level");
                return;
            };
            // Time-priority queues are only ever scanned here; correctness
            // first, cleverness in the phase-3 optimization pass.
            if let Some(pos) = level.queue.iter().position(|&r| r == order_ref) {
                level.queue.remove(pos);
                level.shares -= u64::from(order.shares);
                dequeued = true;
            } else {
                debug_assert!(false, "order {order_ref} not queued at its level");
            }
            if level.queue.is_empty() {
                levels.remove(&order.price);
            }
        }
        if dequeued {
            book.order_count -= 1;
        }
    }

    /// Shared implementation of "E"/"C" (execute) and "X" (partial cancel):
    /// deduct shares, removing the order when it reaches zero.
    fn reduce(
        &mut self,
        message_locate: u16,
        order_ref: u64,
        quantity: u32,
        kind: Reduction,
    ) -> Result<Effect, BookError> {
        if quantity == 0 {
            return Err(BookError::ZeroQuantity { order_ref });
        }
        let order = self.validate_live(message_locate, order_ref)?;
        if quantity > order.shares {
            return Err(match kind {
                Reduction::Execute => BookError::Overfill {
                    order_ref,
                    resting: order.shares,
                    requested: quantity,
                },
                Reduction::Cancel => BookError::Overcancel {
                    order_ref,
                    resting: order.shares,
                    requested: quantity,
                },
            });
        }
        let remaining = order.shares - quantity;
        if remaining == 0 {
            self.remove_order(order_ref);
        } else {
            // Deduct in place: a partial execution or cancel keeps the
            // order's queue position.
            self.orders
                .get_mut(&order_ref)
                .expect("validated live above")
                .shares = remaining;
            let level = self
                .books
                .get_mut(&order.stock_locate)
                .and_then(|b| b.levels_mut(order.side).get_mut(&order.price))
                .expect("live order has a level");
            level.shares -= u64::from(quantity);
        }
        Ok(match kind {
            Reduction::Execute => Effect::Executed {
                order_ref,
                executed: quantity,
                remaining,
            },
            Reduction::Cancel => Effect::Cancelled {
                order_ref,
                cancelled: quantity,
                remaining,
            },
        })
    }

    /// The book for a locate, if any order was ever added under it.
    pub fn book(&self, locate: u16) -> Option<&Book> {
        self.books.get(&locate)
    }

    /// All books with at least one order ever added, in locate order.
    pub fn books(&self) -> impl Iterator<Item = (u16, &Book)> {
        self.books.iter().map(|(l, b)| (*l, b))
    }

    /// A live order by reference number.
    pub fn order(&self, order_ref: u64) -> Option<&Order> {
        self.orders.get(&order_ref)
    }

    /// Total live orders across all books.
    pub fn live_orders(&self) -> usize {
        self.orders.len()
    }

    /// True while the system is inside market hours ("Q" seen, no
    /// "M"/"E"/"C" yet).
    pub fn market_hours(&self) -> bool {
        self.market_hours
    }

    /// Last announced trading state for a locate; securities absent from
    /// the Trading Action spin count as halted (spec §1.2.2).
    pub fn trading_state(&self, locate: u16) -> TradingState {
        self.trading_state
            .get(&locate)
            .copied()
            .unwrap_or(TradingState::Halted)
    }

    /// True while the system is in market hours and the security is in the
    /// Trading state. Outside this window (pre-open, halts) books
    /// legitimately cross. Note that enforcement of the crossed-book
    /// invariant additionally requires the locate to be *armed* — see
    /// [`Self::cross_violation`].
    pub fn in_continuous_trading(&self, locate: u16) -> bool {
        self.market_hours && self.trading_state(locate) == TradingState::Trading
    }

    /// True if the locate's Stock Directory entry marked it a test
    /// instrument (Authenticity "T", spec §1.2.1).
    pub fn is_test_security(&self, locate: u16) -> bool {
        self.test_securities.contains(&locate)
    }

    /// True when this locate's book violates the crossed-book invariant
    /// right now: crossed while in continuous trading, on a production
    /// (non-test) security, with the invariant armed (the book has been
    /// seen uncrossed since the last trading-state transition, so the
    /// reopening-cross serialization window is over).
    pub fn cross_violation(&self, locate: u16) -> bool {
        self.in_continuous_trading(locate)
            && !self.is_test_security(locate)
            && self.armed.contains(&locate)
            && self.crossed(locate)
    }

    /// True when this locate is crossed in continuous trading but the
    /// invariant has not re-armed since its last trading-state transition —
    /// the legitimate transient state while Nasdaq serializes a reopening
    /// cross around the "T" release action. It becomes a real violation
    /// only if it persists: callers should report any locate still pending
    /// when market hours end (see `Replay`).
    pub fn cross_check_pending(&self, locate: u16) -> bool {
        self.in_continuous_trading(locate)
            && !self.is_test_security(locate)
            && !self.armed.contains(&locate)
            && self.crossed(locate)
    }

    /// True if the locate's book is crossed or locked (best bid >= best
    /// ask). Only an invariant violation when [`Self::in_continuous_trading`].
    pub fn crossed(&self, locate: u16) -> bool {
        let Some(book) = self.books.get(&locate) else {
            return false;
        };
        match (book.best_bid(), book.best_ask()) {
            (Some((bid, _)), Some((ask, _))) => bid >= ask,
            _ => false,
        }
    }

    /// Re-derives every cached aggregate from the order map and level
    /// queues and cross-checks them: level share totals, book order
    /// counts, no empty levels, no zero-share or misfiled or duplicated
    /// queued orders, and every live order queued exactly once.
    pub fn verify(&self) -> Result<(), ConsistencyError> {
        let mut seen: HashSet<u64> = HashSet::with_capacity(self.orders.len());
        for (&locate, book) in &self.books {
            let mut book_total = 0usize;
            for side in [Side::Buy, Side::Sell] {
                for (&price, level) in book.levels(side) {
                    if level.queue.is_empty() {
                        return Err(ConsistencyError::EmptyLevel {
                            locate,
                            side,
                            price,
                        });
                    }
                    let mut actual = 0u64;
                    for &order_ref in &level.queue {
                        if !seen.insert(order_ref) {
                            return Err(ConsistencyError::DuplicateQueued { order_ref });
                        }
                        let Some(order) = self.orders.get(&order_ref) else {
                            return Err(ConsistencyError::QueuedOrderMissing { order_ref });
                        };
                        if order.stock_locate != locate
                            || order.side != side
                            || order.price != price
                        {
                            return Err(ConsistencyError::QueuedOrderMisfiled { order_ref });
                        }
                        if order.shares == 0 {
                            return Err(ConsistencyError::ZeroShareOrder { order_ref });
                        }
                        actual += u64::from(order.shares);
                    }
                    if actual != level.shares {
                        return Err(ConsistencyError::LevelShareMismatch {
                            locate,
                            side,
                            price,
                            cached: level.shares,
                            actual,
                        });
                    }
                    book_total += level.queue.len();
                }
            }
            if book_total != book.order_count {
                return Err(ConsistencyError::BookCountMismatch {
                    locate,
                    cached: book.order_count,
                    actual: book_total,
                });
            }
        }
        if seen.len() != self.orders.len() {
            let example_order_ref = self
                .orders
                .keys()
                .filter(|r| !seen.contains(*r))
                .min()
                .copied()
                .unwrap_or(0);
            return Err(ConsistencyError::UnqueuedOrders {
                count: self.orders.len() - seen.len(),
                example_order_ref,
            });
        }
        Ok(())
    }
}

/// Distinguishes the two share-deduction messages only for error naming.
#[derive(Clone, Copy)]
enum Reduction {
    Execute,
    Cancel,
}
