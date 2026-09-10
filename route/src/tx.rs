//! Staging helpers over `bloom:tx/outbox`.
//!
//! Every staged transaction is `value_wei: "0"` (all day-1 paths spend the
//! ERC-20 USDC view), `nonce: None`, and no fee overrides (critique M9: the
//! TxEngine sets EIP-1559 fees and estimates gas itself). The `eth_call{from}`
//! pre-flight is the mandatory revert guard before any stage: the engine falls
//! back to a 500k gas limit when its own estimate fails, so a bad calldata
//! would otherwise burn gas reverting on-chain.
//!
//! `tx_confirm` is deliberately never called (D12): under a wallet policy of
//! `agent_autonomy = under_policy` it would broadcast without a prompt. The
//! owner confirms at `/bloom/wallets/<wallet>/chains/arc/outbox/pending/<outbox_id>/confirm`.

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
    match host::tx_stage(&request) {
        Ok(staged) => Ok(staged),
        Err(SdkError::Host(HostStatus::Denied)) => {
            Err(StageError::Denied("denied by the host".into()))
        }
        Err(e) => {
            let message = sanitize_host_error(&e.message());
            let lower = message.to_ascii_lowercase();
            if lower.contains("denied") || lower.contains("policy") || lower.contains("valuation") {
                Err(StageError::Denied(message))
            } else {
                Err(StageError::Backend(message))
            }
        }
    }
}

/// The confirm path the owner uses for a staged entry.
pub fn confirm_path(wallet: &str, outbox_id: &str) -> String {
    format!("/bloom/wallets/{wallet}/chains/{CHAIN}/outbox/pending/{outbox_id}/confirm")
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
    fn confirm_path_names_the_outbox_entry() {
        assert_eq!(
            confirm_path("main", "ob-7"),
            "/bloom/wallets/main/chains/arc/outbox/pending/ob-7/confirm"
        );
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
