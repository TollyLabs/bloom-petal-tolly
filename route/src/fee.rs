//! The TOLLY interface fee, mirrored exactly from `src/data/interfaceFee.ts`
//! and `TollyMultiRouter.tollFor` (pure).
//!
//! Three rules: external (non-pad) tokens only; buys only (input = USDC);
//! taken off the input before the swap, `amountIn * 20 / 10000` floored, and
//! zero when it would round to zero. The contract is what actually charges;
//! `chain::multi_router_toll_for` cross-checks this on every external buy.

use alloy_primitives::U256;

use crate::constants::INTERFACE_FEE_BPS;

const BPS: u64 = 10_000;

/// What TOLLY takes from this trade, in the input's raw units.
pub fn interface_fee(amount_in: U256, external: bool, is_buy: bool) -> U256 {
    if !external || !is_buy || amount_in == U256::ZERO {
        return U256::ZERO;
    }
    let fee = amount_in.saturating_mul(U256::from(INTERFACE_FEE_BPS)) / U256::from(BPS);
    if fee > U256::ZERO && fee < amount_in {
        fee
    } else {
        U256::ZERO
    }
}

/// The amount that actually reaches the pool.
pub fn amount_after_fee(amount_in: U256, external: bool, is_buy: bool) -> U256 {
    amount_in - interface_fee(amount_in, external, is_buy)
}

/// `amountOutMinimum` for a quoted output at a slippage tolerance, integer
/// math (`swapExecution.ts#protectBestVenueQuote`). `None` when the floor
/// would be zero or the tolerance is not a fraction.
pub fn protect(out: U256, slippage_bps: u32) -> Option<U256> {
    if slippage_bps as u64 >= BPS || out == U256::ZERO {
        return None;
    }
    let floor = out.saturating_mul(U256::from(BPS - slippage_bps as u64)) / U256::from(BPS);
    (floor > U256::ZERO).then_some(floor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::{pow10, usdc6_to_native18};

    #[test]
    fn fee_matrix_external_buy_only() {
        let amount = U256::from(25_000_000u64);
        assert_eq!(interface_fee(amount, true, true), U256::from(50_000u64));
        assert_eq!(interface_fee(amount, false, true), U256::ZERO);
        assert_eq!(interface_fee(amount, true, false), U256::ZERO);
        assert_eq!(interface_fee(amount, false, false), U256::ZERO);
        assert_eq!(
            amount_after_fee(amount, true, true),
            U256::from(24_950_000u64)
        );
    }

    #[test]
    fn fee_floors_and_rounds_dust_to_zero() {
        assert_eq!(interface_fee(U256::from(499u64), true, true), U256::ZERO);
        assert_eq!(
            interface_fee(U256::from(500u64), true, true),
            U256::from(1u64)
        );
        assert_eq!(
            interface_fee(U256::from(999u64), true, true),
            U256::from(1u64)
        );
        assert_eq!(interface_fee(U256::ZERO, true, true), U256::ZERO);
        assert_eq!(interface_fee(U256::from(1u64), true, true), U256::ZERO);
    }

    /// Critique M3: the 20 bps rate is decimal-invariant. Scaling the input by
    /// 1e12 scales the fee by 1e12 exactly whenever the 6-dec fee has no
    /// rounding remainder (input multiple of 500 raw); otherwise the 18-dec
    /// fee is larger by less than one 6-dec unit. The Petal computes the fee
    /// once in the ERC-20 unit the router charges and scales `net` for
    /// native-quote V4 quoting.
    #[test]
    fn fee_is_decimal_invariant_up_to_rounding() {
        for raw6 in [500u64, 25_000_000, 100_000_000, 250_000_000, 123_456_000] {
            let six = U256::from(raw6);
            assert_eq!(
                interface_fee(usdc6_to_native18(six), true, true),
                usdc6_to_native18(interface_fee(six, true, true)),
                "raw6={raw6}"
            );
        }
        for raw6 in [1u64, 499, 123_456_789, 99_999_999] {
            let six = U256::from(raw6);
            let fee18 = interface_fee(usdc6_to_native18(six), true, true);
            let fee6_scaled = usdc6_to_native18(interface_fee(six, true, true));
            assert!(fee18 >= fee6_scaled, "raw6={raw6}");
            assert!(fee18 - fee6_scaled < pow10(12), "raw6={raw6}");
        }
    }

    #[test]
    fn protect_floors_in_integer_math() {
        let out = U256::from(1_000_000u64);
        assert_eq!(protect(out, 500), Some(U256::from(950_000u64)));
        assert_eq!(protect(out, 50), Some(U256::from(995_000u64)));
        assert_eq!(protect(U256::from(3u64), 5000), Some(U256::from(1u64)));
        assert_eq!(protect(U256::from(1u64), 500), None);
        assert_eq!(protect(U256::ZERO, 500), None);
        assert_eq!(protect(out, 10_000), None);
        assert_eq!(protect(out, 20_000), None);
        // Matches the frontend vector: 1234567 at 5% -> 1172838 (floor).
        assert_eq!(
            protect(U256::from(1_234_567u64), 500),
            Some(U256::from(1_172_838u64))
        );
    }
}
