//! The durable operation record (`tolly/ops/{wallet}/{id}`) and its state
//! machine.
//!
//! One write advances an operation by AT MOST one staged transaction. A write
//! never claims completion: the record is reconciled from the host's
//! `tx_inspect` (critique B3 mapping) and domain evidence (critique B1/D11: a
//! token balance delta for swaps, the creator's launch index for launches),
//! never from receipt logs, which `bloom:chain` cannot serve. Reconciliation
//! runs from the READ of the route that staged the entry (`buy.json`,
//! `sell.json`, `launch.json`; see `route_read_side`): Bloom binds outbox
//! inspection to the staging route, so `operations/<id>.json` is a pure
//! projection of the stored record (`read_operation`).
//!
//! Vocabulary (`status`): `created` (id claimed, nothing staged), `staged`
//! (entry pending in Bloom's outbox, the owner confirms), `broadcast` (sent,
//! no receipt), `confirmed` (mined successfully; for an approve step the
//! agent re-POSTs, for a swap/create step completion evidence is pending),
//! `completed`, `failed` (`error.retryable` says whether a re-POST re-quotes
//! and re-stages the same step), `unknown` (the outbox entry is no longer
//! inspectable; nothing is re-staged).

use alloy_primitives::{Address, U256};
use petal::{DispatchResponse, HostStatus, OutboxInspection, SdkError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::abi::TokenMeta;
use crate::amount::{addr_hex, format_units, parse_u256_decimal};
use crate::api::{ApiRoute, Network, fetch_json, launch_rows};
use crate::chain;
use crate::constants::{CHAIN, USDC, USDC_ERC20_DECIMALS};
use crate::host;
use crate::policy::{OPS_SCAN_MAX_OPS, RECONCILE_MAX_OPS};
use crate::sanitize_host_error;
use crate::tx;
use crate::wallet::check_wallet_id;

pub const SCHEMA: &str = "tolly.operation.v1";
pub const STORE_PREFIX: &str = "tolly/ops/";
/// One key per `(wallet, kind, subject)` naming the operation that last
/// staged for it (critique M1): the live-entry check is one read, not a scan.
pub const LIVE_PREFIX: &str = "tolly/live/";
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_LIST_BYTES: usize = 256 * 1024;
const MAX_LIVE_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Buy,
    Sell,
    Launch,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
            Self::Launch => "launch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Created,
    Staged,
    Broadcast,
    Confirmed,
    Completed,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Approve,
    Swap,
    Create,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    /// The owner confirms the pending outbox entry by writing to
    /// `confirm_path`, which is RELATIVE to the Bloom mount root
    /// (`confirm_path_note` says so; `cancel_hint` says how to cancel instead).
    ConfirmInBloom,
    /// Broadcast or awaiting completion evidence: poll this file.
    Wait,
    /// POST the same body again to stage the next transaction.
    Repost,
    /// Retryable failure: POST again (execution parameters may change).
    Retry,
    /// The outbox entry is not inspectable; inspect Bloom's outbox by hand.
    Inspect,
    /// Terminal.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxEntry {
    pub role: Step,
    pub to: String,
    pub outbox_id: String,
    /// `wallets/<wallet>/chains/arc/outbox/pending/<outbox_id>/confirm`,
    /// relative to the Bloom mount root (`tx::confirm_path`).
    pub confirm_path: String,
    /// Where `confirm_path` is rooted (`tx::MOUNT_NOTE`).
    #[serde(default = "crate::tx::mount_note")]
    pub confirm_path_note: String,
    pub staged_ms: u64,
    /// Host state as last inspected: pending|sent|success|reverted|failed|cancelled|unknown.
    pub outbox_state: String,
    pub tx_hash: Option<String>,
    pub outcome: Option<String>,
    pub block_number: Option<u64>,
    pub revert_reason: Option<String>,
    /// Set only after the host reported this entry failed/reverted/cancelled
    /// and a new attempt was staged for the same step (critique M1).
    pub superseded: bool,
    /// Execution parameters of this attempt (mutable per attempt, critique M2).
    pub attempt_params: Value,
    pub spender: Option<String>,
    pub amount_raw: Option<String>,
    /// `balanceOf(wallet)` of the output token when the swap was staged.
    pub balance_before_raw: Option<String>,
    pub plan_md: String,
}

/// Written to the record BEFORE `tx_stage` and cleared by the save that
/// records the resulting `txs[]` entry. If that save fails, the marker
/// survives: an outbox entry may be live that no `txs[]` entry names, so a
/// re-POST refuses to stage again until the agent acknowledges it (critique
/// M1: never two live entries for one operation).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageMarker {
    pub step: Step,
    pub to: String,
    pub data_sha256: String,
    pub staged_ms: u64,
}

/// Frozen at the first stage; execution parameters may be refreshed on a
/// later attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub token: Option<String>,
    pub symbol: Option<String>,
    pub decimals: Option<u32>,
    pub provenance: Option<String>,
    pub venue: Option<Value>,
    pub spender: Option<String>,
    pub router_call: Option<String>,
    pub amount_in_raw: Option<String>,
    pub interface_fee_raw: Option<String>,
    pub amount_to_pool_raw: Option<String>,
    pub quote_out_raw: Option<String>,
    pub slippage_bps: Option<u32>,
    pub amount_out_minimum_raw: Option<String>,
    pub quoted_ms: Option<u64>,
    pub name: Option<String>,
    pub launch_symbol: Option<String>,
    pub meta: Option<TokenMeta>,
    pub dev_buy_raw: Option<String>,
    pub salt: Option<String>,
}

/// A write this operation refused while its record could not be rewritten
/// (a live outbox entry, a mined step, a completed or terminal operation, an
/// unrecorded stage): the refusal is appended here instead, bounded to the
/// newest `MAX_REFUSALS`, and `status` is left alone. Bloom delivers mounted
/// writes asynchronously, so this list is where such a refusal is visible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub ts_ms: u64,
    /// The route response code the write would have returned (`-1`..`-4`).
    pub response_code: i32,
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

pub const MAX_REFUSALS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub schema: String,
    pub id: String,
    pub wallet: String,
    pub wallet_address: String,
    pub kind: Kind,
    pub network: String,
    pub chain: String,
    /// sha256(kind || JCS(economic tuple)); binds the id to what is being bought/sold/launched.
    pub request_sha256: String,
    pub status: Status,
    pub step: Option<Step>,
    pub next_action: NextAction,
    /// While `staged`: the pending entry's confirm file, relative to the
    /// Bloom mount root (see `confirm_path_note`).
    pub confirm_path: Option<String>,
    /// Set with `confirm_path`: where it is rooted (`tx::MOUNT_NOTE`).
    #[serde(default)]
    pub confirm_path_note: Option<String>,
    /// Set with `confirm_path`: how the owner cancels instead
    /// (`tx::CANCEL_HINT`).
    #[serde(default)]
    pub cancel_hint: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub request: Value,
    pub plan: Plan,
    pub txs: Vec<TxEntry>,
    pub result: Option<Value>,
    pub error: Option<OpError>,
    pub note: Option<String>,
    /// A stage whose `txs[]` entry could not be persisted (see `StageMarker`).
    #[serde(default)]
    pub stage_in_flight: Option<StageMarker>,
    /// Markers the agent acknowledged with `acknowledge_unrecorded_stage`
    /// (audit trail; these entries live only in Bloom's outbox).
    #[serde(default)]
    pub unrecorded_stages: Vec<StageMarker>,
    /// Refusals recorded while the record was protected (see `Refusal`).
    #[serde(default)]
    pub refusals: Vec<Refusal>,
    /// When a write last addressed this operation (accepted or refused).
    #[serde(default)]
    pub last_write_ms: Option<u64>,
}

impl Operation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        wallet: &str,
        wallet_address: Address,
        kind: Kind,
        network: &str,
        request_sha256: String,
        request: Value,
        now_ms: u64,
    ) -> Self {
        Self {
            schema: SCHEMA.into(),
            id: id.into(),
            wallet: wallet.into(),
            wallet_address: addr_hex(wallet_address),
            kind,
            network: network.into(),
            chain: CHAIN.into(),
            request_sha256,
            status: Status::Created,
            step: None,
            next_action: NextAction::Repost,
            confirm_path: None,
            confirm_path_note: None,
            cancel_hint: None,
            created_ms: now_ms,
            updated_ms: now_ms,
            request,
            plan: Plan::default(),
            txs: Vec::new(),
            result: None,
            error: None,
            note: None,
            stage_in_flight: None,
            unrecorded_stages: Vec::new(),
            refusals: Vec::new(),
            last_write_ms: None,
        }
    }

    /// A record created by a refused write carries no economic digest yet;
    /// the first write that gets past validation binds it (the flows set
    /// `request_sha256` on load).
    pub fn is_unbound(&self) -> bool {
        self.request_sha256.is_empty()
    }

    /// Whether a refusal may rewrite this record's `status`/`error`. Never
    /// while an outbox entry may be live (`staged`, `broadcast`, `unknown`,
    /// `stage_in_flight`), after a step mined (`confirmed`), or once the
    /// operation is completed or terminally failed: the refusal is appended
    /// to `refusals[]` instead and the status stays.
    pub fn refusal_rewrites_status(&self) -> bool {
        self.stage_in_flight.is_none()
            && !self.is_terminal()
            && matches!(self.status, Status::Created | Status::Failed)
    }

    /// Append a refusal, keeping only the newest `MAX_REFUSALS`.
    pub fn push_refusal(&mut self, refusal: Refusal) {
        self.refusals.push(refusal);
        if self.refusals.len() > MAX_REFUSALS {
            let excess = self.refusals.len() - MAX_REFUSALS;
            self.refusals.drain(..excess);
        }
    }

    pub fn is_terminal(&self) -> bool {
        match self.status {
            Status::Completed => true,
            Status::Failed => self.error.as_ref().is_some_and(|e| !e.retryable),
            _ => false,
        }
    }

    /// Index of the latest attempt that has not been superseded.
    pub fn latest_live(&self) -> Option<usize> {
        self.txs.iter().rposition(|t| !t.superseded)
    }

    pub fn wallet_address(&self) -> Result<Address, String> {
        self.wallet_address
            .parse::<Address>()
            .map_err(|_| "record wallet_address is not an address".into())
    }

    /// The token (buy/sell) or launch symbol (launch) this operation is about,
    /// for the cross-operation live-entry check (critique M1).
    pub fn subject(&self) -> String {
        match self.kind {
            Kind::Buy | Kind::Sell => self
                .plan
                .token
                .clone()
                .or_else(|| {
                    self.request["token"]
                        .as_str()
                        .map(|s| s.to_ascii_lowercase())
                })
                .unwrap_or_default(),
            Kind::Launch => self
                .plan
                .launch_symbol
                .clone()
                .or_else(|| {
                    self.request["symbol"]
                        .as_str()
                        .map(|s| s.trim().to_ascii_uppercase())
                })
                .unwrap_or_default(),
        }
    }

    pub fn set_failed(
        &mut self,
        step: Option<Step>,
        code: &str,
        message: impl Into<String>,
        retryable: bool,
        now_ms: u64,
    ) {
        self.status = Status::Failed;
        if step.is_some() {
            self.step = step;
        }
        self.error = Some(OpError {
            code: code.into(),
            message: message.into(),
            retryable,
        });
        self.clear_confirm_path();
        self.updated_ms = now_ms;
        self.finalize_next_action();
    }

    /// Point the record at a pending entry's confirm file together with the
    /// notes an agent needs to use it: the path is relative to the Bloom
    /// mount root, and the word `cancel` written there cancels.
    pub fn set_confirm_path(&mut self, confirm_path: String) {
        self.confirm_path = Some(confirm_path);
        self.confirm_path_note = Some(tx::MOUNT_NOTE.into());
        self.cancel_hint = Some(tx::CANCEL_HINT.into());
    }

    fn clear_confirm_path(&mut self) {
        self.confirm_path = None;
        self.confirm_path_note = None;
        self.cancel_hint = None;
    }

    pub fn finalize_next_action(&mut self) {
        self.next_action = match self.status {
            Status::Created if self.stage_in_flight.is_some() => NextAction::Inspect,
            Status::Created => NextAction::Repost,
            Status::Staged => NextAction::ConfirmInBloom,
            Status::Broadcast => NextAction::Wait,
            Status::Confirmed => match self.step {
                Some(Step::Approve) => NextAction::Repost,
                _ => NextAction::Wait,
            },
            Status::Completed => NextAction::None,
            Status::Failed => {
                if self.error.as_ref().is_some_and(|e| e.retryable) {
                    NextAction::Retry
                } else {
                    NextAction::None
                }
            }
            Status::Unknown => NextAction::Inspect,
        };
        if self.status == Status::Staged {
            if let Some(path) = self.confirm_path.clone() {
                // Re-emit the notes with the path (a record from before
                // they existed).
                self.set_confirm_path(path);
            }
        } else {
            self.clear_confirm_path();
        }
    }
}

// ---- identity and storage ----

/// `^[a-z0-9][a-z0-9._-]{0,63}$`
pub fn validate_id(id: &str) -> Result<(), String> {
    let bytes = id.as_bytes();
    let ok = !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && id != "."
        && id != "..";
    if ok {
        Ok(())
    } else {
        Err("operationId must match [a-z0-9][a-z0-9._-]{0,63}".into())
    }
}

pub fn store_key(wallet: &str, id: &str) -> String {
    format!("{STORE_PREFIX}{wallet}/{id}")
}

/// sha256 of arbitrary bytes, hex (stage markers).
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// sha256(kind || JCS(economic tuple)), hex.
pub fn request_digest(kind: Kind, economic: &Value) -> String {
    let canonical =
        serde_jcs::to_vec(economic).unwrap_or_else(|_| economic.to_string().into_bytes());
    let mut hasher = Sha256::new();
    hasher.update(kind.name().as_bytes());
    hasher.update(b"\0");
    hasher.update(&canonical);
    hex::encode(hasher.finalize())
}

pub fn load(wallet: &str, id: &str) -> Result<Option<Operation>, String> {
    match host::store_get(&store_key(wallet, id), MAX_RECORD_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("operation record parse: {e}")),
        Err(SdkError::Host(HostStatus::NotFound)) => Ok(None),
        Err(e) => Err(format!(
            "operation record: {}",
            sanitize_host_error(&e.message())
        )),
    }
}

pub fn save(op: &Operation) -> Result<(), String> {
    let bytes = serde_json::to_vec(op).map_err(|e| format!("operation record serialize: {e}"))?;
    host::store_put(&store_key(&op.wallet, &op.id), &bytes)
        .map_err(|e| format!("operation record: {}", sanitize_host_error(&e.message())))
}

/// Atomically claim the id. `Ok(false)` means it already exists.
///
/// The daemon reports an existing key as a message containing "already
/// exists" (`vm.rs` `component_store_put_new`), which the SDK's `host_err`
/// surfaces as `SdkError::Message` or, depending on the prefix, as
/// `Denied`/`Invalid`; every one of those means "exists", none is a failure.
pub fn claim(op: &Operation) -> Result<bool, String> {
    let bytes = serde_json::to_vec(op).map_err(|e| format!("operation record serialize: {e}"))?;
    match host::store_put_new(&store_key(&op.wallet, &op.id), &bytes) {
        Ok(()) => Ok(true),
        Err(SdkError::Host(HostStatus::Denied | HostStatus::Invalid)) => Ok(false),
        Err(SdkError::Message(m)) if m.to_ascii_lowercase().contains("already exists") => Ok(false),
        Err(e) => Err(format!(
            "operation record: {}",
            sanitize_host_error(&e.message())
        )),
    }
}

// ---- live-entry index (critique M1) ----

fn live_key(wallet: &str, kind: Kind, subject: &str) -> String {
    format!(
        "{LIVE_PREFIX}{wallet}/{}/{}",
        kind.name(),
        subject.to_ascii_lowercase()
    )
}

/// Point the `(wallet, kind, subject)` index at `id` before staging for it.
pub fn live_index_set(wallet: &str, kind: Kind, subject: &str, id: &str) -> Result<(), String> {
    let value = serde_json::to_vec(&json!({ "id": id }))
        .map_err(|e| format!("live index serialize: {e}"))?;
    host::store_put(&live_key(wallet, kind, subject), &value)
        .map_err(|e| format!("live index: {}", sanitize_host_error(&e.message())))
}

fn live_index_get(wallet: &str, kind: Kind, subject: &str) -> Result<Option<String>, String> {
    match host::store_get(&live_key(wallet, kind, subject), MAX_LIVE_BYTES) {
        Ok(bytes) => {
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|e| format!("live index parse: {e}"))?;
            Ok(value["id"].as_str().map(str::to_owned))
        }
        Err(SdkError::Host(HostStatus::NotFound)) => Ok(None),
        Err(e) => Err(format!("live index: {}", sanitize_host_error(&e.message()))),
    }
}

fn live_index_clear(wallet: &str, kind: Kind, subject: &str) -> Result<(), String> {
    match host::store_del(&live_key(wallet, kind, subject)) {
        Ok(()) | Err(SdkError::Host(HostStatus::NotFound)) => Ok(()),
        Err(e) => Err(format!("live index: {}", sanitize_host_error(&e.message()))),
    }
}

pub fn list_ids(wallet: &str) -> Result<Vec<String>, String> {
    let prefix = format!("{STORE_PREFIX}{wallet}/");
    let mut ids: Vec<String> = host::store_list(&prefix, MAX_LIST_BYTES)
        .map_err(|e| format!("operation list: {}", sanitize_host_error(&e.message())))?
        .into_iter()
        .filter_map(|key| key.strip_prefix(&prefix).map(str::to_owned))
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub fn list_wallets() -> Result<Vec<String>, String> {
    let mut wallets: Vec<String> = host::store_list(STORE_PREFIX, MAX_LIST_BYTES)
        .map_err(|e| format!("operation list: {}", sanitize_host_error(&e.message())))?
        .into_iter()
        .filter_map(|key| {
            key.strip_prefix(STORE_PREFIX)
                .and_then(|rest| rest.split('/').next())
                .map(str::to_owned)
        })
        .filter(|w| !w.is_empty())
        .collect();
    wallets.sort();
    wallets.dedup();
    Ok(wallets)
}

/// The most recent operations of a wallet (by `updated_ms`), optionally of
/// one kind. Every record is loaded before sorting, up to
/// `OPS_SCAN_MAX_OPS` ids; `Recent::truncated` says whether that bound hit.
pub struct Recent {
    pub ops: Vec<Operation>,
    pub scanned: usize,
    pub truncated: bool,
}

pub fn recent(wallet: &str, kind: Option<Kind>, max: usize) -> Result<Recent, String> {
    let ids = list_ids(wallet)?;
    let truncated = ids.len() > OPS_SCAN_MAX_OPS;
    let scanned = ids.len().min(OPS_SCAN_MAX_OPS);
    let mut ops: Vec<Operation> = ids
        .iter()
        .take(OPS_SCAN_MAX_OPS)
        .filter_map(|id| load(wallet, id).ok().flatten())
        .filter(|op| kind.is_none_or(|k| op.kind == k))
        .collect();
    ops.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    ops.truncate(max);
    Ok(Recent {
        ops,
        scanned,
        truncated,
    })
}

// ---- host truth -> record ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxState {
    Pending,
    Broadcast,
    Success,
    Reverted,
    Dropped,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspected {
    pub state: TxState,
    pub raw_state: String,
    pub tx_hash: Option<String>,
    pub block_number: Option<u64>,
    pub revert_reason: Option<String>,
}

/// Critique B3: `state` is the receipt outcome (`success`|`reverted`) when a
/// receipt exists, else the staged status (`pending`|`sent`|`success`|
/// `reverted`|`failed`|`cancelled`). pp's `confirmed|mined` are accepted
/// defensively.
pub fn classify(inspection: &OutboxInspection) -> Inspected {
    let receipt: Option<Value> = inspection
        .receipt_json
        .as_deref()
        .and_then(|r| serde_json::from_str(r).ok());
    let outcome = receipt
        .as_ref()
        .and_then(|r| r["outcome"].as_str().map(str::to_owned));
    let raw_state = outcome.clone().unwrap_or_else(|| inspection.state.clone());
    let state = match raw_state.as_str() {
        "pending" => TxState::Pending,
        "sent" => TxState::Broadcast,
        "success" | "confirmed" | "mined" => TxState::Success,
        "reverted" => TxState::Reverted,
        "failed" => TxState::Dropped,
        "cancelled" => TxState::Cancelled,
        _ => TxState::Unknown,
    };
    Inspected {
        state,
        raw_state,
        tx_hash: inspection.tx_hash.clone().or_else(|| {
            receipt
                .as_ref()
                .and_then(|r| r["tx_hash"].as_str().map(str::to_owned))
        }),
        block_number: receipt.as_ref().and_then(|r| r["block_number"].as_u64()),
        revert_reason: receipt
            .as_ref()
            .and_then(|r| r["revert_reason"].as_str().map(sanitize_host_error)),
    }
}

/// `tx_inspect` errors: `Denied` and `NotFound` (entry gone) map to a
/// non-regressing `unknown`, never to `failed`. The host binds inspection to
/// the execution origin that staged the entry (petal id, package hash AND
/// route id), and every inspection here runs from the staging route's read,
/// so a `Denied` is an anomaly (a package re-hash, an entry staged by another
/// route or build), which the note says.
pub fn classify_error(error: &SdkError) -> Inspected {
    Inspected {
        state: TxState::Unknown,
        raw_state: "unknown".into(),
        tx_hash: None,
        block_number: None,
        revert_reason: Some(format!(
            "outbox inspection: {} (entry not staged by this route?)",
            sanitize_host_error(&error.message())
        )),
    }
}

/// Apply one inspection to `txs[index]` and derive the operation status.
/// Never regresses a terminal status; never advances to `completed` (that
/// needs domain evidence, see `try_complete`).
pub fn apply(op: &mut Operation, index: usize, inspected: &Inspected, now_ms: u64) -> bool {
    if index >= op.txs.len() {
        return false;
    }
    let before = op.clone();
    let tx = &mut op.txs[index];
    // A recorded receipt outcome is final: losing sight of the entry later
    // (pruned, denied) must not regress what the host already told us.
    if inspected.state == TxState::Unknown && tx.outcome.is_some() {
        op.note = inspected.revert_reason.clone();
        let changed = *op != before;
        if changed {
            op.updated_ms = now_ms;
        }
        return changed;
    }
    if inspected.state != TxState::Unknown {
        tx.outbox_state = inspected.raw_state.clone();
        if inspected.tx_hash.is_some() {
            tx.tx_hash = inspected.tx_hash.clone();
        }
        if inspected.block_number.is_some() {
            tx.block_number = inspected.block_number;
        }
        tx.outcome = match inspected.state {
            TxState::Success => Some("success".into()),
            TxState::Reverted => Some("reverted".into()),
            TxState::Dropped => Some("dropped".into()),
            TxState::Cancelled => Some("cancelled".into()),
            _ => None,
        };
        if inspected.revert_reason.is_some() {
            tx.revert_reason = inspected.revert_reason.clone();
        }
    } else {
        tx.outbox_state = "unknown".into();
    }
    let superseded = tx.superseded;
    let role = tx.role;
    let confirm = tx.confirm_path.clone();
    if !superseded && !op.is_terminal() {
        op.step = Some(role);
        match inspected.state {
            TxState::Pending => {
                op.status = Status::Staged;
                op.error = None;
                op.set_confirm_path(confirm);
            }
            TxState::Broadcast => {
                op.status = Status::Broadcast;
                op.error = None;
            }
            TxState::Success => {
                op.status = Status::Confirmed;
                op.error = None;
            }
            TxState::Reverted => {
                op.status = Status::Failed;
                op.error = Some(OpError {
                    code: "reverted".into(),
                    message: inspected
                        .revert_reason
                        .clone()
                        .unwrap_or_else(|| "transaction reverted on-chain".into()),
                    retryable: true,
                });
            }
            TxState::Dropped => {
                op.status = Status::Failed;
                op.error = Some(OpError {
                    code: "expired-or-dropped".into(),
                    message:
                        "the outbox entry expired before it was confirmed, or the host dropped it"
                            .into(),
                    retryable: true,
                });
            }
            TxState::Cancelled => {
                op.status = Status::Failed;
                op.error = Some(OpError {
                    code: "cancelled".into(),
                    message: "the owner cancelled the outbox entry".into(),
                    retryable: true,
                });
            }
            TxState::Unknown => {
                op.status = Status::Unknown;
                op.note = inspected.revert_reason.clone();
            }
        }
        op.finalize_next_action();
    }
    let changed = *op != before;
    if changed {
        op.updated_ms = now_ms;
    }
    changed
}

/// Domain completion for a `confirmed` swap/create step (critique B1/D11).
/// Buy/sell: `balanceOf(wallet)` of the output token now minus the balance
/// frozen at stage time. Launch: the creator's launch index row whose
/// `created_block` equals the receipt block (fallback: newest row with the
/// launched symbol and name). Leaves the status untouched when evidence is
/// not available yet.
pub fn try_complete(op: &mut Operation, network: Network, now_ms: u64) -> Result<bool, String> {
    if op.status != Status::Confirmed {
        return Ok(false);
    }
    let Some(index) = op.latest_live() else {
        return Ok(false);
    };
    let tx = op.txs[index].clone();
    if tx.role == Step::Approve {
        return Ok(false);
    }
    let wallet_address = op.wallet_address()?;
    let before = op.clone();
    match op.kind {
        Kind::Buy | Kind::Sell => {
            let token_out = if op.kind == Kind::Buy {
                op.plan
                    .token
                    .as_deref()
                    .and_then(|t| t.parse::<Address>().ok())
                    .ok_or("plan has no token")?
            } else {
                USDC
            };
            let decimals = if op.kind == Kind::Buy {
                op.plan.decimals.unwrap_or(18)
            } else {
                USDC_ERC20_DECIMALS
            };
            let balance_before = tx
                .balance_before_raw
                .as_deref()
                .map(parse_u256_decimal)
                .transpose()?
                .unwrap_or(U256::ZERO);
            // On Arc the ERC-20 USDC view IS the gas balance: a sell's delta is
            // net of the gas the sell itself paid (and the receipt carries no
            // gas_used to correct it), so it is labelled as such and never
            // triggers the "did not increase" suspicion.
            let net_of_gas = op.kind == Kind::Sell;
            match chain::erc20_balance_of(token_out, wallet_address) {
                Ok(after) => {
                    let delta = after.saturating_sub(balance_before);
                    let evidence = json!({
                        "method": if net_of_gas { "balance_delta_net_of_gas" } else { "balance_delta" },
                        "net_of_gas": net_of_gas,
                        "token_out": addr_hex(token_out),
                        "amount_out_raw": delta.to_string(),
                        "amount_out_human": format_units(delta, decimals),
                        "balance_before_raw": balance_before.to_string(),
                        "balance_after_raw": after.to_string(),
                        "tx_hash": tx.tx_hash,
                        "block_number": tx.block_number,
                    });
                    if delta == U256::ZERO && !net_of_gas {
                        // Durable state never overstates completion: a mined
                        // buy without a visible output stays `confirmed`.
                        op.note = Some("the swap mined but the output token balance did not increase; verify the wallet did not move the token meanwhile, then read again".into());
                    } else {
                        op.result = Some(evidence);
                        op.status = Status::Completed;
                        op.note = if delta == U256::ZERO {
                            Some("the sell mined; the USDC delta net of the gas paid from the same balance is zero".into())
                        } else {
                            None
                        };
                    }
                }
                Err(e) => {
                    op.note = Some(format!("completion evidence unavailable: {e}"));
                }
            }
        }
        Kind::Launch => {
            // Missing evidence never fails the read: while the TOLLY API is
            // down the record keeps its durable `confirmed` state and a note.
            let list = match fetch_json(network, &ApiRoute::LaunchesByCreator(wallet_address)) {
                Ok(list) => list,
                Err(e) => {
                    op.note = Some(format!(
                        "completion evidence unavailable: TOLLY API: {}",
                        e.message()
                    ));
                    op.finalize_next_action();
                    let changed = *op != before;
                    if changed {
                        op.updated_ms = now_ms;
                    }
                    return Ok(changed);
                }
            };
            let rows = launch_rows(&list);
            let by_block = tx
                .block_number
                .and_then(|block| rows.iter().find(|r| r.created_block == Some(block)));
            let by_identity = || {
                rows.iter()
                    .filter(|r| {
                        r.symbol.as_deref() == op.plan.launch_symbol.as_deref()
                            && r.name.as_deref() == op.plan.name.as_deref()
                    })
                    .max_by_key(|r| r.created_block.unwrap_or(0))
            };
            match by_block.or_else(by_identity) {
                Some(row) => {
                    op.result = Some(json!({
                        "method": if by_block.is_some() { "creator-index-block" } else { "creator-index-identity" },
                        "token": addr_hex(row.address),
                        "pool": row.pool.map(addr_hex),
                        "symbol": row.symbol,
                        "name": row.name,
                        "created_block": row.created_block,
                        "tx_hash": tx.tx_hash,
                        "block_number": tx.block_number,
                        "detail": format!("tokens/{}.json", addr_hex(row.address)),
                    }));
                    op.status = Status::Completed;
                    op.note = None;
                }
                None => {
                    op.note = Some("the launch mined but the TOLLY index has not listed it yet; read again shortly".into());
                }
            }
        }
    }
    op.finalize_next_action();
    let changed = *op != before;
    if changed {
        op.updated_ms = now_ms;
    }
    Ok(changed)
}

/// Reconcile the latest live attempt against the host and try domain
/// completion. Returns whether the record changed (the caller persists).
pub fn reconcile(op: &mut Operation, network: Network, now_ms: u64) -> Result<bool, String> {
    let mut changed = false;
    if matches!(
        op.status,
        Status::Staged | Status::Broadcast | Status::Confirmed | Status::Unknown
    ) && let Some(index) = op.latest_live()
        // A success receipt is final; do not ask the host again.
        && op.txs[index].outcome.as_deref() != Some("success")
    {
        let outbox_id = op.txs[index].outbox_id.clone();
        let inspected = match host::tx_inspect(&op.wallet, CHAIN, &outbox_id) {
            Ok(inspection) => classify(&inspection),
            Err(e) => classify_error(&e),
        };
        changed |= apply(op, index, &inspected, now_ms);
    }
    if op.status == Status::Confirmed {
        changed |= try_complete(op, network, now_ms)?;
    }
    Ok(changed)
}

/// Read one operation: the stored record, unchanged, plus a `refresh` hint.
///
/// A pure store projection: no `tx_inspect` (Bloom binds outbox inspection
/// to the route that staged the entry and would answer `Denied` here), no
/// chain or HTTP read, no save. Served under the 5 s account cache; the
/// staging route's read (`route_read_side`) is what advances the record.
pub fn read_operation(wallet: &str, id: &str) -> DispatchResponse {
    if let Err(response) = check_wallet_id(wallet) {
        return response;
    }
    if let Err(e) = validate_id(id) {
        return petal::error(-3, e);
    }
    let op = match load(wallet, id) {
        Ok(Some(op)) => op,
        Ok(None) => return petal::error(-1, "no such operation"),
        Err(e) => return petal::error(-4, e),
    };
    let mut doc = match serde_json::to_value(&op) {
        Ok(Value::Object(map)) => map,
        Ok(_) | Err(_) => return petal::error(-4, "operation record serialize"),
    };
    doc.insert("refresh".into(), Value::String(refresh_hint(&op)));
    petal::read_json_value(&Value::Object(doc))
}

/// Where an agent goes to make this record current.
fn refresh_hint(op: &Operation) -> String {
    format!(
        "this file is a cached projection of the stored record (up to ~5 s stale) and never inspects the outbox: outbox inspection is bound to the staging route, so read wallets/{}/{}.json to reconcile this record (its `reconciled` lists what changed), then re-read this file",
        op.wallet,
        op.kind.name()
    )
}

/// Why another operation blocks a new stage for the same subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveConflict {
    /// That operation's latest entry is still `pending` in the outbox.
    Pending { id: String },
    /// That operation staged an entry it could not record (`stage_in_flight`).
    Unrecorded { id: String },
}

impl LiveConflict {
    pub fn message(&self, what: &str) -> String {
        match self {
            Self::Pending { id } => format!(
                "another operation ({id}) for {what} still has a pending outbox entry; confirm or cancel it in Bloom first"
            ),
            Self::Unrecorded { id } => format!(
                "another operation ({id}) for {what} staged an outbox entry it could not record; inspect that operation and acknowledge it before staging again"
            ),
        }
    }
}

/// Critique M1: refuse a new stage while another operation of this wallet
/// for the same subject still has a live outbox entry. The `(wallet, kind,
/// subject)` index names the last operation that staged for it; the host's
/// `tx_inspect` decides whether its entry is still pending, whatever status
/// the record was last persisted with (`staged`, `unknown`, ...). A stale
/// index entry is cleared on the way.
pub fn live_conflict(
    wallet: &str,
    kind: Kind,
    subject: &str,
    exclude_id: &str,
) -> Result<Option<LiveConflict>, String> {
    let Some(id) = live_index_get(wallet, kind, subject)? else {
        return Ok(None);
    };
    if id == exclude_id {
        return Ok(None);
    }
    let Some(op) = load(wallet, &id)? else {
        live_index_clear(wallet, kind, subject)?;
        return Ok(None);
    };
    if op.stage_in_flight.is_some() {
        return Ok(Some(LiveConflict::Unrecorded { id }));
    }
    if let Some(index) = op.latest_live()
        && op.txs[index].outcome.is_none()
        && let Ok(inspection) = host::tx_inspect(wallet, CHAIN, &op.txs[index].outbox_id)
        && classify(&inspection).state == TxState::Pending
    {
        return Ok(Some(LiveConflict::Pending { id }));
    }
    live_index_clear(wallet, kind, subject)?;
    Ok(None)
}

/// Whether a read of the staging route should ask the host about this
/// operation: an outbox entry may be live, a mined step awaits completion
/// evidence, or a stage was never recorded.
pub fn needs_reconcile(op: &Operation) -> bool {
    op.stage_in_flight.is_some()
        || matches!(
            op.status,
            Status::Staged | Status::Broadcast | Status::Confirmed | Status::Unknown
        )
}

/// The read side of a writable route, after reconciliation.
pub struct RouteReadSide {
    /// One entry per operation this read reconciled (`id`, `status`, `step`,
    /// `next_action`, `changed`, `error_code`, `reconcile_error`, `file`).
    pub reconciled: Vec<Value>,
    /// More in-flight operations exist than this read reconciled.
    pub truncated: bool,
    /// The `recent` projection, from the post-reconcile records.
    pub recent: Value,
}

/// Reconcile this route's in-flight operations of `kind` against Bloom's
/// outbox and domain evidence, persist every advance, then project the
/// newest `recent_max` operations from what was persisted.
///
/// This runs from the READ of `buy.json` / `sell.json` / `launch.json` and
/// nowhere else: the host compares the outbox entry's execution origin
/// (petal id, package hash, route id) with the caller's and answers `Denied`
/// from any other route, so `operations/<id>.json` cannot inspect. Bounded to
/// `RECONCILE_MAX_OPS` candidates, newest first by `updated_ms`, out of the
/// `recent` scan (itself bounded to `OPS_SCAN_MAX_OPS` records).
pub fn route_read_side(wallet: &str, kind: Kind, recent_max: usize) -> RouteReadSide {
    let mut recent = match recent(wallet, Some(kind), OPS_SCAN_MAX_OPS) {
        Ok(recent) => recent,
        Err(e) => {
            return RouteReadSide {
                reconciled: Vec::new(),
                truncated: false,
                recent: json!({ "error": e }),
            };
        }
    };
    let network = Network::current();
    let now = host::now_ms();
    let candidates: Vec<usize> = recent
        .ops
        .iter()
        .enumerate()
        .filter(|(_, op)| needs_reconcile(op))
        .map(|(index, _)| index)
        .collect();
    let truncated = recent.truncated || candidates.len() > RECONCILE_MAX_OPS;
    let mut reconciled = Vec::with_capacity(candidates.len().min(RECONCILE_MAX_OPS));
    for index in candidates.into_iter().take(RECONCILE_MAX_OPS) {
        let op = &mut recent.ops[index];
        let before = op.clone();
        // A change counts only once it is persisted: on any failure the
        // in-memory record goes back to what the store holds.
        let (changed, error) = match reconcile(op, network, now) {
            Ok(true) => match save(op) {
                Ok(()) => (true, None),
                Err(e) => {
                    *op = before;
                    (false, Some(e))
                }
            },
            Ok(false) => (false, None),
            Err(e) => {
                *op = before;
                (false, Some(e))
            }
        };
        reconciled.push(json!({
            "id": op.id,
            "status": op.status,
            "step": op.step,
            "next_action": op.next_action,
            "changed": changed,
            "error_code": op.error.as_ref().map(|e| e.code.clone()),
            "reconcile_error": error,
            "file": format!("operations/{}.json", op.id),
        }));
    }
    recent.ops.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    recent.ops.truncate(recent_max);
    RouteReadSide {
        reconciled,
        truncated,
        recent: recent_projection(recent),
    }
}

/// Compact projection of recent operations for the writable routes' read side.
fn recent_projection(recent: Recent) -> Value {
    json!({
        "operations": recent
            .ops
            .into_iter()
            .map(|op| {
                json!({
                    "id": op.id,
                    "status": op.status,
                    "step": op.step,
                    "next_action": op.next_action,
                    "error_code": op.error.as_ref().map(|e| e.code.clone()),
                    "updated_ms": op.updated_ms,
                    "last_write_ms": op.last_write_ms,
                    "file": format!("operations/{}.json", op.id),
                })
            })
            .collect::<Vec<_>>(),
        "scanned": recent.scanned,
        "scan_truncated": recent.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::confirm_path;

    fn inspection(state: &str, tx_hash: Option<&str>, receipt: Option<Value>) -> OutboxInspection {
        OutboxInspection {
            outbox_id: "ob-1".into(),
            state: state.into(),
            tx_hash: tx_hash.map(str::to_owned),
            receipt_json: receipt.map(|r| r.to_string()),
        }
    }

    fn op_with_tx(role: Step) -> Operation {
        let mut op = Operation::new(
            "buy-1",
            "main",
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()
                .unwrap(),
            Kind::Buy,
            Network::Prod.name(),
            "digest".into(),
            json!({}),
            1_000,
        );
        op.txs.push(TxEntry {
            role,
            to: "0x00".into(),
            outbox_id: "ob-1".into(),
            confirm_path: confirm_path("main", "ob-1"),
            confirm_path_note: crate::tx::mount_note(),
            staged_ms: 1_000,
            outbox_state: "pending".into(),
            tx_hash: None,
            outcome: None,
            block_number: None,
            revert_reason: None,
            superseded: false,
            attempt_params: json!({}),
            spender: None,
            amount_raw: None,
            balance_before_raw: Some("5".into()),
            plan_md: String::new(),
        });
        op.status = Status::Staged;
        op.step = Some(role);
        op.finalize_next_action();
        op.set_confirm_path(confirm_path("main", "ob-1"));
        op
    }

    #[test]
    fn id_grammar() {
        assert!(validate_id("buy-moss-001").is_ok());
        assert!(validate_id("a").is_ok());
        assert!(validate_id("0abc.def_ghi").is_ok());
        for bad in [
            "",
            "Buy",
            "-x",
            "a/b",
            "a b",
            "..",
            ".",
            &"a".repeat(65),
            "a:b",
        ] {
            assert!(validate_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn digest_is_canonical_and_kind_bound() {
        let a = request_digest(
            Kind::Buy,
            &json!({"token": "0xab", "amount_usdc_raw": "25000000"}),
        );
        let b = request_digest(
            Kind::Buy,
            &json!({"amount_usdc_raw": "25000000", "token": "0xab"}),
        );
        assert_eq!(a, b, "key order does not matter (JCS)");
        assert_ne!(
            a,
            request_digest(
                Kind::Sell,
                &json!({"token": "0xab", "amount_usdc_raw": "25000000"})
            )
        );
        assert_ne!(
            a,
            request_digest(
                Kind::Buy,
                &json!({"token": "0xab", "amount_usdc_raw": "25000001"})
            )
        );
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn classify_maps_every_host_state_per_b3() {
        assert_eq!(
            classify(&inspection("pending", None, None)).state,
            TxState::Pending
        );
        assert_eq!(
            classify(&inspection("sent", Some("0xab"), None)).state,
            TxState::Broadcast
        );
        assert_eq!(
            classify(&inspection("failed", None, None)).state,
            TxState::Dropped
        );
        assert_eq!(
            classify(&inspection("cancelled", None, None)).state,
            TxState::Cancelled
        );
        assert_eq!(
            classify(&inspection("success", Some("0xab"), None)).state,
            TxState::Success
        );
        assert_eq!(
            classify(&inspection("confirmed", Some("0xab"), None)).state,
            TxState::Success
        );
        assert_eq!(
            classify(&inspection("weird", None, None)).state,
            TxState::Unknown
        );
        // With a receipt the state IS the outcome.
        let ok = classify(&inspection(
            "success",
            Some("0xab"),
            Some(
                json!({"outcome": "success", "tx_hash": "0xab", "block_number": 20185400, "revert_reason": null}),
            ),
        ));
        assert_eq!(ok.state, TxState::Success);
        assert_eq!(ok.block_number, Some(20185400));
        let rev = classify(&inspection(
            "reverted",
            Some("0xab"),
            Some(
                json!({"outcome": "reverted", "tx_hash": "0xab", "block_number": 7, "revert_reason": "Too little received https://rpc.example/k"}),
            ),
        ));
        assert_eq!(rev.state, TxState::Reverted);
        assert_eq!(
            rev.revert_reason.as_deref(),
            Some("Too little received <url>")
        );
        let denied = classify_error(&SdkError::Host(HostStatus::Denied));
        assert_eq!(denied.state, TxState::Unknown);
        assert!(
            denied
                .revert_reason
                .as_deref()
                .unwrap()
                .contains("(entry not staged by this route?)")
        );
    }

    #[test]
    fn apply_transitions_and_next_actions() {
        let mut op = op_with_tx(Step::Approve);
        assert!(apply(
            &mut op,
            0,
            &classify(&inspection("sent", Some("0x1"), None)),
            2
        ));
        assert_eq!(op.status, Status::Broadcast);
        assert_eq!(op.next_action, NextAction::Wait);
        assert_eq!(op.confirm_path, None);
        assert_eq!(op.confirm_path_note, None, "the notes leave with the path");
        assert_eq!(op.cancel_hint, None);
        apply(
            &mut op,
            0,
            &classify(&inspection(
                "success",
                Some("0x1"),
                Some(json!({"outcome":"success","tx_hash":"0x1","block_number":9})),
            )),
            3,
        );
        assert_eq!(op.status, Status::Confirmed);
        assert_eq!(
            op.next_action,
            NextAction::Repost,
            "approve confirmed -> POST again for the swap"
        );
        assert_eq!(op.txs[0].block_number, Some(9));

        let mut op = op_with_tx(Step::Swap);
        apply(
            &mut op,
            0,
            &classify(&inspection(
                "reverted",
                Some("0x2"),
                Some(json!({"outcome":"reverted","tx_hash":"0x2","revert_reason":"STF"})),
            )),
            3,
        );
        assert_eq!(op.status, Status::Failed);
        assert_eq!(op.error.as_ref().unwrap().code, "reverted");
        assert!(op.error.as_ref().unwrap().retryable);
        assert_eq!(op.next_action, NextAction::Retry);

        let mut op = op_with_tx(Step::Swap);
        apply(&mut op, 0, &classify(&inspection("failed", None, None)), 3);
        assert_eq!(op.error.as_ref().unwrap().code, "expired-or-dropped");
        let mut op = op_with_tx(Step::Swap);
        apply(
            &mut op,
            0,
            &classify(&inspection("cancelled", None, None)),
            3,
        );
        assert_eq!(op.error.as_ref().unwrap().code, "cancelled");

        let mut op = op_with_tx(Step::Swap);
        apply(
            &mut op,
            0,
            &classify_error(&SdkError::Host(HostStatus::NotFound)),
            3,
        );
        assert_eq!(op.status, Status::Unknown);
        assert_eq!(op.next_action, NextAction::Inspect);
        assert!(op.error.is_none(), "unknown is not failed");
    }

    #[test]
    fn apply_never_regresses_terminal_states() {
        let mut op = op_with_tx(Step::Swap);
        op.status = Status::Completed;
        op.result = Some(json!({"amount_out_raw": "1"}));
        op.finalize_next_action();
        let changed = apply(
            &mut op,
            0,
            &classify(&inspection("sent", Some("0x9"), None)),
            9,
        );
        assert_eq!(op.status, Status::Completed);
        assert_eq!(op.next_action, NextAction::None);
        assert!(changed, "tx fields still refresh");
        assert_eq!(op.txs[0].tx_hash.as_deref(), Some("0x9"));
        let mut op = op_with_tx(Step::Swap);
        op.set_failed(Some(Step::Swap), "policy-denied", "no", false, 5);
        apply(
            &mut op,
            0,
            &classify(&inspection("success", Some("0x1"), None)),
            9,
        );
        assert_eq!(op.status, Status::Failed);
        assert_eq!(op.next_action, NextAction::None);
        // Superseded attempts never drive the status.
        let mut op = op_with_tx(Step::Swap);
        op.txs[0].superseded = true;
        op.status = Status::Created;
        op.finalize_next_action();
        apply(
            &mut op,
            0,
            &classify(&inspection("success", Some("0x1"), None)),
            9,
        );
        assert_eq!(op.status, Status::Created);
    }

    #[test]
    fn completed_requires_domain_evidence_not_just_a_receipt() {
        let mut op = op_with_tx(Step::Swap);
        apply(
            &mut op,
            0,
            &classify(&inspection(
                "success",
                Some("0x1"),
                Some(json!({"outcome":"success","tx_hash":"0x1","block_number":1})),
            )),
            3,
        );
        assert_eq!(
            op.status,
            Status::Confirmed,
            "a receipt alone is never completion"
        );
        assert_eq!(op.next_action, NextAction::Wait);
    }

    #[test]
    fn subject_follows_kind() {
        let mut op = op_with_tx(Step::Swap);
        op.request = json!({"token": "0xABCD"});
        assert_eq!(op.subject(), "0xabcd");
        op.plan.token = Some("0x1234".into());
        assert_eq!(op.subject(), "0x1234");
        op.kind = Kind::Launch;
        op.request = json!({"symbol": " moss "});
        assert_eq!(op.subject(), "MOSS");
    }
}
