//! Property-based tests.
//!
//! 1. `decode` never panics, whatever bytes arrive.
//! 2. Encode/decode round-trips for the order-flow message types over
//!    random field values (the encoder here restates the spec layouts
//!    independently of `parse::decode`).
//! 3. Random valid-and-invalid operation sequences against a naive
//!    reference model: the book must agree with the model exactly, stay
//!    deeply self-consistent after every step, and reject invalid
//!    operations without mutating anything.

use std::collections::HashMap;

use limitbook_core::book::{BookError, Market};
use limitbook_core::parse::{
    AddOrder, Header, Message, OrderCancel, OrderDelete, OrderExecuted, OrderReplace, Price4, Side,
    decode,
};
use proptest::prelude::*;

fn enc_header(ty: u8, h: &Header) -> Vec<u8> {
    let mut v = vec![ty];
    v.extend_from_slice(&h.stock_locate.to_be_bytes());
    v.extend_from_slice(&h.tracking_number.to_be_bytes());
    v.extend_from_slice(&h.timestamp.to_be_bytes()[2..8]);
    v
}

fn header_strategy() -> impl Strategy<Value = Header> {
    (any::<u16>(), any::<u16>(), 0u64..(1 << 48)).prop_map(|(l, t, ts)| Header {
        stock_locate: l,
        tracking_number: t,
        timestamp: ts,
    })
}

fn side_strategy() -> impl Strategy<Value = (u8, Side)> {
    prop_oneof![Just((b'B', Side::Buy)), Just((b'S', Side::Sell))]
}

proptest! {
    /// Arbitrary input never panics: typed error or typed message, always.
    #[test]
    fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..80)) {
        let _ = decode(&bytes);
    }

    /// Frame-plausible but corrupt input (valid type byte, wrong length,
    /// arbitrary tail) never panics either.
    #[test]
    fn decode_never_panics_on_known_types(
        ty in proptest::sample::select(&b"SRHYLVWKJhAFECXDUPQBINO"[..]),
        tail in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        let mut bytes = vec![ty];
        bytes.extend_from_slice(&tail);
        let _ = decode(&bytes);
    }

    /// Add Order ("A"/"F") round-trips: spec §1.3 field order.
    #[test]
    fn roundtrip_add_order(
        h in header_strategy(),
        oref in any::<u64>(),
        (side_byte, side) in side_strategy(),
        shares in any::<u32>(),
        stock in proptest::array::uniform8(any::<u8>()),
        price in any::<u32>(),
        attributed in any::<bool>(),
        mpid in proptest::array::uniform4(any::<u8>()),
    ) {
        let mut v = enc_header(if attributed { b'F' } else { b'A' }, &h);
        v.extend_from_slice(&oref.to_be_bytes());
        v.push(side_byte);
        v.extend_from_slice(&shares.to_be_bytes());
        v.extend_from_slice(&stock);
        v.extend_from_slice(&price.to_be_bytes());
        if attributed {
            v.extend_from_slice(&mpid);
        }
        prop_assert_eq!(
            decode(&v).unwrap(),
            Message::AddOrder(AddOrder {
                header: h,
                order_ref: oref,
                side,
                shares,
                stock: &stock,
                price: Price4(price),
                attribution: if attributed { Some(&mpid) } else { None },
            })
        );
    }

    /// Executed / Cancel / Delete / Replace round-trip: spec §1.4.
    #[test]
    fn roundtrip_modify_messages(
        h in header_strategy(),
        oref in any::<u64>(),
        qty in any::<u32>(),
        matchno in any::<u64>(),
        new_ref in any::<u64>(),
        price in any::<u32>(),
    ) {
        let mut v = enc_header(b'E', &h);
        v.extend_from_slice(&oref.to_be_bytes());
        v.extend_from_slice(&qty.to_be_bytes());
        v.extend_from_slice(&matchno.to_be_bytes());
        prop_assert_eq!(
            decode(&v).unwrap(),
            Message::OrderExecuted(OrderExecuted {
                header: h,
                order_ref: oref,
                executed_shares: qty,
                match_number: matchno,
            })
        );

        let mut v = enc_header(b'X', &h);
        v.extend_from_slice(&oref.to_be_bytes());
        v.extend_from_slice(&qty.to_be_bytes());
        prop_assert_eq!(
            decode(&v).unwrap(),
            Message::OrderCancel(OrderCancel {
                header: h,
                order_ref: oref,
                cancelled_shares: qty,
            })
        );

        let mut v = enc_header(b'D', &h);
        v.extend_from_slice(&oref.to_be_bytes());
        prop_assert_eq!(
            decode(&v).unwrap(),
            Message::OrderDelete(OrderDelete { header: h, order_ref: oref })
        );

        let mut v = enc_header(b'U', &h);
        v.extend_from_slice(&oref.to_be_bytes());
        v.extend_from_slice(&new_ref.to_be_bytes());
        v.extend_from_slice(&qty.to_be_bytes());
        v.extend_from_slice(&price.to_be_bytes());
        prop_assert_eq!(
            decode(&v).unwrap(),
            Message::OrderReplace(OrderReplace {
                header: h,
                original_order_ref: oref,
                new_order_ref: new_ref,
                shares: qty,
                price: Price4(price),
            })
        );
    }
}

/// One raw random operation, interpreted against the current model state.
#[derive(Debug, Clone)]
struct RawOp {
    kind: u8,
    a: u32,
    b: u32,
    locate: u16,
}

fn raw_op_strategy() -> impl Strategy<Value = RawOp> {
    (0u8..7, any::<u32>(), any::<u32>(), 0u16..3).prop_map(|(kind, a, b, locate)| RawOp {
        kind,
        a,
        b,
        locate,
    })
}

#[derive(Debug, Clone, Copy)]
struct ModelOrder {
    locate: u16,
    side: Side,
    price: u32,
    shares: u32,
}

fn hdr(locate: u16) -> Header {
    Header {
        stock_locate: locate,
        tracking_number: 0,
        timestamp: 0,
    }
}

fn add_msg(locate: u16, oref: u64, side: Side, shares: u32, price: u32) -> Message<'static> {
    Message::AddOrder(AddOrder {
        header: hdr(locate),
        order_ref: oref,
        side,
        shares,
        stock: b"PROP    ",
        price: Price4(price),
        attribution: None,
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Random operation sequences: Market must agree with a naive model,
    /// stay self-consistent after every step, and treat every invalid
    /// operation as a rejected no-op.
    #[test]
    fn book_matches_reference_model(ops in proptest::collection::vec(raw_op_strategy(), 1..150)) {
        let mut market = Market::new();
        let mut model: HashMap<u64, ModelOrder> = HashMap::new();
        let mut live: Vec<u64> = Vec::new(); // insertion-ordered live ids
        let mut next_id: u64 = 1;

        for op in &ops {
            match op.kind {
                // Add (buy or sell by parity of `a`). Small price space so
                // levels get shared and pruning paths run.
                0 | 1 => {
                    let side = if op.kind == 0 { Side::Buy } else { Side::Sell };
                    let shares = op.a % 1_000 + 1;
                    let price = op.b % 40 + 1;
                    let id = next_id;
                    next_id += 1;
                    let effect = market.apply(&add_msg(op.locate, id, side, shares, price));
                    prop_assert!(effect.is_ok());
                    model.insert(id, ModelOrder { locate: op.locate, side, price, shares });
                    live.push(id);
                }
                // Execute against a live order; sometimes deliberately
                // overfilled.
                2 if !live.is_empty() => {
                    let id = live[op.a as usize % live.len()];
                    let m = model[&id];
                    let qty = op.b % (m.shares + m.shares / 2 + 2) + 1;
                    let msg = Message::OrderExecuted(OrderExecuted {
                        header: hdr(m.locate),
                        order_ref: id,
                        executed_shares: qty,
                        match_number: 0,
                    });
                    if qty > m.shares {
                        let before = market.clone();
                        prop_assert_eq!(
                            market.apply(&msg),
                            Err(BookError::Overfill { order_ref: id, resting: m.shares, requested: qty })
                        );
                        prop_assert_eq!(&market, &before);
                    } else {
                        prop_assert!(market.apply(&msg).is_ok());
                        if qty == m.shares {
                            model.remove(&id);
                            live.retain(|&x| x != id);
                        } else {
                            model.get_mut(&id).unwrap().shares -= qty;
                        }
                    }
                }
                // Partial cancel, sometimes overcancelled.
                3 if !live.is_empty() => {
                    let id = live[op.a as usize % live.len()];
                    let m = model[&id];
                    let qty = op.b % (m.shares + m.shares / 2 + 2) + 1;
                    let msg = Message::OrderCancel(OrderCancel {
                        header: hdr(m.locate),
                        order_ref: id,
                        cancelled_shares: qty,
                    });
                    if qty > m.shares {
                        let before = market.clone();
                        prop_assert_eq!(
                            market.apply(&msg),
                            Err(BookError::Overcancel { order_ref: id, resting: m.shares, requested: qty })
                        );
                        prop_assert_eq!(&market, &before);
                    } else {
                        prop_assert!(market.apply(&msg).is_ok());
                        if qty == m.shares {
                            model.remove(&id);
                            live.retain(|&x| x != id);
                        } else {
                            model.get_mut(&id).unwrap().shares -= qty;
                        }
                    }
                }
                // Delete a live order.
                4 if !live.is_empty() => {
                    let id = live[op.a as usize % live.len()];
                    let m = model[&id];
                    let msg = Message::OrderDelete(OrderDelete {
                        header: hdr(m.locate),
                        order_ref: id,
                    });
                    prop_assert!(market.apply(&msg).is_ok());
                    model.remove(&id);
                    live.retain(|&x| x != id);
                }
                // Replace a live order: same locate and side, fresh id.
                5 if !live.is_empty() => {
                    let id = live[op.a as usize % live.len()];
                    let m = model[&id];
                    let new_id = next_id;
                    next_id += 1;
                    let shares = op.b % 1_000 + 1;
                    let price = (op.b >> 16) % 40 + 1;
                    let msg = Message::OrderReplace(OrderReplace {
                        header: hdr(m.locate),
                        original_order_ref: id,
                        new_order_ref: new_id,
                        shares,
                        price: Price4(price),
                    });
                    prop_assert!(market.apply(&msg).is_ok());
                    model.remove(&id);
                    live.retain(|&x| x != id);
                    model.insert(new_id, ModelOrder { locate: m.locate, side: m.side, price, shares });
                    live.push(new_id);
                }
                // Mutate a reference number that was never allocated: must
                // be rejected without any state change.
                6 => {
                    let ghost = next_id + 1_000_000;
                    let before = market.clone();
                    let msg = Message::OrderDelete(OrderDelete {
                        header: hdr(op.locate),
                        order_ref: ghost,
                    });
                    prop_assert_eq!(
                        market.apply(&msg),
                        Err(BookError::UnknownOrder { order_ref: ghost })
                    );
                    prop_assert_eq!(&market, &before);
                }
                _ => {} // op needed a live order and none exists: skip
            }
            market.verify().expect("book self-consistent after every op");
        }

        // Final reconciliation with the model.
        prop_assert_eq!(market.live_orders(), model.len());
        for (&id, m) in &model {
            let order = market.order(id).expect("model order is live in market");
            prop_assert_eq!(order.stock_locate, m.locate);
            prop_assert_eq!(order.side, m.side);
            prop_assert_eq!(order.price, Price4(m.price));
            prop_assert_eq!(order.shares, m.shares);
        }
        // Aggregate level shares per (locate, side, price) must match.
        let mut level_shares: HashMap<(u16, u8, u32), u64> = HashMap::new();
        for m in model.values() {
            let side_key = if m.side == Side::Buy { 0 } else { 1 };
            *level_shares.entry((m.locate, side_key, m.price)).or_default() +=
                u64::from(m.shares);
        }
        for (&(locate, side_key, price), &shares) in &level_shares {
            let side = if side_key == 0 { Side::Buy } else { Side::Sell };
            let book = market.book(locate).expect("book exists");
            prop_assert_eq!(book.shares_at(side, Price4(price)), Some(shares));
        }
        // Best bid/ask per locate must equal the model's extremes.
        for locate in 0u16..3 {
            let best_bid = model
                .values()
                .filter(|m| m.locate == locate && m.side == Side::Buy)
                .map(|m| m.price)
                .max();
            let best_ask = model
                .values()
                .filter(|m| m.locate == locate && m.side == Side::Sell)
                .map(|m| m.price)
                .min();
            if let Some(book) = market.book(locate) {
                prop_assert_eq!(book.best_bid().map(|(p, _)| p.0), best_bid);
                prop_assert_eq!(book.best_ask().map(|(p, _)| p.0), best_ask);
            } else {
                prop_assert!(best_bid.is_none() && best_ask.is_none());
            }
        }
    }
}
