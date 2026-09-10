//! Bloom wallet ids and the wallet's EVM address.
//!
//! `[wallet]` is a Bloom wallet id under `/bloom/wallets/<id>`, never a `0x`
//! address. `petal::wallet_param` validates the id; this module additionally
//! requires a single safe store segment (critique M5: the SDK grammar allows
//! `/`, which would corrupt `tolly/ops/{wallet}/{id}` keys and listings).

use alloy_primitives::Address;
use petal::DispatchResponse;

use crate::host;
use crate::sanitize_host_error;

/// Extra constraints on top of `petal::wallet_param`.
pub fn check_wallet_id(wallet: &str) -> Result<(), DispatchResponse> {
    if petal::validate_wallet_id(wallet).is_err() {
        return Err(petal::error(
            -3,
            "wallet must be a Bloom wallet id (see /bloom/wallets)",
        ));
    }
    if !petal::is_safe_segment(wallet) || wallet.contains('/') || wallet.len() > 64 {
        return Err(petal::error(
            -3,
            "wallet id must be a single path segment of at most 64 bytes without '/'",
        ));
    }
    Ok(())
}

/// The wallet's checksummed owner/signer address, read from the host VFS
/// (`wallets/<wallet>/address`, relative to the mounted `/bloom`).
pub fn wallet_address(wallet: &str) -> Result<Address, String> {
    let bytes = host::vfs_read(&format!("wallets/{wallet}/address"), 128)
        .map_err(|e| format!("wallet address: {}", sanitize_host_error(&e.message())))?;
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| "wallet address is not UTF-8")?
        .trim();
    value
        .parse::<Address>()
        .map_err(|_| "wallet address is not a 20-byte EVM address".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallet_ids_are_single_segments() {
        assert!(check_wallet_id("main").is_ok());
        assert!(check_wallet_id("agent-1.test_2").is_ok());
        assert!(
            check_wallet_id("team/alice").is_err(),
            "slash would split the store key"
        );
        assert!(check_wallet_id("0x0000000000000000000000000000000000000001").is_err());
        assert!(check_wallet_id("Main").is_err());
        assert!(check_wallet_id("").is_err());
        assert!(check_wallet_id(&"a".repeat(65)).is_err());
    }
}
