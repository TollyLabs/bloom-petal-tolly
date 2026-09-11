//! Buy and sell: the one-transaction-per-write step function behind
//! `wallets/[wallet]/buy.json` and `wallets/[wallet]/sell.json`.
//!
//! Each POST with the same `operationId` advances the operation by at most one
//! staged transaction: `approve` (exact amount, only when the allowance is
//! short) or the swap itself. Before every stage the Petal re-reads the
//! allowance, re-quotes every venue, verifies the executable winner, checks
//! funding, cross-checks the fee against `tollFor`, simulates the exact
//! calldata from the wallet, and only then stages. The owner confirms in
//! Bloom; a write never claims a broadcast, a fill, or a completion.
//!
//! Fee (D2): external-token BUYS go through `MULTI_ROUTER.swapWithToll` /
//! `swapWithTollV2` with the GROSS amount (the router banks the 0.2% toll
//! atomically); pad tokens and every SELL go through `SwapRouter02
//! .exactInputSingle` / the multi router with no fee. Approvals are exact.

use alloy_primitives::{Address, U256};
use petal::DispatchResponse;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::abi;
use crate::amount::{
    addr_hex, format_units, parse_any_address, parse_decimal, parse_u256_decimal, usdc6_to_native18,
};
use crate::api::{Network, Provenance, token_detail};
use crate::chain;
use crate::constants::{INTERFACE_FEE_BPS, MULTI_ROUTER, SWAP_ROUTER02, USDC, USDC_ERC20_DECIMALS};
use crate::fee;
use crate::ops::{self, Kind, NextAction, Operation, Status, Step, TxEntry};
use crate::policy::{self, MAX_BODY_BYTES};
use crate::quote::{self, CrossCheck, Quote, Side, VenueQuote};
use crate::trace::{self, WriteTrace};
use crate::tx::{self, StageError};
use crate::wallet::{check_wallet_id, wallet_address};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuyRequest {
    #[serde(rename = "operationId")]
    pub operation_id: String,
    pub token: String,
    pub amount_usdc: String,
    #[serde(default)]
    pub slippage_bps: Option<u32>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub min_out_raw: Option<String>,
    #[serde(default)]
    pub allow_worse_venue: Option<bool>,
    /// Required `true` to stage again after a stage whose record could not be
    /// written (`stage_in_flight`); see AGENTS.md "Unrecorded stage".
    #[serde(default)]
    pub acknowledge_unrecorded_stage: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SellRequest {
    #[serde(rename = "operationId")]
    pub operation_id: String,
    pub token: String,
    /// A decimal amount of the token, or `"all"` (frozen at first stage).
    pub amount: String,
    #[serde(default)]
    pub slippage_bps: Option<u32>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub min_out_raw: Option<String>,
    #[serde(default)]
    pub allow_worse_venue: Option<bool>,
    #[serde(default)]
    pub acknowledge_unrecorded_stage: Option<bool>,
}

/// Validated, side-independent intent.
struct Intent {
    id: String,
    token: Address,
    side: Side,
    /// Human amount as given (`"25"`, `"1234.5"`, or `"all"` for sells).
    amount_human: String,
    slippage_bps: u32,
    venue: Option<String>,
    min_out_raw: Option<U256>,
    allow_worse_venue: bool,
    acknowledge_unrecorded_stage: bool,
}

/// `-3` with the `invalid-request` code, so a recorded refusal names it.
fn invalid(message: impl std::fmt::Display) -> DispatchResponse {
    petal::error(-3, format!("invalid-request: {message}"))
}

fn parse_body<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, DispatchResponse> {
    if body.len() > MAX_BODY_BYTES {
        return Err(invalid(format!(
            "request body exceeds {MAX_BODY_BYTES} bytes"
        )));
    }
    serde_json::from_slice(body).map_err(|e| invalid(format!("invalid request JSON: {e}")))
}

#[allow(clippy::too_many_arguments)]
fn intent(
    id: &str,
    token: &str,
    side: Side,
    amount_human: &str,
    slippage_bps: Option<u32>,
    venue: Option<String>,
    min_out_raw: Option<String>,
    allow_worse_venue: Option<bool>,
    acknowledge_unrecorded_stage: Option<bool>,
) -> Result<Intent, DispatchResponse> {
    ops::validate_id(id).map_err(invalid)?;
    let token = parse_any_address(token)
        .ok_or_else(|| invalid("token must be a 0x-prefixed 20-byte address"))?;
    if token == USDC || token == Address::ZERO {
        return Err(invalid(
            "token must be a traded token, not USDC or the zero address",
        ));
    }
    let slippage_bps = policy::slippage_bps(slippage_bps).map_err(invalid)?;
    if let Some(v) = venue.as_deref()
        && parse_any_address(v).is_none()
        && !crate::amount::is_bytes32_hex(v)
    {
        return Err(invalid(
            "venue must be a venue id from the quote (address or bytes32 pool id)",
        ));
    }
    let min_out_raw = min_out_raw
        .as_deref()
        .map(parse_u256_decimal)
        .transpose()
        .map_err(|e| invalid(format!("min_out_raw: {e}")))?;
    Ok(Intent {
        id: id.to_owned(),
        token,
        side,
        amount_human: amount_human.trim().to_owned(),
        slippage_bps,
        venue: venue.map(|v| v.to_ascii_lowercase()),
        min_out_raw,
        allow_worse_venue: allow_worse_venue.unwrap_or(false),
        acknowledge_unrecorded_stage: acknowledge_unrecorded_stage.unwrap_or(false),
    })
}

/// `wallets/[wallet]/buy.json` write. Every outcome, accepted or refused,
/// is persisted by the trace (see `trace`): Bloom delivers mounted writes
/// asynchronously, so the response alone would be invisible to the agent.
pub fn route_buy(wallet: &str, body: &[u8]) -> DispatchResponse {
    if let Err(r) = check_wallet_id(wallet) {
        return r;
    }
    let mut trace = WriteTrace::new(Kind::Buy, wallet, body);
    let response = buy_flow(wallet, body, &mut trace);
    trace.finish(response)
}

fn buy_flow(wallet: &str, body: &[u8], trace: &mut WriteTrace) -> DispatchResponse {
    let request: BuyRequest = match parse_body(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let echo = serde_json::to_value(&request).unwrap_or(Value::Null);
    trace.parsed(&request.operation_id, echo.clone());
    trace.swap_tuple(&request.token, &request.amount_usdc);
    match intent(
        &request.operation_id,
        &request.token,
        Side::Buy,
        &request.amount_usdc,
        request.slippage_bps,
        request.venue,
        request.min_out_raw,
        request.allow_worse_venue,
        request.acknowledge_unrecorded_stage,
    ) {
        Ok(intent) => advance(wallet, intent, echo, trace),
        Err(r) => r,
    }
}

/// `wallets/[wallet]/sell.json` write (traced like `route_buy`).
pub fn route_sell(wallet: &str, body: &[u8]) -> DispatchResponse {
    if let Err(r) = check_wallet_id(wallet) {
        return r;
    }
    let mut trace = WriteTrace::new(Kind::Sell, wallet, body);
    let response = sell_flow(wallet, body, &mut trace);
    trace.finish(response)
}

fn sell_flow(wallet: &str, body: &[u8], trace: &mut WriteTrace) -> DispatchResponse {
    let request: SellRequest = match parse_body(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let echo = serde_json::to_value(&request).unwrap_or(Value::Null);
    trace.parsed(&request.operation_id, echo.clone());
    trace.swap_tuple(&request.token, &request.amount);
    match intent(
        &request.operation_id,
        &request.token,
        Side::Sell,
        &request.amount,
        request.slippage_bps,
        request.venue,
        request.min_out_raw,
        request.allow_worse_venue,
        request.acknowledge_unrecorded_stage,
    ) {
        Ok(intent) => advance(wallet, intent, echo, trace),
        Err(r) => r,
    }
}

/// Read side of `buy.json`: the schema, defaults, limits and recent operations.
pub fn buy_description(wallet: &str) -> DispatchResponse {
    if let Err(r) = check_wallet_id(wallet) {
        return r;
    }
    // This route staged the entries, so this is where the host lets them be
    // inspected: reconcile first, then project (see `ops::route_read_side`).
    let side = ops::route_read_side(wallet, Kind::Buy, 5);
    petal::read_json_value(&json!({
        "schema": "tolly.buy-request.v1",
        "description": "Stage a USDC -> token buy for this Bloom wallet. One write stages at most one transaction (an exact USDC approve when the allowance is short, else the swap); after a write read this file (last_write, reconciled), then operations/<operationId>.json and follow next_action.",
        "writes_enabled": policy::writes_enabled(),
        "writes_setting": format!("{}={}", policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE),
        "write_semantics": trace::WRITE_SEMANTICS,
        "last_write": trace::last_write_json(wallet),
        "reconciled": side.reconciled,
        "reconcile_truncated": side.truncated,
        "body": {
            "operationId": "required; [a-z0-9][a-z0-9._-]{0,63}; idempotency key bound to (token, amount_usdc)",
            "token": "required; 0x token address",
            "amount_usdc": format!("required; decimal USDC, > 0 and <= {}", policy::MAX_OP_USDC_HUMAN),
            "slippage_bps": format!("optional; {}..={}, default {}", policy::SLIPPAGE_MIN_BPS, policy::SLIPPAGE_MAX_BPS, policy::SLIPPAGE_DEFAULT_BPS),
            "venue": "optional; pin a venue id from the quote file",
            "min_out_raw": "optional; agent floor in raw token units; the larger of it and the fresh protected floor is used",
            "allow_worse_venue": "optional; required true when the best venue is not executable day-1 (V4) or when pinning a venue that is not the winner",
            "acknowledge_unrecorded_stage": "optional; required true to stage again after the record reports stage_in_flight (an outbox entry this Petal staged but could not record)"
        },
        "limits": { "max_op_usdc": policy::MAX_OP_USDC_HUMAN, "interface_fee_bps_external_buys": INTERFACE_FEE_BPS },
        "quote_first": "quote/<token>/buy/<usdc>.json",
        "recent": side.recent,
    }))
}

/// Read side of `sell.json`.
pub fn sell_description(wallet: &str) -> DispatchResponse {
    if let Err(r) = check_wallet_id(wallet) {
        return r;
    }
    let side = ops::route_read_side(wallet, Kind::Sell, 5);
    petal::read_json_value(&json!({
        "schema": "tolly.sell-request.v1",
        "description": "Stage a token -> USDC sell for this Bloom wallet. One write stages at most one transaction (an exact token approve when the allowance is short, else the swap); no interface fee applies to sells. After a write read this file (last_write, reconciled), then operations/<operationId>.json.",
        "writes_enabled": policy::writes_enabled(),
        "writes_setting": format!("{}={}", policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE),
        "write_semantics": trace::WRITE_SEMANTICS,
        "last_write": trace::last_write_json(wallet),
        "reconciled": side.reconciled,
        "reconcile_truncated": side.truncated,
        "body": {
            "operationId": "required; [a-z0-9][a-z0-9._-]{0,63}; idempotency key bound to (token, amount)",
            "token": "required; 0x token address",
            "amount": "required; decimal token amount or \"all\" (balance frozen at the first stage)",
            "slippage_bps": format!("optional; {}..={}, default {}", policy::SLIPPAGE_MIN_BPS, policy::SLIPPAGE_MAX_BPS, policy::SLIPPAGE_DEFAULT_BPS),
            "venue": "optional; pin a venue id from the quote file",
            "min_out_raw": "optional; agent floor in raw 6-decimal USDC; the larger of it and the fresh protected floor is used",
            "allow_worse_venue": "optional; required true when the best venue is not executable day-1 (V4) or when pinning a venue that is not the winner",
            "acknowledge_unrecorded_stage": "optional; required true to stage again after the record reports stage_in_flight (an outbox entry this Petal staged but could not record)"
        },
        "limits": { "max_quoted_usdc_out": policy::MAX_OP_USDC_HUMAN },
        "quote_first": "quote/<token>/sell/<amount>.json",
        "recent": side.recent,
    }))
}

// ---- the step function ----

struct Failure {
    code: i32,
    op_code: &'static str,
    message: String,
    retryable: bool,
}

fn fail(code: i32, op_code: &'static str, message: impl Into<String>, retryable: bool) -> Failure {
    Failure {
        code,
        op_code,
        message: message.into(),
        retryable,
    }
}

fn record_failure(
    trace: &mut WriteTrace,
    op: &mut Operation,
    step: Step,
    failure: &Failure,
    now: u64,
) -> DispatchResponse {
    op.set_failed(
        Some(step),
        failure.op_code,
        failure.message.clone(),
        failure.retryable,
        now,
    );
    op.last_write_ms = Some(now);
    if let Err(e) = ops::save(op) {
        return petal::error(
            -4,
            format!(
                "{}: {} (and the record could not be updated: {e})",
                failure.op_code, failure.message
            ),
        );
    }
    trace.recorded();
    petal::error(
        failure.code,
        format!("{}: {}", failure.op_code, failure.message),
    )
}

fn venue_plan(v: &VenueQuote) -> Value {
    json!({
        "id": v.venue.id,
        "kind": v.venue.kind.name(),
        "fee": v.venue.fee,
        "fee_bps": v.venue.fee_bps,
        "factory": v.venue.factory.map(addr_hex),
        "native_quote": v.venue.native_quote,
    })
}

fn advance(wallet: &str, intent: Intent, echo: Value, trace: &mut WriteTrace) -> DispatchResponse {
    let now = trace.now();
    if !policy::writes_enabled() {
        return petal::error(
            -2,
            format!(
                "writes-disabled: set the runtime setting {}={} in Bloom to allow this Petal to stage transactions",
                policy::WRITES_SETTING,
                policy::WRITES_ENABLED_VALUE
            ),
        );
    }
    let network = Network::current();
    trace.network(network);
    let kind = match intent.side {
        Side::Buy => Kind::Buy,
        Side::Sell => Kind::Sell,
    };
    let address = match wallet_address(wallet) {
        Ok(a) => a,
        Err(e) => return petal::error(-4, e),
    };
    trace.address(address);
    let detail = match token_detail(network, intent.token) {
        Ok(d) => d,
        Err(r) => return r,
    };

    // Amounts and the economic tuple (critique M2: execution parameters stay out of it).
    let amount_spec: Option<U256> = match intent.side {
        Side::Buy => {
            let raw = match parse_decimal(&intent.amount_human, USDC_ERC20_DECIMALS) {
                Ok(v) => v,
                Err(e) => return invalid(format!("amount_usdc: {e}")),
            };
            if raw > policy::max_op_usdc_raw() {
                return petal::error(
                    -3,
                    format!(
                        "cap-exceeded: amount_usdc must be at most {} USDC per operation",
                        policy::MAX_OP_USDC_HUMAN
                    ),
                );
            }
            Some(raw)
        }
        Side::Sell => {
            if intent.amount_human == "all" {
                None
            } else {
                match parse_decimal(&intent.amount_human, detail.decimals) {
                    Ok(v) => Some(v),
                    Err(e) => return invalid(format!("amount: {e}")),
                }
            }
        }
    };
    let digest = trace::swap_digest(kind, intent.token, amount_spec);
    trace.tuple(digest.clone());

    // Load or claim the record.
    let mut op = match ops::load(wallet, &intent.id) {
        Ok(Some(op)) => op,
        Ok(None) => {
            let mut fresh = Operation::new(
                &intent.id,
                wallet,
                address,
                kind,
                network.name(),
                digest.clone(),
                echo.clone(),
                now,
            );
            fresh.last_write_ms = Some(now);
            match ops::claim(&fresh) {
                Ok(true) => fresh,
                Ok(false) => match ops::load(wallet, &intent.id) {
                    Ok(Some(op)) => op,
                    Ok(None) => {
                        return petal::error(-4, "operation record vanished after a claim race");
                    }
                    Err(e) => return petal::error(-4, e),
                },
                Err(e) => return petal::error(-4, e),
            }
        }
        Err(e) => return petal::error(-4, e),
    };
    // A record created by a refused write is bound by the first write that
    // gets past validation (see `trace`).
    if op.is_unbound() {
        op.request_sha256 = digest.clone();
    }
    if op.request_sha256 != digest {
        return petal::error(
            -3,
            "operation-id-bound: operationId already bound to a different request (token or amount differ); use a new operationId",
        );
    }
    if op.kind != kind {
        return petal::error(
            -3,
            "operation-id-bound: operationId already bound to a different operation kind",
        );
    }
    // Every write past the gates records the network it ran on.
    op.network = network.name().into();
    op.last_write_ms = Some(now);

    // Reconcile what the host says about the latest attempt.
    match ops::reconcile(&mut op, network, now) {
        Ok(changed) => {
            if changed && let Err(e) = ops::save(&op) {
                return petal::error(-4, e);
            }
        }
        Err(e) => return petal::error(-4, e),
    }
    match op.status {
        Status::Completed => return DispatchResponse::Write,
        Status::Failed if op.is_terminal() => return DispatchResponse::Write,
        Status::Staged | Status::Broadcast | Status::Unknown => return DispatchResponse::Write,
        Status::Confirmed if op.step != Some(Step::Approve) => return DispatchResponse::Write,
        _ => {}
    }
    // A stage this operation could not record may still be live in the
    // outbox (critique M1): never stage again over it silently.
    if let Err(r) = acknowledge_unrecorded_stage(&mut op, intent.acknowledge_unrecorded_stage, now)
    {
        return r;
    }
    // Critique M1: never two live entries for one subject.
    match ops::live_conflict(wallet, kind, &addr_hex(intent.token), &intent.id) {
        Ok(Some(conflict)) => {
            return petal::error(
                -2,
                format!("live-entry-conflict: {}", conflict.message("this token")),
            );
        }
        Ok(None) => {}
        Err(e) => return petal::error(-4, e),
    }
    // The body that drives this attempt (execution parameters may differ
    // from the claim's; the economic tuple is digest-bound above).
    op.request = echo;

    // Sell "all": freeze the balance at the first stage.
    let amount_in = match amount_spec {
        Some(raw) => raw,
        None => match op
            .plan
            .amount_in_raw
            .as_deref()
            .map(parse_u256_decimal)
            .transpose()
        {
            Ok(Some(frozen)) => frozen,
            Ok(None) => match chain::erc20_balance_of(intent.token, address) {
                Ok(balance) if balance > U256::ZERO => balance,
                Ok(_) => {
                    return record_failure(
                        trace,
                        &mut op,
                        Step::Swap,
                        &fail(
                            -3,
                            "insufficient-funds",
                            "the wallet holds none of this token",
                            true,
                        ),
                        now,
                    );
                }
                Err(e) => return petal::error(-4, e),
            },
            Err(e) => return petal::error(-4, format!("record plan: {e}")),
        },
    };

    // Fresh quote, every time.
    let quote = quote::quote(&detail, intent.side, amount_in, intent.slippage_bps);

    // Provenance twin: PAD.tokens(token).pool != 0 iff the API says pad (critique M4).
    match chain::pad_token_pool(intent.token) {
        Ok(pool) => {
            let onchain_pad = pool != Address::ZERO;
            if onchain_pad != (detail.provenance == Provenance::Pad) {
                return record_failure(
                    trace,
                    &mut op,
                    Step::Swap,
                    &fail(
                        -4,
                        "provenance-mismatch",
                        "the pad registry disagrees with the API about whether this is a TOLLY launch",
                        true,
                    ),
                    now,
                );
            }
        }
        Err(e) => {
            return record_failure(
                trace,
                &mut op,
                Step::Swap,
                &fail(
                    -4,
                    "quote-unavailable",
                    format!("pad registry read failed: {e}"),
                    true,
                ),
                now,
            );
        }
    }

    // Venue choice (D1).
    let chosen = match choose_venue(&quote, &intent) {
        Ok(v) => v,
        Err(f) => return record_failure(trace, &mut op, Step::Swap, &f, now),
    };
    let planned_venue = op
        .plan
        .venue
        .as_ref()
        .and_then(|v| v["id"].as_str())
        .map(str::to_owned);
    if let Some(planned) = planned_venue
        && !planned.eq_ignore_ascii_case(&chosen.venue.id)
        && intent.venue.is_none()
    {
        return record_failure(
            trace,
            &mut op,
            Step::Swap,
            &fail(
                -3,
                "venue-changed",
                format!(
                    "the executable winner is now {} ({}), not the planned {planned}; re-POST with venue set to accept it",
                    chosen.venue.id,
                    chosen.venue.kind.name()
                ),
                true,
            ),
            now,
        );
    }
    if !quote.cross_check.allows_write() {
        let message = match &quote.cross_check {
            CrossCheck::Mismatch { onchain } => format!(
                "the router would charge {onchain} raw but the local rule says {} raw",
                quote.fee_raw
            ),
            CrossCheck::Unavailable(e) => format!("tollFor cross-check unavailable: {e}"),
            _ => unreachable!(),
        };
        let (code, retryable) = match quote.cross_check {
            CrossCheck::Unavailable(_) => ("fee-check-unavailable", true),
            _ => ("fee-mismatch", false),
        };
        return record_failure(
            trace,
            &mut op,
            Step::Swap,
            &fail(-4, code, message, retryable),
            now,
        );
    }
    let out = chosen.out_raw.unwrap_or_default();
    // Critique M12: sells are capped by the QUOTED USDC output.
    if intent.side == Side::Sell && out > policy::max_op_usdc_raw() {
        return record_failure(
            trace,
            &mut op,
            Step::Swap,
            &fail(
                -3,
                "cap-exceeded",
                format!(
                    "the quoted USDC output {} exceeds {} USDC per operation; sell less",
                    format_units(out, USDC_ERC20_DECIMALS),
                    policy::MAX_OP_USDC_HUMAN
                ),
                true,
            ),
            now,
        );
    }
    let fresh_floor = match fee::protect(out, intent.slippage_bps) {
        Some(f) => f,
        None => {
            return record_failure(
                trace,
                &mut op,
                Step::Swap,
                &fail(
                    -4,
                    "quote-unavailable",
                    "the protected minimum output rounds to zero",
                    true,
                ),
                now,
            );
        }
    };
    let floor = match intent.min_out_raw {
        Some(requested) if out < requested => {
            return record_failure(
                trace,
                &mut op,
                Step::Swap,
                &fail(
                    -3,
                    "below-requested-floor",
                    format!("the fresh quote {out} is below the requested min_out_raw {requested}"),
                    true,
                ),
                now,
            );
        }
        Some(requested) => fresh_floor.max(requested),
        None => fresh_floor,
    };

    // Funding (fixed gas reserve; Bloom's engine does its own native check).
    let (token_in, token_out) = match intent.side {
        Side::Buy => (USDC, intent.token),
        Side::Sell => (intent.token, USDC),
    };
    let native = match chain::eth_get_balance(address) {
        Ok(b) => b,
        Err(e) => return petal::error(-4, e),
    };
    let native_needed = match intent.side {
        Side::Buy => usdc6_to_native18(amount_in).saturating_add(policy::gas_reserve_wei()),
        Side::Sell => policy::gas_reserve_wei(),
    };
    if native < native_needed {
        return record_failure(
            trace,
            &mut op,
            Step::Swap,
            &fail(
                -3,
                "insufficient-funds",
                format!(
                    "native USDC balance {} is below the {} needed (amount plus a {} USDC gas reserve)",
                    format_units(native, 18),
                    format_units(native_needed, 18),
                    format_units(policy::gas_reserve_wei(), 18)
                ),
                true,
            ),
            now,
        );
    }
    let in_balance = match chain::erc20_balance_of(token_in, address) {
        Ok(b) => b,
        Err(e) => return petal::error(-4, e),
    };
    if in_balance < amount_in {
        return record_failure(
            trace,
            &mut op,
            Step::Swap,
            &fail(
                -3,
                "insufficient-funds",
                format!("token balance {in_balance} raw is below the {amount_in} raw to spend"),
                true,
            ),
            now,
        );
    }

    // Freeze the plan.
    let spender = chosen.execution.spender.unwrap_or(SWAP_ROUTER02);
    let router_call = chosen.execution.router_call.unwrap_or("exactInputSingle");
    op.plan.token = Some(addr_hex(intent.token));
    op.plan.symbol = Some(detail.symbol.clone());
    op.plan.decimals = Some(detail.decimals);
    op.plan.provenance = Some(detail.provenance.name().into());
    op.plan.venue = Some(venue_plan(chosen));
    op.plan.spender = Some(addr_hex(spender));
    op.plan.router_call = Some(router_call.into());
    op.plan.amount_in_raw = Some(amount_in.to_string());
    op.plan.interface_fee_raw = Some(quote.fee_raw.to_string());
    op.plan.amount_to_pool_raw = Some(quote.amount_to_pool_raw.to_string());
    op.plan.quote_out_raw = Some(out.to_string());
    op.plan.slippage_bps = Some(intent.slippage_bps);
    op.plan.amount_out_minimum_raw = Some(floor.to_string());
    op.plan.quoted_ms = Some(quote.quoted_ms);
    let attempt_params = json!({
        "slippage_bps": intent.slippage_bps,
        "venue": intent.venue,
        "min_out_raw": intent.min_out_raw.map(|v| v.to_string()),
        "allow_worse_venue": intent.allow_worse_venue,
        "quote_out_raw": out.to_string(),
        "amount_out_minimum_raw": floor.to_string(),
    });

    // Supersede the failed attempt of the step we are about to retry.
    if op.status == Status::Failed
        && let Some(index) = op.latest_live()
        && matches!(
            op.txs[index].outbox_state.as_str(),
            "reverted" | "failed" | "cancelled"
        )
    {
        op.txs[index].superseded = true;
    }

    // Allowance: TOLLY routers pull the gross amount; SwapRouter02 gets net == gross (no fee).
    let allowance = match chain::erc20_allowance(token_in, address, spender) {
        Ok(a) => a,
        Err(e) => return petal::error(-4, e),
    };
    if allowance < amount_in {
        let data = abi::erc20_approve(spender, amount_in);
        if let Err(e) = tx::preflight(address, token_in, &data, "approve pre-flight") {
            return record_failure(
                trace,
                &mut op,
                Step::Approve,
                &fail(-4, "preflight-reverted", e, true),
                now,
            );
        }
        return stage_step(
            trace,
            &mut op,
            wallet,
            Step::Approve,
            token_in,
            &data,
            attempt_params,
            Some(spender),
            amount_in,
            None,
            now,
        );
    }

    // The swap itself.
    let data = match (router_call, detail.provenance) {
        ("exactInputSingle", _) => abi::swap_router02_exact_input_single(
            token_in,
            token_out,
            chosen.venue.fee.unwrap_or(0),
            address,
            amount_in,
            floor,
        ),
        ("swapWithToll", _) => abi::multi_router_swap_with_toll(
            token_in,
            token_out,
            chosen.venue.fee.unwrap_or(0),
            amount_in,
            floor,
        ),
        ("swapWithTollV2", _) => abi::multi_router_swap_with_toll_v2(
            token_in,
            token_out,
            chosen.venue.factory.unwrap_or(Address::ZERO),
            chosen.venue.fee_bps.unwrap_or(0),
            amount_in,
            floor,
        ),
        _ => {
            return record_failure(
                trace,
                &mut op,
                Step::Swap,
                &fail(-4, "execution-plan-invalid", "unknown router call", false),
                now,
            );
        }
    };
    if router_call == "swapWithTollV2" && (chosen.venue.factory.is_none() || floor == U256::ZERO) {
        return record_failure(
            trace,
            &mut op,
            Step::Swap,
            &fail(
                -4,
                "execution-plan-invalid",
                "a V2 swap needs the API factory and a non-zero floor",
                false,
            ),
            now,
        );
    }
    let router = if router_call == "exactInputSingle" {
        SWAP_ROUTER02
    } else {
        MULTI_ROUTER
    };
    if let Err(e) = tx::preflight(address, router, &data, "swap pre-flight") {
        return record_failure(
            trace,
            &mut op,
            Step::Swap,
            &fail(-4, "preflight-reverted", e, true),
            now,
        );
    }
    let balance_before = match chain::erc20_balance_of(token_out, address) {
        Ok(b) => b,
        Err(e) => return petal::error(-4, e),
    };
    stage_step(
        trace,
        &mut op,
        wallet,
        Step::Swap,
        router,
        &data,
        attempt_params,
        None,
        amount_in,
        Some(balance_before),
        now,
    )
}

/// Pick the venue to execute on, honouring a pin and `allow_worse_venue`.
fn choose_venue<'a>(quote: &'a Quote, intent: &Intent) -> Result<&'a VenueQuote, Failure> {
    let best_exec = quote.best_executable();
    let best = quote.best();
    let chosen = match intent.venue.as_deref() {
        Some(pinned) => {
            let v = quote.venue_by_id(pinned).ok_or_else(|| {
                fail(
                    -3,
                    "venue-changed",
                    format!("pinned venue {pinned} is not among this token's venues"),
                    true,
                )
            })?;
            if !v.execution.supported {
                return Err(fail(
                    -3,
                    "venue-unsupported",
                    format!(
                        "pinned venue {pinned} is not executable day-1 ({})",
                        v.execution.reason.unwrap_or("unsupported")
                    ),
                    true,
                ));
            }
            if !v.fills() {
                return Err(fail(
                    -3,
                    "venue-changed",
                    format!("pinned venue {pinned} cannot fill this size"),
                    true,
                ));
            }
            if best_exec.is_some_and(|b| !b.venue.id.eq_ignore_ascii_case(pinned))
                && !intent.allow_worse_venue
            {
                return Err(fail(
                    -3,
                    "venue-changed",
                    format!(
                        "pinned venue {pinned} is no longer the executable winner ({}); re-POST with allow_worse_venue:true to keep it",
                        best_exec.map(|b| b.venue.id.as_str()).unwrap_or("")
                    ),
                    true,
                ));
            }
            v
        }
        None => best_exec.ok_or_else(|| {
            let reason = best
                .map(|b| {
                    format!(
                        "best venue {} ({}) is not executable day-1 ({})",
                        b.venue.id,
                        b.venue.kind.name(),
                        b.execution.reason.unwrap_or("unsupported")
                    )
                })
                .unwrap_or_else(|| "no venue can fill this size".into());
            fail(
                -4,
                "quote-unavailable",
                format!("no executable venue: {reason}"),
                true,
            )
        })?,
    };
    if let Some(b) = best
        && !b.venue.id.eq_ignore_ascii_case(&chosen.venue.id)
        && !intent.allow_worse_venue
    {
        return Err(fail(
            -3,
            "better-venue-unsupported",
            format!(
                "the best venue {} ({}) is {}; the executable venue {} is {:.2}% worse; re-POST with allow_worse_venue:true to accept it",
                b.venue.id,
                b.venue.kind.name(),
                if b.execution.supported {
                    "a different executable venue"
                } else {
                    b.execution.reason.unwrap_or("not executable day-1")
                },
                chosen.venue.id,
                quote.worse_than_best_pct.unwrap_or(0.0)
            ),
            true,
        ));
    }
    Ok(chosen)
}

/// Gate on `stage_in_flight`: refuse unless the agent acknowledged it, in
/// which case the marker moves to `unrecorded_stages` (audit) and the
/// operation may stage again. Shared with `launch`.
pub(crate) fn acknowledge_unrecorded_stage(
    op: &mut Operation,
    acknowledged: bool,
    now: u64,
) -> Result<(), DispatchResponse> {
    let Some(marker) = op.stage_in_flight.clone() else {
        return Ok(());
    };
    if !acknowledged {
        return Err(petal::error(
            -2,
            format!(
                "unrecorded-stage: a {} transaction to {} was staged at {} ms but its record could not be written; inspect the wallet's outbox under wallets/{}/chains/arc/outbox/ at the Bloom mount root (confirm or cancel that entry), then re-POST with acknowledge_unrecorded_stage:true",
                serde_json::to_value(marker.step)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                marker.to,
                marker.staged_ms,
                op.wallet
            ),
        ));
    }
    op.unrecorded_stages.push(marker);
    op.stage_in_flight = None;
    op.note = Some(
        "an unrecorded stage was acknowledged; its outbox entry (if any) lives only in Bloom"
            .into(),
    );
    op.updated_ms = now;
    op.finalize_next_action();
    ops::save(op).map_err(|e| petal::error(-4, e))
}

/// Stage one transaction and persist the attempt. Shared with `launch`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stage_step(
    trace: &mut WriteTrace,
    op: &mut Operation,
    wallet: &str,
    step: Step,
    to: Address,
    data: &[u8],
    attempt_params: Value,
    spender: Option<Address>,
    amount_raw: U256,
    balance_before: Option<U256>,
    now: u64,
) -> DispatchResponse {
    // Persist the plan, the live index and the in-flight marker BEFORE the
    // host effect: if the save after `tx_stage` fails, the marker is what
    // stops a re-POST from staging a second live entry (the re-quote would
    // produce different calldata, which the host does not de-duplicate).
    let subject = op.subject();
    if let Err(e) = ops::live_index_set(wallet, op.kind, &subject, &op.id) {
        return petal::error(-4, e);
    }
    op.status = Status::Created;
    op.step = Some(step);
    op.error = None;
    op.stage_in_flight = Some(ops::StageMarker {
        step,
        to: addr_hex(to),
        data_sha256: ops::sha256_hex(data),
        staged_ms: now,
    });
    op.updated_ms = now;
    op.finalize_next_action();
    if let Err(e) = ops::save(op) {
        return petal::error(-4, e);
    }
    let staged = match tx::stage(wallet, to, data) {
        Ok(s) => s,
        // The engine returned an error: nothing was staged, the marker lifts.
        Err(StageError::Denied(message)) => {
            op.stage_in_flight = None;
            let code = if message.to_ascii_lowercase().contains("valuation") {
                "valuation-unavailable"
            } else {
                "policy-denied"
            };
            return record_failure(
                trace,
                op,
                step,
                &fail(
                    -2,
                    code,
                    format!("the host refused to stage: {message}"),
                    false,
                ),
                now,
            );
        }
        Err(StageError::Backend(message)) => {
            op.stage_in_flight = None;
            return record_failure(
                trace,
                op,
                step,
                &fail(
                    -4,
                    "stage-failed",
                    format!("staging failed: {message}"),
                    true,
                ),
                now,
            );
        }
    };
    op.txs.push(TxEntry {
        role: step,
        to: addr_hex(to),
        outbox_id: staged.outbox_id.clone(),
        confirm_path: tx::confirm_path(wallet, &staged.outbox_id),
        confirm_path_note: tx::mount_note(),
        staged_ms: now,
        outbox_state: "pending".into(),
        tx_hash: None,
        outcome: None,
        block_number: None,
        revert_reason: None,
        superseded: false,
        attempt_params,
        spender: spender.map(addr_hex),
        amount_raw: Some(amount_raw.to_string()),
        balance_before_raw: balance_before.map(|b| b.to_string()),
        plan_md: tx::truncate_plan_md(&staged.plan_md),
    });
    op.status = Status::Staged;
    op.step = Some(step);
    op.set_confirm_path(tx::confirm_path(wallet, &staged.outbox_id));
    op.next_action = NextAction::ConfirmInBloom;
    op.error = None;
    op.note = None;
    op.stage_in_flight = None;
    op.updated_ms = now;
    // The tx IS staged. Try the save twice; if it still fails the durable
    // record keeps `created` + `stage_in_flight`, so the next POST refuses
    // until the agent inspects the outbox and acknowledges (see
    // `acknowledge_unrecorded_stage`).
    if let Err(e) = ops::save(op).or_else(|_| ops::save(op)) {
        return petal::error(
            -4,
            format!(
                "unrecorded-stage: transaction staged (outbox_id={}) but the record could not be written: {e}; inspect the outbox, then re-POST with acknowledge_unrecorded_stage:true",
                staged.outbox_id
            ),
        );
    }
    DispatchResponse::Write
}
