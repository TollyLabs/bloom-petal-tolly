//! Every write leaves a readable trace.
//!
//! On the mounted filesystem Bloom delivers a Petal write asynchronously: the
//! writer's `write()` succeeds before the route runs, and the route's error
//! response is logged by the daemon and never shown to the agent (only
//! `bloom vfs write` returns it synchronously; verified on Bloom v0.2.1). A
//! refusal that lives only in the response is therefore invisible. This
//! module makes every outcome discoverable through reads:
//!
//! - the operation record (`tolly/ops/{wallet}/{id}`) is created (unbound)
//!   or advanced to `failed` for a refused write whose body parsed and whose
//!   `operationId` is unclaimed, unbound, or owned by the same economic
//!   tuple (or bound but with nothing staged); a record whose truth
//!   lives elsewhere (a live outbox entry, a mined step, a completed or
//!   terminal operation, an unrecorded stage) keeps its status and gets the
//!   refusal appended to its bounded `refusals[]`;
//! - a per-wallet marker (`tolly/lastwrite/{wallet}`) describes the last
//!   write to any of the wallet's writable routes, accepted or refused,
//!   including writes whose body never parsed.
//!
//! The route response itself is never changed here.

use alloy_primitives::{Address, U256};
use petal::DispatchResponse;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::amount::{addr_hex, parse_any_address, parse_decimal};
use crate::api::{self, Network};
use crate::host;
use crate::ops::{self, Kind, OpError, Operation, Refusal};
use crate::sanitize_host_error;
use crate::wallet::wallet_address;

pub const LASTWRITE_SCHEMA: &str = "tolly.lastwrite.v1";
pub const LASTWRITE_PREFIX: &str = "tolly/lastwrite/";
const MAX_MARKER_BYTES: usize = 8 * 1024;
const MAX_MESSAGE_CHARS: usize = 512;
const MAX_ID_ECHO_CHARS: usize = 96;

/// What an agent reads to learn how a write ended when no record says so.
pub const WRITE_SEMANTICS: &str = "On the mounted filesystem write() always succeeds: Bloom delivers Petal writes asynchronously and the route's answer is not returned to the writer. Right after every write read this file first: last_write — check body_sha256 against the bytes you wrote, then record_effect/note say whether and where the outcome landed. Reading this file also reconciles this route's in-flight operations against Bloom's outbox (see reconciled; the host binds outbox inspection to the route that staged the entry, so no other file can do it), and operations/<operationId>.json is a cached projection of the stored record (~5 s) that lags until you read this file; if record is set, read it after this file. Via `bloom vfs write` the same errors are returned synchronously.";

pub fn marker_key(wallet: &str) -> String {
    format!("{LASTWRITE_PREFIX}{wallet}")
}

/// The per-wallet marker: the last write to `buy.json`, `sell.json` or
/// `launch.json` for this wallet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastWrite {
    pub schema: String,
    /// `buy` | `sell` | `launch`.
    pub route: String,
    pub ts_ms: u64,
    /// `accepted` (the route returned success) or `refused`.
    pub outcome: String,
    /// `0` when accepted, else the route error code (`-1`..`-4`).
    pub response_code: i32,
    /// The `operationId` of the parsed body, when the body parsed.
    #[serde(rename = "operationId")]
    pub operation_id: Option<String>,
    pub body_sha256: String,
    pub body_bytes: usize,
    pub error: Option<OpError>,
    /// `operations/<id>.json` when a record holds this outcome.
    pub record: Option<String>,
    /// `created` | `failed` | `refusal_appended` | `accepted` | `none`.
    pub record_effect: String,
    /// Why no record was touched, when `record_effect` is `none`.
    pub note: Option<String>,
}

/// Read the marker for a wallet (`None` when the wallet never wrote).
pub fn last_write(wallet: &str) -> Option<LastWrite> {
    match host::store_get(&marker_key(wallet), MAX_MARKER_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes).ok(),
        Err(_) => None,
    }
}

/// Marker projection for the writable routes' read side.
pub fn last_write_json(wallet: &str) -> Value {
    last_write(wallet)
        .and_then(|m| serde_json::to_value(m).ok())
        .unwrap_or(Value::Null)
}

/// The economic tuple of a write, as far as it can be known without the host.
pub enum Tuple {
    Known(String),
    /// A sell with a decimal amount: the digest needs the token's decimals,
    /// which a record that already planned the sell carries.
    SellNeedsDecimals {
        token: Address,
        amount_human: String,
    },
    Unknown,
}

/// The economic digest shared by the write flows and the trace.
pub fn swap_digest(kind: Kind, token: Address, amount_spec: Option<U256>) -> String {
    let economic = json!({
        "kind": kind.name(),
        "token": addr_hex(token),
        "amount_raw": amount_spec.map(|a| a.to_string()).unwrap_or_else(|| "all".into()),
    });
    ops::request_digest(kind, &economic)
}

enum Ownership {
    Owned,
    Foreign,
    Unverifiable,
}

/// Accumulates what a write flow learned, then persists the outcome.
pub struct WriteTrace {
    kind: Kind,
    wallet: String,
    now: u64,
    body_sha256: String,
    body_bytes: usize,
    operation_id: Option<String>,
    id_valid: bool,
    echo: Option<Value>,
    tuple: Tuple,
    address: Option<Address>,
    network: Option<String>,
    recorded: bool,
}

impl WriteTrace {
    pub fn new(kind: Kind, wallet: &str, body: &[u8]) -> Self {
        Self {
            kind,
            wallet: wallet.to_owned(),
            now: host::now_ms(),
            body_sha256: ops::sha256_hex(body),
            body_bytes: body.len(),
            operation_id: None,
            id_valid: false,
            echo: None,
            tuple: Tuple::Unknown,
            address: None,
            network: None,
            recorded: false,
        }
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    /// The body parsed; `id` is what it named.
    pub fn parsed(&mut self, id: &str, echo: Value) {
        self.operation_id = Some(bounded(id, MAX_ID_ECHO_CHARS));
        self.id_valid = ops::validate_id(id).is_ok();
        self.echo = Some(echo);
    }

    /// Best-effort offline tuple for a buy or sell (no host call).
    pub fn swap_tuple(&mut self, token: &str, amount_human: &str) {
        let Some(token) = parse_any_address(token) else {
            return;
        };
        let amount_human = amount_human.trim();
        self.tuple = match self.kind {
            Kind::Buy => match parse_decimal(amount_human, crate::constants::USDC_ERC20_DECIMALS) {
                Ok(raw) => Tuple::Known(swap_digest(Kind::Buy, token, Some(raw))),
                Err(_) => Tuple::Unknown,
            },
            Kind::Sell if amount_human == "all" => {
                Tuple::Known(swap_digest(Kind::Sell, token, None))
            }
            Kind::Sell => Tuple::SellNeedsDecimals {
                token,
                amount_human: amount_human.to_owned(),
            },
            Kind::Launch => Tuple::Unknown,
        };
    }

    pub fn tuple(&mut self, digest: String) {
        self.tuple = Tuple::Known(digest);
    }

    pub fn address(&mut self, address: Address) {
        self.address = Some(address);
    }

    pub fn network(&mut self, network: Network) {
        self.network = Some(network.name().to_owned());
    }

    /// The flow already persisted this outcome in the record.
    pub fn recorded(&mut self) {
        self.recorded = true;
    }

    fn ownership(&self, op: &Operation) -> Ownership {
        if op.is_unbound() {
            return Ownership::Owned;
        }
        // A bound record with nothing staged and nothing in flight protects
        // nothing: a refusal whose tuple cannot be computed here (a decimal
        // sell before the record planned the token's decimals, an unparseable
        // token or amount) still lands on it. A known, different tuple stays
        // foreign: the flow refuses it `operation-id-bound` and the marker
        // says so.
        let nothing_staged = op.txs.is_empty() && op.stage_in_flight.is_none();
        let digest = match &self.tuple {
            Tuple::Known(d) => d.clone(),
            Tuple::SellNeedsDecimals {
                token,
                amount_human,
            } => match op
                .plan
                .decimals
                .and_then(|decimals| parse_decimal(amount_human, decimals).ok())
            {
                Some(raw) => swap_digest(Kind::Sell, *token, Some(raw)),
                None if nothing_staged => return Ownership::Owned,
                None => return Ownership::Unverifiable,
            },
            Tuple::Unknown if nothing_staged => return Ownership::Owned,
            Tuple::Unknown => return Ownership::Unverifiable,
        };
        if digest == op.request_sha256 {
            Ownership::Owned
        } else {
            Ownership::Foreign
        }
    }

    /// Persist the outcome and return the response unchanged.
    pub fn finish(self, response: DispatchResponse) -> DispatchResponse {
        let (response_code, message) = match &response {
            DispatchResponse::Error { code, message } => (*code, message.clone()),
            DispatchResponse::Write | DispatchResponse::Read(_) => (0, String::new()),
        };
        let refused = response_code != 0;
        let error = refused.then(|| classify(response_code, &message));
        let (record, effect, note) = self.touch_record(error.as_ref(), response_code);
        let marker = LastWrite {
            schema: LASTWRITE_SCHEMA.into(),
            route: self.kind.name().into(),
            ts_ms: self.now,
            outcome: if refused { "refused" } else { "accepted" }.into(),
            response_code,
            operation_id: self.operation_id.clone(),
            body_sha256: self.body_sha256.clone(),
            body_bytes: self.body_bytes,
            error,
            record,
            record_effect: effect.into(),
            note,
        };
        // Best effort: a marker that cannot be written changes nothing about
        // the write itself, and there is nowhere else to report it.
        if let Ok(bytes) = serde_json::to_vec(&marker) {
            let _ = host::store_put(&marker_key(&self.wallet), &bytes);
        }
        response
    }

    /// Create or advance the record for this write. Returns the record path
    /// (when one holds the outcome), the effect, and a note explaining why
    /// the record was left alone.
    fn touch_record(
        &self,
        error: Option<&OpError>,
        response_code: i32,
    ) -> (Option<String>, &'static str, Option<String>) {
        let Some(id) = self.operation_id.as_deref() else {
            return (
                None,
                "none",
                Some("the body did not parse; no operationId".into()),
            );
        };
        if !self.id_valid {
            return (
                None,
                "none",
                Some("operationId is not valid ([a-z0-9][a-z0-9._-]{0,63}); no record".into()),
            );
        }
        let path = format!("operations/{id}.json");
        let existing = match ops::load(&self.wallet, id) {
            Ok(existing) => existing,
            Err(e) => return (None, "none", Some(format!("record unreadable: {e}"))),
        };
        let Some(mut op) = existing else {
            let Some(error) = error else {
                return (
                    None,
                    "none",
                    Some("accepted, but no record exists for this operationId".into()),
                );
            };
            if self.recorded {
                return (Some(path), "failed", None);
            }
            return self.create_failed(id, error, path);
        };
        if op.kind != self.kind {
            return (
                None,
                "none",
                Some(format!(
                    "operationId belongs to a {} operation; the record was left untouched",
                    op.kind.name()
                )),
            );
        }
        if self.recorded {
            return (Some(path), "failed", None);
        }
        match self.ownership(&op) {
            Ownership::Owned => {}
            Ownership::Foreign => {
                return (
                    None,
                    "none",
                    Some("operationId is bound to a different request; the record was left untouched".into()),
                );
            }
            Ownership::Unverifiable => {
                return (
                    None,
                    "none",
                    Some("the record could not be matched to this request (its economic tuple is unknown here); left untouched".into()),
                );
            }
        }
        let effect = match error {
            None => {
                if op.last_write_ms == Some(self.now) {
                    return (Some(path), "accepted", None);
                }
                "accepted"
            }
            Some(error) if op.refusal_rewrites_status() => {
                op.set_failed(
                    None,
                    &error.code,
                    error.message.clone(),
                    error.retryable,
                    self.now,
                );
                "failed"
            }
            Some(error) => {
                op.push_refusal(Refusal {
                    ts_ms: self.now,
                    response_code,
                    code: error.code.clone(),
                    message: error.message.clone(),
                    retryable: error.retryable,
                });
                op.updated_ms = self.now;
                "refusal_appended"
            }
        };
        op.last_write_ms = Some(self.now);
        match ops::save(&op) {
            Ok(()) => (Some(path), effect, None),
            Err(e) => (
                None,
                "none",
                Some(format!("record could not be written: {e}")),
            ),
        }
    }

    fn create_failed(
        &self,
        id: &str,
        error: &OpError,
        path: String,
    ) -> (Option<String>, &'static str, Option<String>) {
        let address = match self.address {
            Some(a) => a,
            None => match wallet_address(&self.wallet) {
                Ok(a) => a,
                Err(e) => return (None, "none", Some(format!("no record: {e}"))),
            },
        };
        // A refusal never binds, even when the tuple is known offline: there
        // is nothing staged to protect, and binding would make the only
        // remedy (a corrected body under the same id) `operation-id-bound`.
        // The first write past validation and the gates binds the record.
        let network = self
            .network
            .clone()
            .unwrap_or_else(api::requested_network_name);
        let mut op = Operation::new(
            id,
            &self.wallet,
            address,
            self.kind,
            &network,
            String::new(),
            self.echo.clone().unwrap_or(Value::Null),
            self.now,
        );
        op.set_failed(
            None,
            &error.code,
            error.message.clone(),
            error.retryable,
            self.now,
        );
        op.last_write_ms = Some(self.now);
        match ops::claim(&op) {
            Ok(true) => (Some(path), "created", None),
            Ok(false) => (
                None,
                "none",
                Some("the record appeared concurrently; read it".into()),
            ),
            Err(e) => (
                None,
                "none",
                Some(format!("record could not be created: {e}")),
            ),
        }
    }
}

/// Operation error codes and whether a re-POST can help. The first block is
/// the write-time refusal vocabulary; the second mirrors the flows' own
/// `record_failure` codes so a refusal classified here agrees with them.
const CODES: &[(&str, bool)] = &[
    ("writes-disabled", true),
    ("network-setting-invalid", true),
    ("live-entry-conflict", true),
    ("unrecorded-stage", false),
    ("invalid-request", true),
    ("operation-id-bound", false),
    ("not-found", true),
    ("backend", true),
    ("denied", false),
    ("reverted", true),
    ("expired-or-dropped", true),
    ("cancelled", true),
    ("stage-failed", true),
    ("quote-unavailable", true),
    ("fee-check-unavailable", true),
    ("venue-changed", true),
    ("venue-unsupported", true),
    ("better-venue-unsupported", true),
    ("below-requested-floor", true),
    ("insufficient-funds", true),
    ("cap-exceeded", true),
    ("preflight-reverted", true),
    ("provenance-mismatch", true),
    ("policy-denied", false),
    ("valuation-unavailable", false),
    ("fee-mismatch", false),
    ("execution-plan-invalid", false),
];

/// Turn a route error into the record's `error`: a known `<code>: message`
/// prefix names the code; otherwise the response code picks a generic one.
pub fn classify(response_code: i32, message: &str) -> OpError {
    let message = sanitize_host_error(message);
    if let Some((prefix, rest)) = message.split_once(": ")
        && let Some((code, retryable)) = CODES.iter().find(|(c, _)| *c == prefix)
    {
        return OpError {
            code: (*code).into(),
            message: bounded(rest, MAX_MESSAGE_CHARS),
            retryable: *retryable,
        };
    }
    let code = match response_code {
        -1 => "not-found",
        -2 => "denied",
        -3 => "invalid-request",
        _ => "backend",
    };
    let retryable = CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, r)| *r)
        .unwrap_or(true);
    OpError {
        code: code.into(),
        message: bounded(&message, MAX_MESSAGE_CHARS),
        retryable,
    }
}

/// Whether a code is retryable per the table (for docs/tests).
pub fn retryable(code: &str) -> Option<bool> {
    CODES.iter().find(|(c, _)| *c == code).map(|(_, r)| *r)
}

fn bounded(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{cut}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_uses_the_prefix_and_falls_back_on_the_response_code() {
        let e = classify(-2, "writes-disabled: set the runtime setting");
        assert_eq!(e.code, "writes-disabled");
        assert_eq!(e.message, "set the runtime setting");
        assert!(e.retryable);
        let e = classify(-3, "operationId already bound to a different request");
        assert_eq!(e.code, "invalid-request");
        let e = classify(-3, "operation-id-bound: operationId already bound");
        assert_eq!(e.code, "operation-id-bound");
        assert!(!e.retryable);
        let e = classify(-1, "unknown token");
        assert_eq!(e.code, "not-found");
        let e = classify(-4, "eth_call: https://arc.example/rpc/KEY down");
        assert_eq!(e.code, "backend");
        assert!(!e.message.contains("KEY"));
        // An unknown prefix is not a code.
        let e = classify(-4, "amount_usdc: must be positive");
        assert_eq!(e.code, "backend");
        assert_eq!(e.message, "amount_usdc: must be positive");
        let e = classify(-4, "fee-mismatch: the router would charge more");
        assert!(!e.retryable);
    }

    #[test]
    fn messages_are_bounded() {
        let long = "x".repeat(2000);
        let e = classify(-3, &format!("invalid-request: {long}"));
        assert!(e.message.chars().count() <= MAX_MESSAGE_CHARS);
        assert!(e.message.ends_with("..."));
    }
}
