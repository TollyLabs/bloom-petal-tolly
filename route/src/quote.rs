//! Best-execution quoting across a token's venues (port of
//! `src/data/venues.ts#quoteVenues` + `bestVenue`, `swapExecution.ts#protectBestVenueQuote`).
//!
//! Every venue is quoted at the size the POOL will see: on an external-token
//! buy that is `gross - interface fee`; sells and pad tokens pay no fee. A
//! native-quote V4 venue is asked in 18-decimal native units (`net6 * 1e12`,
//! critique M3) and its sell output is normalised back to 6 decimals before
//! ranking. Venues are ranked by raw output, unanswerable last; `best` is the
//! first that fills, `best_executable` the first that fills AND the day-1
//! Petal can execute (D1: V4 is quoted for honesty but never executed).

use alloy_primitives::{Address, U256};
use serde_json::{Value, json};

use crate::abi::PoolKey;
use crate::amount::{addr_hex, format_units, native18_to_usdc6, u256_to_f64, usdc6_to_native18};
use crate::api::{Execution, Provenance, TokenDetail, Venue, VenueKind, execution_for};
use crate::chain;
use crate::constants::{INTERFACE_FEE_BPS, USDC, USDC_ERC20_DECIMALS, V4_NATIVE_CURRENCY};
use crate::fee;
use crate::host;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }
    pub fn is_buy(self) -> bool {
        self == Self::Buy
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrossCheck {
    /// `tollFor` on chain equals the local fee.
    Matched,
    /// The contract would charge something else; writes refuse.
    Mismatch { onchain: U256 },
    /// Not applicable (pad token or sell).
    Skipped,
    /// The call failed; writes refuse.
    Unavailable(String),
}

impl CrossCheck {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Matched => "tollFor-matched",
            Self::Mismatch { .. } => "tollFor-mismatch",
            Self::Skipped => "skipped",
            Self::Unavailable(_) => "tollFor-unavailable",
        }
    }
    pub fn allows_write(&self) -> bool {
        matches!(self, Self::Matched | Self::Skipped)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VenueQuote {
    pub venue: Venue,
    pub execution: Execution,
    /// The exact amount this venue was asked for, in its own representation.
    pub amount_in_raw: U256,
    /// Normalised output: token units on a buy, 6-decimal USDC on a sell.
    pub out_raw: Option<U256>,
    /// Raw 18-decimal output of a native-quote V4 sell (before normalisation).
    pub out_raw_native: Option<U256>,
    pub impact_pct: Option<f64>,
    pub error: Option<String>,
}

impl VenueQuote {
    pub fn fills(&self) -> bool {
        self.out_raw.is_some_and(|o| o > U256::ZERO)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Quote {
    pub side: Side,
    pub token: Address,
    pub symbol: String,
    pub token_decimals: u32,
    pub provenance: Provenance,
    /// Gross input: 6-decimal USDC on a buy, token units on a sell.
    pub amount_in_raw: U256,
    pub fee_raw: U256,
    pub fee_applies: bool,
    pub cross_check: CrossCheck,
    pub amount_to_pool_raw: U256,
    pub slippage_bps: u32,
    /// Ranked best first.
    pub venues: Vec<VenueQuote>,
    pub best: Option<usize>,
    pub best_executable: Option<usize>,
    pub amount_out_minimum_raw: Option<U256>,
    pub worse_than_best_pct: Option<f64>,
    pub warnings: Vec<String>,
    pub quoted_ms: u64,
}

impl Quote {
    pub fn best_executable(&self) -> Option<&VenueQuote> {
        self.best_executable.map(|i| &self.venues[i])
    }
    pub fn best(&self) -> Option<&VenueQuote> {
        self.best.map(|i| &self.venues[i])
    }
    pub fn venue_by_id(&self, id: &str) -> Option<&VenueQuote> {
        self.venues
            .iter()
            .find(|v| v.venue.id.eq_ignore_ascii_case(id))
    }
    pub fn out_decimals(&self) -> u32 {
        match self.side {
            Side::Buy => self.token_decimals,
            Side::Sell => USDC_ERC20_DECIMALS,
        }
    }
}

/// The reference trade for the marginal rate: a thousandth of the real one,
/// floored so it cannot round to zero.
fn ref_size(amount_in: U256) -> U256 {
    let r = amount_in / U256::from(1000u64);
    if r > U256::ZERO { r } else { U256::from(1u64) }
}

/// How far this fill lands below the venue's own marginal rate, in percent.
pub fn impact_pct(out: U256, amount_in: U256, ref_out: U256, ref_in: U256) -> Option<f64> {
    if out == U256::ZERO || ref_out == U256::ZERO || amount_in == U256::ZERO || ref_in == U256::ZERO
    {
        return None;
    }
    let rate = u256_to_f64(out) / u256_to_f64(amount_in);
    let marginal = u256_to_f64(ref_out) / u256_to_f64(ref_in);
    if marginal.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) || !rate.is_finite() {
        return None;
    }
    let d = (1.0 - rate / marginal) * 100.0;
    Some(if d > 0.0 { d } else { 0.0 })
}

/// `TollyMultiRouter.v2AmountOut`, mirrored: `(in*(10000-f)*rOut)/(rIn*10000+in*(10000-f))`.
pub fn v2_amount_out(amount_in: U256, reserve_in: U256, reserve_out: U256, fee_bps: u16) -> U256 {
    if amount_in == U256::ZERO || reserve_in == U256::ZERO || reserve_out == U256::ZERO {
        return U256::ZERO;
    }
    let bps = U256::from(10_000u64);
    let after_fee = amount_in.saturating_mul(bps - U256::from(fee_bps));
    let denominator = reserve_in.saturating_mul(bps).saturating_add(after_fee);
    if denominator == U256::ZERO {
        return U256::ZERO;
    }
    after_fee.saturating_mul(reserve_out) / denominator
}

fn v4_pool_key(venue: &Venue) -> Option<PoolKey> {
    Some(PoolKey {
        currency0: venue.currency0?,
        currency1: venue.currency1?,
        fee: venue.fee?,
        tick_spacing: venue.tick_spacing?,
        hooks: venue.hooks?,
    })
}

fn no_fill(
    venue: &Venue,
    execution: Execution,
    amount_in: U256,
    error: impl Into<String>,
) -> VenueQuote {
    VenueQuote {
        venue: venue.clone(),
        execution,
        amount_in_raw: amount_in,
        out_raw: None,
        out_raw_native: None,
        impact_pct: None,
        error: Some(error.into()),
    }
}

fn quote_venue(
    venue: &Venue,
    provenance: Provenance,
    token: Address,
    quote_decimals: u32,
    side: Side,
    net: U256,
) -> VenueQuote {
    let execution = execution_for(venue, provenance);
    let (token_in, token_out) = match side {
        Side::Buy => (USDC, token),
        Side::Sell => (token, USDC),
    };
    match venue.kind {
        VenueKind::V3 => {
            let Some(fee_tier) = venue.fee else {
                return no_fill(venue, execution, net, "no-fee-tier");
            };
            let ref_in = ref_size(net);
            match chain::quoter_v2_amount_out(token_in, token_out, net, fee_tier) {
                Ok(out) => {
                    let ref_out =
                        chain::quoter_v2_amount_out(token_in, token_out, ref_in, fee_tier)
                            .unwrap_or(U256::ZERO);
                    VenueQuote {
                        venue: venue.clone(),
                        execution,
                        amount_in_raw: net,
                        out_raw: Some(out),
                        out_raw_native: None,
                        impact_pct: impact_pct(out, net, ref_out, ref_in),
                        error: None,
                    }
                }
                Err(_) => no_fill(venue, execution, net, "no-fill-at-size"),
            }
        }
        VenueKind::V4 => {
            if !venue.tradeable {
                return no_fill(venue, execution, net, "v4-no-live-liquidity");
            }
            // `venueMatchesQuoteDecimals`: the site only considers a V4 venue
            // whose quote representation is the one the API's canonical market
            // uses for this token. Mirror it so `best` never names a venue the
            // website would not.
            let venue_quote_decimals = if venue.native_quote { 18 } else { 6 };
            if venue_quote_decimals != quote_decimals {
                return no_fill(venue, execution, net, "quote-decimals-mismatch");
            }
            let Some(key) = v4_pool_key(venue) else {
                return no_fill(venue, execution, net, "v4-incomplete-pool-key");
            };
            let quote_currency = if venue.native_quote {
                V4_NATIVE_CURRENCY
            } else {
                venue.quote_token.unwrap_or(USDC)
            };
            // `v4PoolKey` derives currency0/1 by sorting (token, quote); the
            // API row must agree or the swap direction below is wrong.
            let (lo, hi) = if token < quote_currency {
                (token, quote_currency)
            } else {
                (quote_currency, token)
            };
            if (key.currency0, key.currency1) != (lo, hi) {
                return no_fill(venue, execution, net, "v4-pool-key-mismatch");
            }
            let input_currency = match side {
                Side::Buy => quote_currency,
                Side::Sell => token,
            };
            let zero_for_one = input_currency == key.currency0;
            // Critique M3: a native-quote pool is asked in 18-decimal native units.
            let exact_amount = if side.is_buy() && venue.native_quote {
                usdc6_to_native18(net)
            } else {
                net
            };
            let ref_in = ref_size(exact_amount);
            match chain::v4_quoter_amount_out(&key, zero_for_one, exact_amount) {
                Ok(out) => {
                    let ref_out = chain::v4_quoter_amount_out(&key, zero_for_one, ref_in)
                        .unwrap_or(U256::ZERO);
                    let (out_norm, out_native) = if !side.is_buy() && venue.native_quote {
                        (native18_to_usdc6(out), Some(out))
                    } else {
                        (out, None)
                    };
                    VenueQuote {
                        venue: venue.clone(),
                        execution,
                        amount_in_raw: exact_amount,
                        out_raw: Some(out_norm),
                        out_raw_native: out_native,
                        impact_pct: impact_pct(out, exact_amount, ref_out, ref_in),
                        error: None,
                    }
                }
                Err(_) => no_fill(venue, execution, exact_amount, "v4-no-fill-at-size"),
            }
        }
        VenueKind::V2 => {
            if !venue.tradeable {
                return no_fill(venue, execution, net, "v2-not-verified");
            }
            let Some(fee_bps) = venue.fee_bps else {
                return no_fill(venue, execution, net, "v2-fee-unsolved");
            };
            let Some(pair) = venue.pool_address() else {
                return no_fill(venue, execution, net, "v2-bad-pair");
            };
            match chain::v2_pair_state(pair) {
                Ok(state) => {
                    let (reserve_in, reserve_out) = if state.token0 == token_in {
                        (state.reserve0, state.reserve1)
                    } else {
                        (state.reserve1, state.reserve0)
                    };
                    let out = v2_amount_out(net, reserve_in, reserve_out, fee_bps);
                    let ref_in = ref_size(net);
                    let ref_out = v2_amount_out(ref_in, reserve_in, reserve_out, fee_bps);
                    VenueQuote {
                        venue: venue.clone(),
                        execution,
                        amount_in_raw: net,
                        out_raw: Some(out),
                        out_raw_native: None,
                        impact_pct: impact_pct(out, net, ref_out, ref_in),
                        error: None,
                    }
                }
                Err(_) => no_fill(venue, execution, net, "v2-pair-did-not-answer"),
            }
        }
        VenueKind::Pump | VenueKind::Dyor => {
            no_fill(venue, execution, net, "custom-curve-not-quoted")
        }
    }
}

/// Quote a buy (`amount_in_raw` = gross 6-decimal USDC) or a sell
/// (`amount_in_raw` = token units) across every venue of `detail`.
pub fn quote(detail: &TokenDetail, side: Side, amount_in_raw: U256, slippage_bps: u32) -> Quote {
    let external = detail.provenance.is_external();
    let fee_applies = side.is_buy() && external;
    let fee_raw = fee::interface_fee(amount_in_raw, external, side.is_buy());
    let net = amount_in_raw - fee_raw;
    let cross_check = if fee_applies {
        match chain::multi_router_toll_for(USDC, detail.address, amount_in_raw) {
            Ok(onchain) if onchain == fee_raw => CrossCheck::Matched,
            Ok(onchain) => CrossCheck::Mismatch { onchain },
            Err(e) => CrossCheck::Unavailable(e),
        }
    } else {
        CrossCheck::Skipped
    };

    let mut venues: Vec<VenueQuote> = detail
        .venues
        .iter()
        .map(|venue| {
            quote_venue(
                venue,
                detail.provenance,
                detail.address,
                detail.quote_decimals,
                side,
                net,
            )
        })
        .collect();
    // Descending by output; a venue that could not answer sorts last (stable).
    venues.sort_by(|a, b| match (a.out_raw, b.out_raw) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => y.cmp(&x),
    });
    let best = venues.iter().position(VenueQuote::fills);
    let best_executable = venues
        .iter()
        .position(|v| v.fills() && v.execution.supported);
    let amount_out_minimum_raw = best_executable
        .and_then(|i| fee::protect(venues[i].out_raw.unwrap_or_default(), slippage_bps));
    let worse_than_best_pct = match (best, best_executable) {
        (Some(b), Some(e)) if b != e => {
            let best_out = u256_to_f64(venues[b].out_raw.unwrap_or_default());
            let exec_out = u256_to_f64(venues[e].out_raw.unwrap_or_default());
            (best_out > 0.0).then(|| ((1.0 - exec_out / best_out) * 100.0).max(0.0))
        }
        _ => None,
    };

    let mut warnings = Vec::new();
    match (best, best_executable) {
        (Some(b), Some(e)) if b != e => warnings.push(format!(
            "best venue {} ({}) is not executable day-1 ({}); executable venue {} ({}) is {:.2}% worse; a write needs allow_worse_venue:true",
            venues[b].venue.id,
            venues[b].venue.kind.name(),
            venues[b].execution.reason.unwrap_or("unsupported"),
            venues[e].venue.id,
            venues[e].venue.kind.name(),
            worse_than_best_pct.unwrap_or(0.0)
        )),
        (Some(b), None) => warnings.push(format!(
            "best venue {} ({}) is not executable day-1 ({}) and no executable venue fills this size; a write will refuse",
            venues[b].venue.id,
            venues[b].venue.kind.name(),
            venues[b].execution.reason.unwrap_or("unsupported")
        )),
        (None, _) => warnings.push("no venue can fill this size; a write will refuse".into()),
        _ => {}
    }
    match &cross_check {
        CrossCheck::Mismatch { onchain } => warnings.push(format!(
            "tollFor mismatch: the router would charge {onchain} raw but the local rule says {fee_raw}; writes refuse"
        )),
        CrossCheck::Unavailable(e) => warnings.push(format!("tollFor cross-check unavailable ({e}); writes refuse")),
        _ => {}
    }
    if best_executable.is_some() && amount_out_minimum_raw.is_none() {
        warnings.push("the protected minimum output rounds to zero; a write will refuse".into());
    }

    Quote {
        side,
        token: detail.address,
        symbol: detail.symbol.clone(),
        token_decimals: detail.decimals,
        provenance: detail.provenance,
        amount_in_raw,
        fee_raw,
        fee_applies,
        cross_check,
        amount_to_pool_raw: net,
        slippage_bps,
        venues,
        best,
        best_executable,
        amount_out_minimum_raw,
        worse_than_best_pct,
        warnings,
        quoted_ms: host::now_ms(),
    }
}

fn opt_u256(value: Option<U256>) -> Value {
    value
        .map(|v| Value::String(v.to_string()))
        .unwrap_or(Value::Null)
}

fn venue_quote_json(v: &VenueQuote, out_decimals: u32) -> Value {
    json!({
        "id": v.venue.id,
        "kind": v.venue.kind.name(),
        "fee": v.venue.fee,
        "fee_bps": v.venue.fee_bps,
        "native_quote": v.venue.native_quote,
        "amount_in_raw": v.amount_in_raw.to_string(),
        "out_raw": opt_u256(v.out_raw),
        "out_human": v.out_raw.map(|o| format_units(o, out_decimals)),
        "out_raw_native": opt_u256(v.out_raw_native),
        "impact_pct": v.impact_pct,
        "liquidity_usdc": v.venue.liquidity_usdc.map(|l| format!("{l}")),
        "execution": if v.execution.supported { "supported" } else { "unsupported" },
        "execution_reason": v.execution.reason,
        "spender": v.execution.spender.map(addr_hex),
        "router_call": v.execution.router_call,
        "error": v.error,
    })
}

/// `quote/[address]/{buy,sell}/[amount].json`
pub fn quote_document(q: &Quote) -> Value {
    let (in_asset, in_decimals) = match q.side {
        Side::Buy => ("USDC", USDC_ERC20_DECIMALS),
        Side::Sell => (q.symbol.as_str(), q.token_decimals),
    };
    let out_decimals = q.out_decimals();
    let out_asset = match q.side {
        Side::Buy => q.symbol.as_str(),
        Side::Sell => "USDC",
    };
    let venues: Vec<Value> = q
        .venues
        .iter()
        .map(|v| venue_quote_json(v, out_decimals))
        .collect();
    let best = q.best().map(|v| json!({ "id": v.venue.id, "kind": v.venue.kind.name(), "out_raw": opt_u256(v.out_raw), "execution": if v.execution.supported { "supported" } else { "unsupported" } }));
    let best_executable = q.best_executable().map(|v| json!({
        "id": v.venue.id,
        "kind": v.venue.kind.name(),
        "out_raw": opt_u256(v.out_raw),
        "out_human": v.out_raw.map(|o| format_units(o, out_decimals)),
        "amount_out_minimum_raw": opt_u256(q.amount_out_minimum_raw),
        "amount_out_minimum_human": q.amount_out_minimum_raw.map(|o| format_units(o, out_decimals)),
        "slippage_bps": q.slippage_bps,
        "worse_than_best_pct": q.worse_than_best_pct,
        "spender": v.execution.spender.map(addr_hex),
        "router_call": v.execution.router_call,
    }));
    json!({
        "schema": "tolly.quote.v1",
        "side": q.side.name(),
        "token": addr_hex(q.token),
        "symbol": q.symbol,
        "provenance": q.provenance.name(),
        "amount_in": { "asset": in_asset, "human": format_units(q.amount_in_raw, in_decimals), "raw": q.amount_in_raw.to_string(), "decimals": in_decimals },
        "amount_out_asset": { "asset": out_asset, "decimals": out_decimals },
        "interface_fee": { "bps": INTERFACE_FEE_BPS, "raw": q.fee_raw.to_string(), "applies": q.fee_applies, "cross_check": q.cross_check.label() },
        "amount_to_pool_raw": q.amount_to_pool_raw.to_string(),
        "slippage_bps": q.slippage_bps,
        "venues": venues,
        "best": best,
        "best_executable": best_executable,
        "warnings": q.warnings,
        "quoted_ms": q.quoted_ms,
        "note": "quotes are simulations at the latest block; a write re-quotes before staging and stages one transaction for the owner to confirm in Bloom",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_math_matches_the_router_identity() {
        // in=1e6, rIn=18187e6, rOut=1.23456789e24, fee 30 bps -> matches the
        // frontend's local mirror by construction of the same integer formula.
        let out = v2_amount_out(
            U256::from(1_000_000u64),
            U256::from(18_187_000_000u64),
            U256::from(1_234_567_890_000_000_000_000_000u128),
            30,
        );
        let expected = (U256::from(1_000_000u64)
            * U256::from(9970u64)
            * U256::from(1_234_567_890_000_000_000_000_000u128))
            / (U256::from(18_187_000_000u64) * U256::from(10_000u64)
                + U256::from(1_000_000u64) * U256::from(9970u64));
        assert_eq!(out, expected);
        assert_eq!(
            v2_amount_out(U256::ZERO, U256::from(1u64), U256::from(1u64), 30),
            U256::ZERO
        );
        assert_eq!(
            v2_amount_out(U256::from(1u64), U256::ZERO, U256::from(1u64), 30),
            U256::ZERO
        );
    }

    #[test]
    fn impact_is_measured_against_the_venue_marginal_rate() {
        // Full trade fills at 0.9 per unit, reference at 1.0 per unit -> 10%.
        let pct = impact_pct(
            U256::from(900u64),
            U256::from(1000u64),
            U256::from(1u64),
            U256::from(1u64),
        )
        .unwrap();
        assert!((pct - 10.0).abs() < 1e-9);
        // Better than marginal clamps to zero.
        assert_eq!(
            impact_pct(
                U256::from(1100u64),
                U256::from(1000u64),
                U256::from(1u64),
                U256::from(1u64)
            ),
            Some(0.0)
        );
        assert_eq!(
            impact_pct(
                U256::ZERO,
                U256::from(1u64),
                U256::from(1u64),
                U256::from(1u64)
            ),
            None
        );
    }

    #[test]
    fn ref_size_floors_at_one() {
        assert_eq!(ref_size(U256::from(999u64)), U256::from(1u64));
        assert_eq!(ref_size(U256::from(25_000_000u64)), U256::from(25_000u64));
    }
}
