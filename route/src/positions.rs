//! `wallets/[wallet]/positions.json`: the two views of the wallet's USDC and
//! the balance of every token this wallet's operations touched (critique
//! M11: symbols/decimals come from the frozen records, only `balanceOf` is
//! read, and the token set is bounded).

use alloy_primitives::Address;
use petal::DispatchResponse;
use serde_json::json;

use crate::amount::{addr_hex, format_units, native18_to_usdc6};
use crate::chain;
use crate::constants::{CHAIN, USDC, USDC_ERC20_DECIMALS};
use crate::host;
use crate::ops::{self, Kind, Status};
use crate::policy::{POSITIONS_MAX_OPS, POSITIONS_MAX_TOKENS};
use crate::wallet::{check_wallet_id, wallet_address};

pub fn positions_document(wallet: &str) -> DispatchResponse {
    if let Err(r) = check_wallet_id(wallet) {
        return r;
    }
    let address = match wallet_address(wallet) {
        Ok(a) => a,
        Err(e) => return petal::error(-4, e),
    };
    let native = match chain::eth_get_balance(address) {
        Ok(b) => b,
        Err(e) => return petal::error(-4, e),
    };
    let erc20 = match chain::erc20_balance_of(USDC, address) {
        Ok(b) => b,
        Err(e) => return petal::error(-4, e),
    };

    // Tokens from this wallet's operations: frozen symbol/decimals, no extra reads.
    let mut tokens: Vec<(Address, Option<String>, u32, String)> = Vec::new();
    match ops::recent(wallet, POSITIONS_MAX_OPS) {
        Ok(operations) => {
            for op in operations {
                let (token, symbol, decimals) = match op.kind {
                    Kind::Buy | Kind::Sell => (
                        op.plan.token.clone(),
                        op.plan.symbol.clone(),
                        op.plan.decimals.unwrap_or(18),
                    ),
                    Kind::Launch => {
                        if op.status != Status::Completed {
                            continue;
                        }
                        (
                            op.result
                                .as_ref()
                                .and_then(|r| r["token"].as_str().map(str::to_owned)),
                            op.plan.launch_symbol.clone(),
                            18,
                        )
                    }
                };
                let Some(token) = token.and_then(|t| t.parse::<Address>().ok()) else {
                    continue;
                };
                if tokens.iter().any(|(t, ..)| *t == token) {
                    continue;
                }
                tokens.push((token, symbol, decimals, op.id.clone()));
                if tokens.len() >= POSITIONS_MAX_TOKENS {
                    break;
                }
            }
        }
        Err(e) => return petal::error(-4, e),
    }
    let mut holdings = Vec::with_capacity(tokens.len());
    for (token, symbol, decimals, last_op) in tokens {
        let entry = match chain::erc20_balance_of(token, address) {
            Ok(balance) => json!({
                "token": addr_hex(token),
                "symbol": symbol,
                "decimals": decimals,
                "balance_raw": balance.to_string(),
                "balance_human": format_units(balance, decimals),
                "last_operation": last_op,
                "detail": format!("tokens/{}.json", addr_hex(token)),
            }),
            Err(e) => {
                json!({ "token": addr_hex(token), "symbol": symbol, "decimals": decimals, "error": e, "last_operation": last_op })
            }
        };
        holdings.push(entry);
    }
    petal::read_json_value(&json!({
        "schema": "tolly.positions.v1",
        "wallet": wallet,
        "wallet_address": addr_hex(address),
        "chain": CHAIN,
        "usdc": {
            "native_raw": native.to_string(),
            "native_decimals": 18,
            "erc20_raw": erc20.to_string(),
            "erc20_decimals": USDC_ERC20_DECIMALS,
            "human": format_units(native, 18),
            "note": "one balance, two views: native (gas, 18 decimals) and ERC-20 (6 decimals); erc20_raw == native_raw / 1e12",
            "erc20_matches_native": native18_to_usdc6(native) == erc20,
        },
        "tokens": holdings,
        "bounds": { "max_operations_scanned": POSITIONS_MAX_OPS, "max_tokens": POSITIONS_MAX_TOKENS },
        "checked_ms": host::now_ms(),
    }))
}
