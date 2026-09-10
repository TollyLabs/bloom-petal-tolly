//! Launch: `wallets/[wallet]/launch.json` — `TollyPad.createToken` with an
//! optional dev buy (D5: default 0, max 140 USDC, staged as approve then
//! createToken). The salt is drawn from the host RNG at the first stage and
//! frozen in the record so a re-POST reuses it. Logo pinning is out of scope
//! (D8): the agent supplies an already-pinned `imageURI`.

use alloy_primitives::U256;
use petal::DispatchResponse;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::abi::{self, TokenMeta};
use crate::amount::{addr_hex, format_units, parse_decimal, usdc6_to_native18};
use crate::api::Network;
use crate::chain;
use crate::constants::{PAD, USDC, USDC_ERC20_DECIMALS};
use crate::host;
use crate::ops::{self, Kind, Operation, Status, Step};
use crate::policy::{self, MAX_BODY_BYTES, META_MAX_BYTES};
use crate::swap::{acknowledge_unrecorded_stage, stage_step};
use crate::trace::{self, WriteTrace};
use crate::tx;
use crate::wallet::{check_wallet_id, wallet_address};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetaRequest {
    #[serde(rename = "imageURI")]
    pub image_uri: String,
    #[serde(default)]
    pub website: String,
    #[serde(default)]
    pub twitter: String,
    #[serde(default)]
    pub telegram: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    #[serde(rename = "operationId")]
    pub operation_id: String,
    pub name: String,
    pub symbol: String,
    pub meta: MetaRequest,
    #[serde(default)]
    pub dev_buy_usdc: Option<String>,
    /// Required `true` to stage again after a stage whose record could not be
    /// written (`stage_in_flight`); see AGENTS.md "Unrecorded stage".
    #[serde(default)]
    pub acknowledge_unrecorded_stage: Option<bool>,
}

struct Intent {
    id: String,
    name: String,
    symbol: String,
    meta: TokenMeta,
    dev_buy_raw: U256,
    acknowledge_unrecorded_stage: bool,
}

fn validate(request: &LaunchRequest) -> Result<Intent, String> {
    ops::validate_id(&request.operation_id)?;
    // Exactly `launchCall.ts#createTokenArgs`: trims, uppercase ticker.
    let name = request.name.trim().to_owned();
    let symbol = request.symbol.trim().to_ascii_uppercase();
    if name.is_empty() || name.len() > 128 {
        return Err("name must be 1-128 bytes after trimming".into());
    }
    if symbol.is_empty() || symbol.len() > 32 || !symbol.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err("symbol must be 1-32 ASCII letters/digits after trimming".into());
    }
    let meta = TokenMeta {
        image_uri: request.meta.image_uri.trim().to_owned(),
        website: request.meta.website.trim().to_owned(),
        twitter: request.meta.twitter.trim().to_owned(),
        telegram: request.meta.telegram.trim().to_owned(),
    };
    if meta.image_uri.is_empty() {
        return Err("meta.imageURI is required (an already-pinned ipfs:// or https:// URI)".into());
    }
    for (label, value) in [
        ("imageURI", &meta.image_uri),
        ("website", &meta.website),
        ("twitter", &meta.twitter),
        ("telegram", &meta.telegram),
    ] {
        if value.len() > META_MAX_BYTES {
            return Err(format!("meta.{label} exceeds {META_MAX_BYTES} bytes"));
        }
    }
    let dev_buy_raw = match request.dev_buy_usdc.as_deref().map(str::trim) {
        None | Some("0") | Some("") => U256::ZERO,
        Some(value) => {
            parse_decimal(value, USDC_ERC20_DECIMALS).map_err(|e| format!("dev_buy_usdc: {e}"))?
        }
    };
    if dev_buy_raw > policy::max_dev_buy_usdc_raw() {
        return Err(format!(
            "dev_buy_usdc must be at most {} USDC",
            policy::MAX_DEV_BUY_USDC_HUMAN
        ));
    }
    Ok(Intent {
        id: request.operation_id.clone(),
        name,
        symbol,
        meta,
        dev_buy_raw,
        acknowledge_unrecorded_stage: request.acknowledge_unrecorded_stage.unwrap_or(false),
    })
}

/// Read side of `launch.json`.
pub fn launch_description(wallet: &str) -> DispatchResponse {
    if let Err(r) = check_wallet_id(wallet) {
        return r;
    }
    petal::read_json_value(&json!({
        "schema": "tolly.launch-request.v1",
        "description": "Launch a token on TollyPad for this Bloom wallet: a fixed 1B supply minted into a permanently locked single-sided V3 pool (1% tier) quoted in USDC. One write stages at most one transaction (an exact USDC approve to the pad when dev_buy_usdc > 0 and the allowance is short, else createToken).",
        "writes_enabled": policy::writes_enabled(),
        "writes_setting": format!("{}={}", policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE),
        "write_semantics": trace::WRITE_SEMANTICS,
        "last_write": trace::last_write_json(wallet),
        "pad": addr_hex(PAD),
        "body": {
            "operationId": "required; [a-z0-9][a-z0-9._-]{0,63}; bound to (name, symbol, meta, dev_buy_usdc)",
            "name": "required; trimmed",
            "symbol": "required; trimmed and upper-cased",
            "meta": { "imageURI": "required; already-pinned URI, <= 512 bytes", "website": "optional <= 512 bytes", "twitter": "optional <= 512 bytes", "telegram": "optional <= 512 bytes" },
            "dev_buy_usdc": format!("optional decimal USDC, default 0, max {}", policy::MAX_DEV_BUY_USDC_HUMAN),
            "acknowledge_unrecorded_stage": "optional; required true to stage again after the record reports stage_in_flight (an outbox entry this Petal staged but could not record)",
        },
        "notes": [
            "the token address is never predicted; it is read from the TOLLY index after the launch mines (result.token)",
            "the salt is generated by the Petal at the first stage and reused on re-POST",
            "logo pinning is out of scope: supply an imageURI you already pinned",
        ],
        "recent": ops::recent_summary(wallet, Kind::Launch, 5),
    }))
}

/// `wallets/[wallet]/launch.json` write. Every outcome is persisted by the
/// trace (see `trace`): Bloom delivers mounted writes asynchronously.
pub fn route_launch(wallet: &str, body: &[u8]) -> DispatchResponse {
    if let Err(r) = check_wallet_id(wallet) {
        return r;
    }
    let mut trace = WriteTrace::new(Kind::Launch, wallet, body);
    let response = launch_flow(wallet, body, &mut trace);
    trace.finish(response)
}

fn launch_flow(wallet: &str, body: &[u8], trace: &mut WriteTrace) -> DispatchResponse {
    if body.len() > MAX_BODY_BYTES {
        return petal::error(
            -3,
            format!("invalid-request: request body exceeds {MAX_BODY_BYTES} bytes"),
        );
    }
    let request: LaunchRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return petal::error(-3, format!("invalid-request: invalid request JSON: {e}")),
    };
    let echo = serde_json::to_value(&request).unwrap_or(Value::Null);
    trace.parsed(&request.operation_id, echo.clone());
    let intent = match validate(&request) {
        Ok(i) => i,
        Err(e) => return petal::error(-3, format!("invalid-request: {e}")),
    };
    trace.tuple(digest_of(&intent));
    advance(wallet, intent, echo, trace)
}

/// sha256(launch || JCS(name, symbol, meta, dev buy)): the launch tuple.
fn digest_of(intent: &Intent) -> String {
    let economic = json!({
        "kind": "launch",
        "name": intent.name,
        "symbol": intent.symbol,
        "meta": intent.meta,
        "dev_buy_raw": intent.dev_buy_raw.to_string(),
    });
    ops::request_digest(Kind::Launch, &economic)
}

#[allow(clippy::too_many_arguments)]
fn refuse(
    trace: &mut WriteTrace,
    op: &mut Operation,
    step: Step,
    code: i32,
    op_code: &str,
    message: String,
    retryable: bool,
    now: u64,
) -> DispatchResponse {
    op.set_failed(Some(step), op_code, message.clone(), retryable, now);
    op.last_write_ms = Some(now);
    if let Err(e) = ops::save(op) {
        return petal::error(
            -4,
            format!("{op_code}: {message} (and the record could not be updated: {e})"),
        );
    }
    trace.recorded();
    petal::error(code, format!("{op_code}: {message}"))
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
    let network = match Network::current() {
        Ok(n) => n,
        Err(r) => return r,
    };
    trace.network(network);
    let address = match wallet_address(wallet) {
        Ok(a) => a,
        Err(e) => return petal::error(-4, e),
    };
    trace.address(address);
    let digest = digest_of(&intent);
    let mut op = match ops::load(wallet, &intent.id) {
        Ok(Some(op)) => op,
        Ok(None) => {
            let mut fresh = Operation::new(
                &intent.id,
                wallet,
                address,
                Kind::Launch,
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
    if op.is_unbound() {
        op.request_sha256 = digest.clone();
    }
    if op.request_sha256 != digest {
        return petal::error(
            -3,
            "operation-id-bound: operationId already bound to a different launch (name, symbol, meta or dev buy differ); use a new operationId",
        );
    }
    if op.kind != Kind::Launch {
        return petal::error(
            -3,
            "operation-id-bound: operationId already bound to a different operation kind",
        );
    }
    // See `swap::advance`: the resolved network replaces the requested one.
    op.network = network.name().into();
    op.last_write_ms = Some(now);
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
    if let Err(r) = acknowledge_unrecorded_stage(&mut op, intent.acknowledge_unrecorded_stage, now)
    {
        return r;
    }
    match ops::live_conflict(wallet, Kind::Launch, &intent.symbol, &intent.id) {
        Ok(Some(conflict)) => {
            return petal::error(
                -2,
                format!(
                    "live-entry-conflict: {}",
                    conflict.message(&format!("launch {}", intent.symbol))
                ),
            );
        }
        Ok(None) => {}
        Err(e) => return petal::error(-4, e),
    }
    op.request = echo;

    // Salt: drawn once, frozen.
    let salt: [u8; 32] = match op.plan.salt.as_deref() {
        Some(hex) => match abi::decode_hex0x(hex)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
        {
            Some(s) => s,
            None => return petal::error(-4, "record salt is malformed"),
        },
        None => match host::random_bytes(32) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut s = [0u8; 32];
                s.copy_from_slice(&bytes);
                s
            }
            _ => return petal::error(-4, "host RNG did not yield 32 bytes"),
        },
    };
    op.plan.salt = Some(abi::hex0x(&salt));
    op.plan.name = Some(intent.name.clone());
    op.plan.launch_symbol = Some(intent.symbol.clone());
    op.plan.meta = Some(intent.meta.clone());
    op.plan.dev_buy_raw = Some(intent.dev_buy_raw.to_string());
    op.plan.spender = Some(addr_hex(PAD));
    op.plan.router_call = Some("createToken".into());
    op.plan.provenance = Some("pad".into());
    op.plan.decimals = Some(crate::constants::PAD_TOKEN_DECIMALS);
    let attempt_params =
        json!({ "dev_buy_raw": intent.dev_buy_raw.to_string(), "salt": abi::hex0x(&salt) });

    // Funding: the dev buy and gas come from the same balance.
    let native = match chain::eth_get_balance(address) {
        Ok(b) => b,
        Err(e) => return petal::error(-4, e),
    };
    let native_needed =
        usdc6_to_native18(intent.dev_buy_raw).saturating_add(policy::gas_reserve_wei());
    if native < native_needed {
        return refuse(
            trace,
            &mut op,
            Step::Create,
            -3,
            "insufficient-funds",
            format!(
                "native USDC balance {} is below the {} needed (dev buy plus a {} USDC gas reserve)",
                format_units(native, 18),
                format_units(native_needed, 18),
                format_units(policy::gas_reserve_wei(), 18)
            ),
            true,
            now,
        );
    }
    if op.status == Status::Failed
        && let Some(index) = op.latest_live()
        && matches!(
            op.txs[index].outbox_state.as_str(),
            "reverted" | "failed" | "cancelled"
        )
    {
        op.txs[index].superseded = true;
    }

    if intent.dev_buy_raw > U256::ZERO {
        let erc20 = match chain::erc20_balance_of(USDC, address) {
            Ok(b) => b,
            Err(e) => return petal::error(-4, e),
        };
        if erc20 < intent.dev_buy_raw {
            return refuse(
                trace,
                &mut op,
                Step::Create,
                -3,
                "insufficient-funds",
                format!(
                    "USDC balance {} is below the dev buy {}",
                    format_units(erc20, 6),
                    format_units(intent.dev_buy_raw, 6)
                ),
                true,
                now,
            );
        }
        let allowance = match chain::erc20_allowance(USDC, address, PAD) {
            Ok(a) => a,
            Err(e) => return petal::error(-4, e),
        };
        if allowance < intent.dev_buy_raw {
            let data = abi::erc20_approve(PAD, intent.dev_buy_raw);
            if let Err(e) = tx::preflight(address, USDC, &data, "approve pre-flight") {
                return refuse(
                    trace,
                    &mut op,
                    Step::Approve,
                    -4,
                    "preflight-reverted",
                    e,
                    true,
                    now,
                );
            }
            return stage_step(
                trace,
                &mut op,
                wallet,
                Step::Approve,
                USDC,
                &data,
                attempt_params,
                Some(PAD),
                intent.dev_buy_raw,
                None,
                now,
            );
        }
    }

    let data = abi::pad_create_token(
        &intent.name,
        &intent.symbol,
        &intent.meta,
        salt,
        intent.dev_buy_raw,
    );
    if let Err(e) = tx::preflight(address, PAD, &data, "createToken pre-flight") {
        return refuse(
            trace,
            &mut op,
            Step::Create,
            -4,
            "preflight-reverted",
            format!("{e} (banned name/symbol, missing logo, or an over-long field revert here)"),
            true,
            now,
        );
    }
    stage_step(
        trace,
        &mut op,
        wallet,
        Step::Create,
        PAD,
        &data,
        attempt_params,
        None,
        intent.dev_buy_raw,
        None,
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str, symbol: &str, image: &str, dev_buy: Option<&str>) -> LaunchRequest {
        LaunchRequest {
            operation_id: "launch-moss".into(),
            name: name.into(),
            symbol: symbol.into(),
            meta: MetaRequest {
                image_uri: image.into(),
                website: " https://moss.example ".into(),
                twitter: "".into(),
                telegram: "".into(),
            },
            dev_buy_usdc: dev_buy.map(str::to_owned),
            acknowledge_unrecorded_stage: None,
        }
    }

    #[test]
    fn validation_mirrors_create_token_args() {
        let intent =
            validate(&request("  Moss Coin  ", " moss ", " ipfs://x ", Some("5"))).unwrap();
        assert_eq!(intent.name, "Moss Coin");
        assert_eq!(intent.symbol, "MOSS");
        assert_eq!(intent.meta.image_uri, "ipfs://x");
        assert_eq!(intent.meta.website, "https://moss.example");
        assert_eq!(intent.dev_buy_raw, U256::from(5_000_000u64));
        assert_eq!(
            validate(&request("A", "b", "ipfs://x", None))
                .unwrap()
                .dev_buy_raw,
            U256::ZERO
        );
        assert_eq!(
            validate(&request("A", "b", "ipfs://x", Some("0")))
                .unwrap()
                .dev_buy_raw,
            U256::ZERO
        );
        assert_eq!(
            validate(&request("A", "b", "ipfs://x", Some("140")))
                .unwrap()
                .dev_buy_raw,
            U256::from(140_000_000u64)
        );
    }

    #[test]
    fn validation_rejects_bad_launches() {
        assert!(validate(&request("", "MOSS", "ipfs://x", None)).is_err());
        assert!(validate(&request("Moss", "", "ipfs://x", None)).is_err());
        assert!(validate(&request("Moss", "MO SS", "ipfs://x", None)).is_err());
        assert!(
            validate(&request("Moss", "MOSS", "   ", None)).is_err(),
            "logo required"
        );
        assert!(
            validate(&request("Moss", "MOSS", &"x".repeat(513), None)).is_err(),
            "512-byte meta cap"
        );
        assert!(
            validate(&request("Moss", "MOSS", "ipfs://x", Some("140.000001"))).is_err(),
            "dev buy cap"
        );
        assert!(validate(&request("Moss", "MOSS", "ipfs://x", Some("-1"))).is_err());
        let mut bad_id = request("Moss", "MOSS", "ipfs://x", None);
        bad_id.operation_id = "Bad Id".into();
        assert!(validate(&bad_id).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let body =
            br#"{"operationId":"x","name":"a","symbol":"b","meta":{"imageURI":"i"},"extra":1}"#;
        assert!(serde_json::from_slice::<LaunchRequest>(body).is_err());
        let body =
            br#"{"operationId":"x","name":"a","symbol":"b","meta":{"imageURI":"i","logo":"x"}}"#;
        assert!(serde_json::from_slice::<LaunchRequest>(body).is_err());
    }
}
