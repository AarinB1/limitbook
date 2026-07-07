//! Core library for `limitbook`: a zero-copy NASDAQ TotalView-ITCH 5.0 parser
//! and limit order book reconstructor.
//!
//! # Constraints
//!
//! This crate is pure computation over byte slices. It is `#![no_std]`, does
//! no file or OS I/O, and must compile for `wasm32-unknown-unknown` (CI
//! enforces this) so it can power a browser demo. All I/O — reading capture
//! files, decompression, output — belongs in `limitbook-cli`.
//!
//! # Source of truth
//!
//! Every message layout and field offset is taken from the committed spec
//! text at `spec/itch50_spec.txt`, never from memory.
//!
//! # Book invariants
//!
//! - Best bid < best ask during continuous trading.
//! - Executed shares never exceed the resting order's shares.
//! - Deletes, cancels, and replaces must reference a live order.

#![no_std]
#![deny(unsafe_code)]

// Book reconstruction needs heap collections; `alloc` keeps the crate
// no_std + wasm-clean (the wasm demo supplies a global allocator).
extern crate alloc;

pub mod book;
pub mod frame;
pub mod parse;
pub mod replay;
