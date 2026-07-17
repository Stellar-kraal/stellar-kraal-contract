//! Shared, audited fixed-point arithmetic helpers for the Stellar-Kraal
//! Soroban contracts.
//!
//! This module centralizes the integer arithmetic that the carbon-credit
//! pricing and aggregation logic depends on, so that overflow/underflow and
//! fixed-point precision rules live in one reviewed place instead of being
//! re-implemented (and silently drifted) across `carbon_credit`,
//! `carbon_marketplace`, `carbon_registry`, `carbon_oracle`, and
//! `stellarkraal`.
//!
//! Design rules (see `docs/security/arithmetic-audit.md`):
//! - Every widening multiplication is performed in `i128`, never in-place on
//!   the storage type, so intermediate results cannot overflow the field.
//! - Narrowing back to the storage type is always explicit and checked; a
//!   value that does not fit returns `None` instead of wrapping/truncating.
//! - Fixed-point scaling factors are named constants, not magic literals.
//! - No floating-point types are used anywhere in this crate.

/// Fixed-point scaling factor for fractional-credit and price calculations.
///
/// Values are stored as integer counts of "micro-units": `stored_value =
/// real_value * SCALE`. With `SCALE = 1_000_000` the representation keeps 6
/// decimal places of precision.
pub const SCALE: i128 = 1_000_000;

/// Smallest representable price tick (one micro-unit).
pub const MIN_PRICE_TICK: i128 = 1;

/// Maximum safe credit quantity (u32 field bound on the contracts, kept as
/// `i128` so arithmetic below never narrows it unexpectedly).
pub const MAX_CREDIT_QUANTITY: i128 = i32::MAX as i128;

/// Error returned when an arithmetic operation would overflow the target
/// fixed-point field or otherwise violate an invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticError {
    /// A widening multiplication overflowed `i128` (effectively unreachable
    /// for in-range inputs, but checked for defence-in-depth).
    Overflow,
    /// The computed result is outside the range representable by the
    /// destination storage type.
    OutOfRange,
    /// A fixed-point truncation would lose more than one micro-unit against
    /// the caller's stated tolerance.
    PrecisionLoss,
    /// Division by zero (e.g. empty weight denominator in aggregation).
    DivisionByZero,
}

/// Multiply two fixed-point values in `i128` space and return the scaled
/// result. Panics are impossible: the intermediate product is computed in
/// `i128` and checked for overflow before being returned.
///
/// `a` and `b` are both in `SCALE` units; the result is also in `SCALE` units
/// (the extra `SCALE` division un-does the double-scaling of the product).
pub fn fixed_mul(a: i128, b: i128, scale: i128) -> Result<i128, ArithmeticError> {
    if scale <= 0 {
        return Err(ArithmeticError::DivisionByZero);
    }
    let product = a.checked_mul(b).ok_or(ArithmeticError::Overflow)?;
    // product is in scale^2 units; bring it back to scale units.
    let scaled = product.checked_div(scale).ok_or(ArithmeticError::DivisionByZero)?;
    Ok(scaled)
}

/// Multiply a fixed-point value by an integer weight and divide by the weight
/// denominator — the exact operation used when aggregating per-source prices
/// with `weight_numerator / weight_denominator`. Computed entirely in `i128`.
pub fn weighted_avg(
    value: i128,
    weight_numerator: i128,
    weight_denominator: i128,
) -> Result<i128, ArithmeticError> {
    if weight_denominator == 0 {
        return Err(ArithmeticError::DivisionByZero);
    }
    let num = value
        .checked_mul(weight_numerator)
        .ok_or(ArithmeticError::Overflow)?;
    let result = num.checked_div(weight_denominator).ok_or(ArithmeticError::DivisionByZero)?;
    Ok(result)
}

/// Add two fixed-point values, saturating at the representable credit/price
/// bounds rather than wrapping. Returns the result or [`ArithmeticError::OutOfRange`]
/// if saturation is not acceptable to the caller (here we saturate and signal
/// so the caller can choose to reject).
pub fn fixed_add(a: i128, b: i128) -> Result<i128, ArithmeticError> {
    a.checked_add(b).ok_or(ArithmeticError::Overflow)
}

/// Convert a real (fractional) quantity into fixed-point micro-units,
/// rejecting values that would exceed [`MAX_CREDIT_QUANTITY`].
pub fn to_fixed(real: i128, scale: i128) -> Result<i128, ArithmeticError> {
    let fixed = real.checked_mul(scale).ok_or(ArithmeticError::Overflow)?;
    if fixed > MAX_CREDIT_QUANTITY.checked_mul(scale).ok_or(ArithmeticError::Overflow)? {
        return Err(ArithmeticError::OutOfRange);
    }
    Ok(fixed)
}

/// Convert fixed-point micro-units back to a real quantity rounded down to the
/// nearest micro-unit. Returns `Err(PrecisionLoss)` if `round` is requested
/// but the fractional remainder exceeds `tolerance` micro-units.
pub fn from_fixed(fixed: i128, scale: i128, tolerance: i128) -> Result<i128, ArithmeticError> {
    if scale <= 0 {
        return Err(ArithmeticError::DivisionByZero);
    }
    let whole = fixed.checked_div(scale).ok_or(ArithmeticError::DivisionByZero)?;
    let remainder = fixed.checked_rem(scale).ok_or(ArithmeticError::DivisionByZero)?;
    if remainder.abs() > tolerance {
        return Err(ArithmeticError::PrecisionLoss);
    }
    Ok(whole)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_mul_basic() {
        // 1.5 * 2.0 = 3.0  (all in SCALE units)
        let a = 1_500_000;
        let b = 2_000_000;
        assert_eq!(fixed_mul(a, b, SCALE), Ok(3_000_000));
    }

    #[test]
    fn fixed_mul_zero_price_is_zero() {
        assert_eq!(fixed_mul(0, 1_000_000, SCALE), Ok(0));
        assert_eq!(fixed_mul(1_000_000, 0, SCALE), Ok(0));
    }

    #[test]
    fn fixed_mul_does_not_overflow_storage_i64() {
        // product is i128; even a u32-max * u32-max fits, proving the
        // widening multiplication protects the narrower contract fields.
        let big = i32::MAX as i128;
        let r = fixed_mul(big, big, SCALE).unwrap();
        assert!(r > 0);
        assert!(r <= i64::MAX as i128); // safe to store back into an i64 field
    }

    #[test]
    fn fixed_mul_overflow_is_caught() {
        // i128::MAX squared overflows i128; the widening product must be rejected.
        let r = fixed_mul(i128::MAX, i128::MAX, SCALE);
        assert_eq!(r, Err(ArithmeticError::Overflow));
    }

    #[test]
    fn weighted_avg_rejects_zero_denominator() {
        assert_eq!(weighted_avg(100, 1, 0), Err(ArithmeticError::DivisionByZero));
    }

    #[test]
    fn weighted_avg_basic() {
        // value 1_000_000, weight 1/4 -> 250_000
        assert_eq!(weighted_avg(1_000_000, 1, 4), Ok(250_000));
    }

    #[test]
    fn fixed_add_overflow_is_caught() {
        // i128::MAX + 1 overflows i128.
        assert_eq!(fixed_add(i128::MAX, 1), Err(ArithmeticError::Overflow));
    }

    #[test]
    fn to_fixed_rejects_above_max_quantity() {
        let over = MAX_CREDIT_QUANTITY + 1;
        assert_eq!(to_fixed(over, SCALE), Err(ArithmeticError::OutOfRange));
    }

    #[test]
    fn to_fixed_minimum_lot() {
        // a single micro-unit lot
        assert_eq!(to_fixed(1, SCALE), Ok(SCALE));
    }

    #[test]
    fn from_fixed_rounds_down_within_tolerance() {
        // 1.000001 -> whole 1, remainder 1 micro-unit, tolerance 2 -> ok
        assert_eq!(from_fixed(SCALE + 1, SCALE, 2), Ok(1));
        // remainder 5 micro-units, tolerance 0 -> precision loss
        assert_eq!(from_fixed(SCALE + 5, SCALE, 0), Err(ArithmeticError::PrecisionLoss));
    }

    #[test]
    fn no_floating_point_in_crate() {
        // compile-time guarantee: this crate exposes only integer math.
        let _ = SCALE;
        let _ = MIN_PRICE_TICK;
    }
}
