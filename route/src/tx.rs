//! Staging helpers over `bloom:tx/outbox`.
//!
//! Every staged transaction is `value_wei: "0"` (all day-1 paths spend the
//! ERC-20 USDC view), `nonce: None`, and no fee overrides (critique M9: the
//! TxEngine sets EIP-1559 fees and estimates gas itself). The `eth_call{from}`
//! pre-flight is the mandatory revert guard before any stage: the engine falls
//! back to a 500k gas limit when its own estimate fails, so a bad calldata
//! would otherwise burn gas reverting on-chain.
//!
//! `tx_confirm` is deliberately never called (D12). Outbox confirms are
//! passkey-per-transaction by construction on Bloom v0.2.1: every confirm
//! mints a single-use Exact approval bound to {bloom-machine,
//! transaction.confirm} and requires the owner's passkey (bloom-tx
//! `tx_engine.rs` `triad_sign_evm_payload`, ~2864-2875 and ~3981-4006); the
//! local `agent_autonomy` branch is non-gating (~3527-3547) and
//! `bloom_proto::Policy` has no config loader, so no daemon setting turns a
//! confirm into an unprompted broadcast. A `tx_confirm` from this Petal would
//! therefore gain nothing, and with `acknowledge_warnings = true` it would
//! bypass simulation. The owner confirms by writing to `confirm_path`
//! (`wallets/<wallet>/chains/arc/outbox/pending/<outbox_id>/confirm`, relative
//! to the Bloom mount root, see `MOUNT_NOTE`).

use alloy_primitives::Address;
use petal::{EvmTransaction, HostStatus, SdkError, StagedTransaction};

use crate::abi::hex0x;
use crate::amount::addr_hex;
use crate::chain;
use crate::constants::CHAIN;
use crate::host;
use crate::policy::MAX_PLAN_MD_BYTES;
use crate::sanitize_host_error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StageError {
    /// The host refused the stage (wallet policy, MEV guard, valuation).
    Denied(String),
    Backend(String),
}

impl StageError {
    pub fn message(&self) -> &str {
        match self {
            Self::Denied(m) | Self::Backend(m) => m,
        }
    }
}

/// Simulate the exact calldata from the wallet before staging it.
pub fn preflight(from: Address, to: Address, data: &[u8], label: &str) -> Result<(), String> {
    chain::eth_call_from(from, to, data, label).map(|_| ())
}

/// Stage one zero-value transaction for the owner to confirm in Bloom.
pub fn stage(wallet: &str, to: Address, data: &[u8]) -> Result<StagedTransaction, StageError> {
    let request = EvmTransaction {
        wallet: wallet.to_owned(),
        chain: CHAIN.to_owned(),
        to: addr_hex(to),
        value_wei: "0".into(),
        data_hex: hex0x(data),
        nonce: None,
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
    };
    // The daemon renders the engine's error as `backend: stage EVM outbox:
    // <TxEngineError>`; the SDK's `host_err` turns anything containing
    // "denied" (`policy denied`, `approval denied: ...`, capability denials)
    // into `HostStatus::Denied` before it reaches us, so only the engine's
    // exact `valuation unavailable: ...` wording is matched here. Everything
    // else (RPC, transport, simulation) is a retryable backend failure.
    match host::tx_stage(&request) {
        Ok(staged) => Ok(staged),
        Err(SdkError::Host(HostStatus::Denied)) => {
            Err(StageError::Denied("denied by the host".into()))
        }
        Err(e) => {
            let message = sanitize_host_error(&e.message());
            if message
                .to_ascii_lowercase()
                .contains("valuation unavailable")
            {
                Err(StageError::Denied(message))
            } else {
                Err(StageError::Backend(message))
            }
        }
    }
}

/// The confirm file of a staged entry, RELATIVE to the Bloom mount root:
/// `wallets/<wallet>/chains/arc/outbox/pending/<outbox_id>/confirm`.
///
/// The mount point is wherever the owner's fstab puts it (`~/bloom` on a
/// default Linux install); it is not `/bloom`: `mount_path = "/bloom"` in
/// `~/.bloom/config.toml` is informational only (bloom-proto `config.rs`),
/// and a literal `/bloom/...` gets ENOENT. Agents prefix the root themselves;
/// `MOUNT_NOTE` travels with every emitted path (`confirm_path_note`).
pub fn confirm_path(wallet: &str, outbox_id: &str) -> String {
    format!("wallets/{wallet}/chains/{CHAIN}/outbox/pending/{outbox_id}/confirm")
}

/// Emitted next to every `confirm_path` as `confirm_path_note`.
pub const MOUNT_NOTE: &str = "paths are relative to the Bloom mount root (the owner's mount point, `~/bloom` on a default Linux install; `mount_path` in config.toml is not the mount point)";

/// Emitted as `cancel_hint` while an entry is pending: the mount refuses the
/// separate `cancel` file (EPERM) and the word `cancel` written into the
/// confirm file cancels (Bloom v0.2.1, see README "Host facts").
pub const CANCEL_HINT: &str = "to cancel, write the word `cancel` into the same confirm file (the separate cancel file is refused on the mount)";

/// serde default for `TxEntry::confirm_path_note` (records written before
/// the note existed).
pub fn mount_note() -> String {
    MOUNT_NOTE.to_owned()
}

/// `plan_md` is the engine's rendered plan (no key material); keep a bounded
/// copy for audit.
pub fn truncate_plan_md(plan_md: &str) -> String {
    if plan_md.len() <= MAX_PLAN_MD_BYTES {
        return plan_md.to_owned();
    }
    let mut end = MAX_PLAN_MD_BYTES;
    while !plan_md.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &plan_md[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_path_names_the_outbox_entry_relative_to_the_mount_root() {
        let path = confirm_path("main", "ob-7");
        assert_eq!(path, "wallets/main/chains/arc/outbox/pending/ob-7/confirm");
        assert!(
            !path.starts_with('/'),
            "never absolute: the mount point is the owner's"
        );
        assert!(
            !path.contains("bloom"),
            "`/bloom` is config.toml's mount_path, not the mount"
        );
        assert_eq!(mount_note(), MOUNT_NOTE);
        assert!(MOUNT_NOTE.contains("relative to the Bloom mount root"));
        assert!(MOUNT_NOTE.contains("`~/bloom`"));
        assert!(CANCEL_HINT.contains("write the word `cancel`"));
    }

    #[test]
    fn plan_md_is_bounded() {
        let long = "x".repeat(MAX_PLAN_MD_BYTES + 100);
        let truncated = truncate_plan_md(&long);
        assert!(truncated.ends_with("[truncated]"));
        assert!(truncated.len() <= MAX_PLAN_MD_BYTES + 12);
        assert_eq!(truncate_plan_md("short"), "short");
    }
}
