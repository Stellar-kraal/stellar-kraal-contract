//! Shared, audited fixed-point arithmetic helpers for the Stellar-Kraal
//! Soroban contracts.
//!
//! The actual implementation lives in [`math`]; this root module re-exports
//! it so dependents can use `arithmetic::fixed_mul`, etc., while the file is
//! named `math.rs` per the issue #6 acceptance criterion ("a shared `math.rs`
//! utility module").
pub mod math;
pub use math::*;
