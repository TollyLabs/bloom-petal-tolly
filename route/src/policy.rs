//! Day-1 limits and the write gate. These are the founder-controlled compiled
//! defaults (DESIGN D4-D6, critique M7/M12); `tolly_writes` is the USER's
//! runtime setting, not a founder gate.

use alloy_primitives::U256;

use crate::host;

/// Largest single operation, in 6-decimal USDC raw units (250 USDC). Applies
/// to the USDC spent on a buy, and to the QUOTED USDC output of a sell.
pub const MAX_OP_USDC_RAW: u64 = 250_000_000;
pub const MAX_OP_USDC_HUMAN: &str = "250";

/// Largest launch dev buy, in 6-decimal USDC raw units (140 USDC), the web
/// form's cap for the 3% anti-snipe wallet limit.
pub const MAX_DEV_BUY_USDC_RAW: u64 = 140_000_000;
pub const MAX_DEV_BUY_USDC_HUMAN: &str = "140";

/// Slippage tolerance in basis points.
pub const SLIPPAGE_DEFAULT_BPS: u32 = 500;
pub const SLIPPAGE_MIN_BPS: u32 = 50;
pub const SLIPPAGE_MAX_BPS: u32 = 5_000;

/// Each launch metadata field is bounded on-chain (`TollyPad.MAX_META_LEN`).
pub const META_MAX_BYTES: usize = 512;

/// Native balance (18-dec) that must remain after an operation for gas.
/// Bloom's engine estimates gas itself; this is a fixed reserve, not a computed
/// budget (critique B1). 0.05 USDC.
pub const GAS_RESERVE_WEI: u128 = 50_000_000_000_000_000;

/// Largest accepted write body.
pub const MAX_BODY_BYTES: usize = 4 * 1024;

/// `plan_md` from the outbox is kept for audit, truncated.
pub const MAX_PLAN_MD_BYTES: usize = 4 * 1024;

/// Rows served by `markets.json` and listed under `tokens/`.
pub const MARKETS_LIMIT: u32 = 50;

/// Bounds for `positions.json` (critique M11).
pub const POSITIONS_MAX_OPS: usize = 200;
pub const POSITIONS_MAX_TOKENS: usize = 32;

/// Most operation records loaded per listing read (`recent`); a wallet with
/// more ids reports `scan_truncated: true`. The live-entry check (critique
/// M1) does not scan: it reads the `tolly/live/` index.
pub const OPS_SCAN_MAX_OPS: usize = 1_000;

/// Runtime setting that enables writes. Lives in the user's
/// `[petals.runtime.tolly.values]`; any Bloom user can flip it.
pub const WRITES_SETTING: &str = "tolly_writes";
pub const WRITES_ENABLED_VALUE: &str = "enabled";

pub fn max_op_usdc_raw() -> U256 {
    U256::from(MAX_OP_USDC_RAW)
}

pub fn max_dev_buy_usdc_raw() -> U256 {
    U256::from(MAX_DEV_BUY_USDC_RAW)
}

pub fn gas_reserve_wei() -> U256 {
    U256::from(GAS_RESERVE_WEI)
}

/// Whether the owner has enabled live writes for this Petal.
pub fn writes_enabled() -> bool {
    matches!(
        host::runtime_setting(WRITES_SETTING),
        Ok(Some(value)) if value.trim() == WRITES_ENABLED_VALUE
    )
}

/// Validate a slippage tolerance, applying the default when absent.
pub fn slippage_bps(value: Option<u32>) -> Result<u32, String> {
    let bps = value.unwrap_or(SLIPPAGE_DEFAULT_BPS);
    if !(SLIPPAGE_MIN_BPS..=SLIPPAGE_MAX_BPS).contains(&bps) {
        return Err(format!(
            "slippage_bps must be between {SLIPPAGE_MIN_BPS} and {SLIPPAGE_MAX_BPS} (default {SLIPPAGE_DEFAULT_BPS})"
        ));
    }
    Ok(bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slippage_bounds() {
        assert_eq!(slippage_bps(None).unwrap(), 500);
        assert_eq!(slippage_bps(Some(50)).unwrap(), 50);
        assert_eq!(slippage_bps(Some(5000)).unwrap(), 5000);
        assert!(slippage_bps(Some(49)).is_err());
        assert!(slippage_bps(Some(5001)).is_err());
        assert!(slippage_bps(Some(0)).is_err());
    }
}
