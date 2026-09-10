//! On-chain reads through `bloom:chain` (`petal::sdk::chain_read`).
//!
//! The daemon's Petal allowlist is exactly `eth_chainId`, `eth_getBalance`,
//! `eth_getCode` and `eth_call`, all pinned to the latest block (critique B1).
//! Nothing else is requested here: no receipts, no gas estimation, no gas
//! price, no block number. Every error is sanitized before it leaves.

use alloy_primitives::{Address, U256};

use crate::abi;
use crate::amount::addr_hex;
use crate::constants::{CHAIN, MULTI_ROUTER, PAD, QUOTER_V2, V4_QUOTER};
use crate::host;
use crate::sanitize_host_error;

fn call_json(params: serde_json::Value, method: &str, label: &str) -> Result<String, String> {
    let params = params.to_string();
    let result = host::chain_read(CHAIN, method, &params)
        .map_err(|e| format!("{label}: {}", sanitize_host_error(&e.message())))?;
    serde_json::from_str::<String>(&result)
        .map_err(|_| format!("{label}: RPC result is not a string"))
}

fn decode_result(result: &str, label: &str) -> Result<Vec<u8>, String> {
    let body = result
        .strip_prefix("0x")
        .ok_or(format!("{label}: result is not 0x-prefixed"))?;
    hex::decode(body).map_err(|_| format!("{label}: result is not hex"))
}

/// `eth_call {to, data}` at latest.
pub fn eth_call(to: Address, data: &[u8], label: &str) -> Result<Vec<u8>, String> {
    let params = serde_json::json!([{ "to": addr_hex(to), "data": abi::hex0x(data) }, "latest"]);
    decode_result(&call_json(params, "eth_call", label)?, label)
}

/// `eth_call {from, to, data}` at latest: the mandatory pre-flight before a
/// stage (critique M9). Any error is a refusal.
pub fn eth_call_from(
    from: Address,
    to: Address,
    data: &[u8],
    label: &str,
) -> Result<Vec<u8>, String> {
    let params = serde_json::json!([
        { "from": addr_hex(from), "to": addr_hex(to), "data": abi::hex0x(data) },
        "latest"
    ]);
    decode_result(&call_json(params, "eth_call", label)?, label)
}

/// Native (18-decimal) balance.
pub fn eth_get_balance(address: Address) -> Result<U256, String> {
    let params = serde_json::json!([addr_hex(address), "latest"]);
    let result = call_json(params, "eth_getBalance", "eth_getBalance")?;
    let body = result
        .strip_prefix("0x")
        .ok_or("eth_getBalance: result is not 0x-prefixed")?;
    U256::from_str_radix(body, 16).map_err(|_| "eth_getBalance: result is not hex".to_string())
}

fn single_u256(ret: &[u8], label: &str) -> Result<U256, String> {
    abi::u256_at(ret, 0).ok_or(format!("{label}: short return ({} bytes)", ret.len()))
}

pub fn erc20_balance_of(token: Address, owner: Address) -> Result<U256, String> {
    single_u256(
        &eth_call(token, &abi::erc20_balance_of(owner), "balanceOf")?,
        "balanceOf",
    )
}

pub fn erc20_allowance(token: Address, owner: Address, spender: Address) -> Result<U256, String> {
    single_u256(
        &eth_call(token, &abi::erc20_allowance(owner, spender), "allowance")?,
        "allowance",
    )
}

pub fn erc20_decimals(token: Address) -> Result<u32, String> {
    let value = single_u256(
        &eth_call(token, &abi::erc20_decimals(), "decimals")?,
        "decimals",
    )?;
    u32::try_from(value)
        .ok()
        .filter(|d| *d <= 36)
        .ok_or("decimals: out of range".into())
}

pub fn erc20_symbol(token: Address) -> Result<String, String> {
    let ret = eth_call(token, &abi::erc20_symbol(), "symbol")?;
    abi::decode_string(&ret).ok_or("symbol: not an ABI string".into())
}

/// `PAD.tokens(token).pool`; a non-zero pool means the pad launched it.
pub fn pad_token_pool(token: Address) -> Result<Address, String> {
    let ret = eth_call(PAD, &abi::pad_tokens(token), "PAD.tokens")?;
    abi::decode_pad_token_info(&ret)
        .map(|(_, pool, _)| pool)
        .ok_or("PAD.tokens: short return".into())
}

/// `MULTI_ROUTER.tollFor(tokenIn, tokenOut, amountIn)` — what the contract would charge.
pub fn multi_router_toll_for(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
) -> Result<U256, String> {
    single_u256(
        &eth_call(
            MULTI_ROUTER,
            &abi::multi_router_toll_for(token_in, token_out, amount_in),
            "tollFor",
        )?,
        "tollFor",
    )
}

/// QuoterV2 simulation. `Err` means the pool cannot fill this size (or did not answer).
pub fn quoter_v2_amount_out(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    fee: u32,
) -> Result<U256, String> {
    let ret = eth_call(
        QUOTER_V2,
        &abi::quoter_v2_quote_exact_input_single(token_in, token_out, amount_in, fee),
        "QuoterV2",
    )?;
    single_u256(&ret, "QuoterV2")
}

/// V4Quoter simulation (quoting only day-1).
pub fn v4_quoter_amount_out(
    key: &abi::PoolKey,
    zero_for_one: bool,
    exact_amount: U256,
) -> Result<U256, String> {
    let ret = eth_call(
        V4_QUOTER,
        &abi::v4_quoter_quote_exact_input_single(key, zero_for_one, exact_amount),
        "V4Quoter",
    )?;
    single_u256(&ret, "V4Quoter")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V2PairState {
    pub token0: Address,
    pub reserve0: U256,
    pub reserve1: U256,
}

/// `pair.token0()` + `pair.getReserves()`; reserves are never cached.
pub fn v2_pair_state(pair: Address) -> Result<V2PairState, String> {
    let token0 = abi::address_at(&eth_call(pair, &abi::v2_pair_token0(), "token0")?, 0)
        .ok_or("token0: bad return")?;
    let reserves = eth_call(pair, &abi::v2_pair_get_reserves(), "getReserves")?;
    Ok(V2PairState {
        token0,
        reserve0: abi::u256_at(&reserves, 0).ok_or("getReserves: short return")?,
        reserve1: abi::u256_at(&reserves, 1).ok_or("getReserves: short return")?,
    })
}
