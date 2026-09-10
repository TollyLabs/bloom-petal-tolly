//! Hand-rolled ABI encoding/decoding for the exact calls this Petal makes.
//!
//! Selectors and topics are literal constants; `tests::selectors_match_keccak`
//! re-derives every one of them from its signature, and the golden tests in
//! `tests::golden_vectors` compare full calldata against
//! `route/tests/fixtures/calldata.json`, produced by an independent encoder
//! (`scripts/gen-calldata-fixtures.py`, Python `eth_abi`).

use alloy_primitives::{Address, U256};
use sha3::{Digest, Keccak256};

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&hasher.finalize());
    out
}

/// First 4 bytes of `keccak256(signature)`.
pub fn selector(signature: &str) -> [u8; 4] {
    let mut out = [0u8; 4];
    out.copy_from_slice(&keccak256(signature.as_bytes())[..4]);
    out
}

// ERC-20
pub const SEL_APPROVE: [u8; 4] = [0x09, 0x5e, 0xa7, 0xb3];
pub const SEL_ALLOWANCE: [u8; 4] = [0xdd, 0x62, 0xed, 0x3e];
pub const SEL_BALANCE_OF: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];
pub const SEL_DECIMALS: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];
pub const SEL_SYMBOL: [u8; 4] = [0x95, 0xd8, 0x9b, 0x41];
// TollyPad
pub const SEL_CREATE_TOKEN: [u8; 4] = [0xb1, 0x86, 0xba, 0xdf];
pub const SEL_PAD_TOKENS: [u8; 4] = [0xe4, 0x86, 0x03, 0x39];
// SwapRouter02
pub const SEL_EXACT_INPUT_SINGLE: [u8; 4] = [0x04, 0xe4, 0x5a, 0xaf];
// TollyMultiRouter
pub const SEL_SWAP_WITH_TOLL: [u8; 4] = [0x17, 0x47, 0x06, 0x3e];
pub const SEL_SWAP_WITH_TOLL_V2: [u8; 4] = [0x2c, 0x29, 0xc0, 0x54];
pub const SEL_TOLL_FOR: [u8; 4] = [0x0d, 0x9a, 0x99, 0x72];
// QuoterV2 / V4Quoter
pub const SEL_QUOTE_EXACT_INPUT_SINGLE_V3: [u8; 4] = [0xc6, 0xa5, 0x02, 0x6a];
pub const SEL_QUOTE_EXACT_INPUT_SINGLE_V4: [u8; 4] = [0xaa, 0x9d, 0x21, 0xcb];
// Uniswap V2 pair / V3 factory
pub const SEL_TOKEN0: [u8; 4] = [0x0d, 0xfe, 0x16, 0x81];
pub const SEL_GET_RESERVES: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];
pub const SEL_GET_POOL: [u8; 4] = [0x16, 0x98, 0xee, 0x82];

/// `TokenCreated(address indexed token, address indexed creator, string name, string symbol, address pool, string imageURI, string website, string twitter, string telegram)`
pub const TOPIC_TOKEN_CREATED: [u8; 32] = [
    0x87, 0x55, 0x22, 0xb0, 0x92, 0xd9, 0xe1, 0x9a, 0x1d, 0xe3, 0x59, 0xe4, 0xbd, 0x21, 0x80, 0x90,
    0xd5, 0x82, 0xfa, 0x52, 0x1c, 0x97, 0x33, 0x88, 0x9a, 0xcf, 0x1a, 0x5f, 0xf1, 0x94, 0x12, 0x55,
];
/// `Transfer(address indexed from, address indexed to, uint256 value)`
pub const TOPIC_TRANSFER: [u8; 32] = [
    0xdd, 0xf2, 0x52, 0xad, 0x1b, 0xe2, 0xc8, 0x9b, 0x69, 0xc2, 0xb0, 0x68, 0xfc, 0x37, 0x8d, 0xaa,
    0x95, 0x2b, 0xa7, 0xf1, 0x63, 0xc4, 0xa1, 0x16, 0x28, 0xf5, 0x5a, 0x4d, 0xf5, 0x23, 0xb3, 0xef,
];
/// `SwappedWithToll(address indexed trader, address indexed tokenIn, address indexed tokenOut, uint256 amountIn, uint256 toll, uint256 amountOut, uint8 venue)`
pub const TOPIC_SWAPPED_WITH_TOLL: [u8; 32] = [
    0xe6, 0x81, 0xb3, 0xd3, 0x69, 0xa9, 0xa0, 0x22, 0xc0, 0xfc, 0xeb, 0x9b, 0x70, 0xda, 0x22, 0x77,
    0xb3, 0x49, 0x62, 0xa3, 0x63, 0x8f, 0x5c, 0xf7, 0xf2, 0xe5, 0xcb, 0x27, 0x3a, 0xd3, 0x08, 0x43,
];

/// A value in the ABI type system, enough for every call this Petal encodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Address(Address),
    Uint(U256),
    /// Signed integer (int24 tick spacing); two's complement in a 32-byte word.
    Int(i64),
    Bool(bool),
    FixedBytes32([u8; 32]),
    Str(String),
    Bytes(Vec<u8>),
    Tuple(Vec<Token>),
}

fn is_dynamic(token: &Token) -> bool {
    match token {
        Token::Str(_) | Token::Bytes(_) => true,
        Token::Tuple(items) => items.iter().any(is_dynamic),
        _ => false,
    }
}

fn head_size(token: &Token) -> usize {
    match token {
        Token::Tuple(items) if !is_dynamic(token) => items.iter().map(head_size).sum(),
        _ => 32,
    }
}

fn word_usize(value: usize) -> [u8; 32] {
    U256::from(value).to_be_bytes::<32>()
}

fn int_word(value: i64) -> [u8; 32] {
    let mut out = if value < 0 { [0xffu8; 32] } else { [0u8; 32] };
    out[24..].copy_from_slice(&(value as u64).to_be_bytes());
    out
}

fn padded(bytes: &[u8]) -> Vec<u8> {
    let mut out = word_usize(bytes.len()).to_vec();
    out.extend_from_slice(bytes);
    let rem = bytes.len() % 32;
    if rem != 0 {
        out.extend(std::iter::repeat_n(0u8, 32 - rem));
    }
    out
}

fn encode_token(token: &Token) -> Vec<u8> {
    match token {
        Token::Address(a) => {
            let mut w = [0u8; 32];
            w[12..].copy_from_slice(a.as_slice());
            w.to_vec()
        }
        Token::Uint(v) => v.to_be_bytes::<32>().to_vec(),
        Token::Int(v) => int_word(*v).to_vec(),
        Token::Bool(b) => word_usize(usize::from(*b)).to_vec(),
        Token::FixedBytes32(b) => b.to_vec(),
        Token::Str(s) => padded(s.as_bytes()),
        Token::Bytes(b) => padded(b),
        Token::Tuple(items) => encode_tuple(items),
    }
}

/// Head/tail encoding of a tuple (also the top-level argument list).
pub fn encode_tuple(items: &[Token]) -> Vec<u8> {
    let head_len: usize = items.iter().map(head_size).sum();
    let mut head = Vec::with_capacity(head_len);
    let mut tail = Vec::new();
    for item in items {
        if is_dynamic(item) {
            head.extend_from_slice(&word_usize(head_len + tail.len()));
            tail.extend(encode_token(item));
        } else {
            head.extend(encode_token(item));
        }
    }
    head.extend(tail);
    head
}

pub fn encode_call(selector: [u8; 4], args: &[Token]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32 * args.len());
    out.extend_from_slice(&selector);
    out.extend(encode_tuple(args));
    out
}

// ---- decoding ----

pub fn word_at(buf: &[u8], index: usize) -> Option<[u8; 32]> {
    let start = index.checked_mul(32)?;
    let slice = buf.get(start..start + 32)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(slice);
    Some(out)
}

pub fn u256_at(buf: &[u8], index: usize) -> Option<U256> {
    word_at(buf, index).map(U256::from_be_bytes)
}

pub fn address_at(buf: &[u8], index: usize) -> Option<Address> {
    let word = word_at(buf, index)?;
    if word[..12].iter().any(|b| *b != 0) {
        return None;
    }
    Some(Address::from_slice(&word[12..]))
}

pub fn bool_at(buf: &[u8], index: usize) -> Option<bool> {
    let value = u256_at(buf, index)?;
    if value == U256::ZERO {
        Some(false)
    } else if value == U256::from(1u64) {
        Some(true)
    } else {
        None
    }
}

/// Decode a single ABI `string` return value.
pub fn decode_string(buf: &[u8]) -> Option<String> {
    let offset = usize::try_from(u256_at(buf, 0)?).ok()?;
    let len = usize::try_from(U256::from_be_bytes(
        *<&[u8; 32]>::try_from(buf.get(offset..offset + 32)?).ok()?,
    ))
    .ok()?;
    let bytes = buf.get(offset + 32..offset.checked_add(32)?.checked_add(len)?)?;
    String::from_utf8(bytes.to_vec()).ok()
}

// ---- call builders ----

pub fn erc20_approve(spender: Address, amount: U256) -> Vec<u8> {
    encode_call(SEL_APPROVE, &[Token::Address(spender), Token::Uint(amount)])
}

pub fn erc20_allowance(owner: Address, spender: Address) -> Vec<u8> {
    encode_call(
        SEL_ALLOWANCE,
        &[Token::Address(owner), Token::Address(spender)],
    )
}

pub fn erc20_balance_of(owner: Address) -> Vec<u8> {
    encode_call(SEL_BALANCE_OF, &[Token::Address(owner)])
}

pub fn erc20_decimals() -> Vec<u8> {
    SEL_DECIMALS.to_vec()
}

pub fn erc20_symbol() -> Vec<u8> {
    SEL_SYMBOL.to_vec()
}

/// Launch metadata, already trimmed per `launchCall.ts#createTokenArgs`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenMeta {
    #[serde(rename = "imageURI")]
    pub image_uri: String,
    pub website: String,
    pub twitter: String,
    pub telegram: String,
}

/// `createToken(string,string,(string,string,string,string),bytes32,uint256)`
pub fn pad_create_token(
    name: &str,
    symbol: &str,
    meta: &TokenMeta,
    salt: [u8; 32],
    dev_buy_quote: U256,
) -> Vec<u8> {
    encode_call(
        SEL_CREATE_TOKEN,
        &[
            Token::Str(name.to_string()),
            Token::Str(symbol.to_string()),
            Token::Tuple(vec![
                Token::Str(meta.image_uri.clone()),
                Token::Str(meta.website.clone()),
                Token::Str(meta.twitter.clone()),
                Token::Str(meta.telegram.clone()),
            ]),
            Token::FixedBytes32(salt),
            Token::Uint(dev_buy_quote),
        ],
    )
}

pub fn pad_tokens(token: Address) -> Vec<u8> {
    encode_call(SEL_PAD_TOKENS, &[Token::Address(token)])
}

/// `TokenInfo { creator, pool, lpTokenId }` — `pool` is word index 1.
pub fn decode_pad_token_info(ret: &[u8]) -> Option<(Address, Address, U256)> {
    Some((address_at(ret, 0)?, address_at(ret, 1)?, u256_at(ret, 2)?))
}

/// `exactInputSingle((tokenIn,tokenOut,fee,recipient,amountIn,amountOutMinimum,sqrtPriceLimitX96))`, limit 0.
pub fn swap_router02_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_minimum: U256,
) -> Vec<u8> {
    encode_call(
        SEL_EXACT_INPUT_SINGLE,
        &[Token::Tuple(vec![
            Token::Address(token_in),
            Token::Address(token_out),
            Token::Uint(U256::from(fee)),
            Token::Address(recipient),
            Token::Uint(amount_in),
            Token::Uint(amount_out_minimum),
            Token::Uint(U256::ZERO),
        ])],
    )
}

/// `swapWithToll(tokenIn,tokenOut,poolFee,amountIn,amountOutMinimum)` — `amountIn` is GROSS.
pub fn multi_router_swap_with_toll(
    token_in: Address,
    token_out: Address,
    pool_fee: u32,
    amount_in: U256,
    amount_out_minimum: U256,
) -> Vec<u8> {
    encode_call(
        SEL_SWAP_WITH_TOLL,
        &[
            Token::Address(token_in),
            Token::Address(token_out),
            Token::Uint(U256::from(pool_fee)),
            Token::Uint(amount_in),
            Token::Uint(amount_out_minimum),
        ],
    )
}

/// `swapWithTollV2(tokenIn,tokenOut,factory,feeBps,amountIn,amountOutMinimum)` — `amountOutMinimum` must be > 0.
pub fn multi_router_swap_with_toll_v2(
    token_in: Address,
    token_out: Address,
    factory: Address,
    fee_bps: u16,
    amount_in: U256,
    amount_out_minimum: U256,
) -> Vec<u8> {
    encode_call(
        SEL_SWAP_WITH_TOLL_V2,
        &[
            Token::Address(token_in),
            Token::Address(token_out),
            Token::Address(factory),
            Token::Uint(U256::from(fee_bps)),
            Token::Uint(amount_in),
            Token::Uint(amount_out_minimum),
        ],
    )
}

pub fn multi_router_toll_for(token_in: Address, token_out: Address, amount_in: U256) -> Vec<u8> {
    encode_call(
        SEL_TOLL_FOR,
        &[
            Token::Address(token_in),
            Token::Address(token_out),
            Token::Uint(amount_in),
        ],
    )
}

/// QuoterV2 `quoteExactInputSingle((tokenIn,tokenOut,amountIn,fee,sqrtPriceLimitX96))`, limit 0.
pub fn quoter_v2_quote_exact_input_single(
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    fee: u32,
) -> Vec<u8> {
    encode_call(
        SEL_QUOTE_EXACT_INPUT_SINGLE_V3,
        &[Token::Tuple(vec![
            Token::Address(token_in),
            Token::Address(token_out),
            Token::Uint(amount_in),
            Token::Uint(U256::from(fee)),
            Token::Uint(U256::ZERO),
        ])],
    )
}

/// A Uniswap V4 PoolKey with currencies sorted ascending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolKey {
    pub currency0: Address,
    pub currency1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub hooks: Address,
}

/// V4Quoter `quoteExactInputSingle(((currency0,currency1,fee,tickSpacing,hooks),zeroForOne,exactAmount,hookData))`, empty hookData.
pub fn v4_quoter_quote_exact_input_single(
    key: &PoolKey,
    zero_for_one: bool,
    exact_amount: U256,
) -> Vec<u8> {
    encode_call(
        SEL_QUOTE_EXACT_INPUT_SINGLE_V4,
        &[Token::Tuple(vec![
            Token::Tuple(vec![
                Token::Address(key.currency0),
                Token::Address(key.currency1),
                Token::Uint(U256::from(key.fee)),
                Token::Int(i64::from(key.tick_spacing)),
                Token::Address(key.hooks),
            ]),
            Token::Bool(zero_for_one),
            Token::Uint(exact_amount),
            Token::Bytes(Vec::new()),
        ])],
    )
}

pub fn v2_pair_token0() -> Vec<u8> {
    SEL_TOKEN0.to_vec()
}

pub fn v2_pair_get_reserves() -> Vec<u8> {
    SEL_GET_RESERVES.to_vec()
}

pub fn v3_factory_get_pool(a: Address, b: Address, fee: u32) -> Vec<u8> {
    encode_call(
        SEL_GET_POOL,
        &[
            Token::Address(a),
            Token::Address(b),
            Token::Uint(U256::from(fee)),
        ],
    )
}

pub fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

pub fn decode_hex0x(value: &str) -> Result<Vec<u8>, String> {
    let body = value.strip_prefix("0x").ok_or("expected 0x-prefixed hex")?;
    hex::decode(body).map_err(|e| format!("hex decode failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn fixtures() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/calldata.json")).expect("fixture JSON")
    }

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    fn u(s: &str) -> U256 {
        U256::from_str_radix(s, 10).unwrap()
    }

    #[test]
    fn selectors_match_keccak() {
        for (sig, expected) in [
            ("approve(address,uint256)", SEL_APPROVE),
            ("allowance(address,address)", SEL_ALLOWANCE),
            ("balanceOf(address)", SEL_BALANCE_OF),
            ("decimals()", SEL_DECIMALS),
            ("symbol()", SEL_SYMBOL),
            (
                "createToken(string,string,(string,string,string,string),bytes32,uint256)",
                SEL_CREATE_TOKEN,
            ),
            ("tokens(address)", SEL_PAD_TOKENS),
            (
                "exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))",
                SEL_EXACT_INPUT_SINGLE,
            ),
            (
                "swapWithToll(address,address,uint24,uint256,uint256)",
                SEL_SWAP_WITH_TOLL,
            ),
            (
                "swapWithTollV2(address,address,address,uint16,uint256,uint256)",
                SEL_SWAP_WITH_TOLL_V2,
            ),
            ("tollFor(address,address,uint256)", SEL_TOLL_FOR),
            (
                "quoteExactInputSingle((address,address,uint256,uint24,uint160))",
                SEL_QUOTE_EXACT_INPUT_SINGLE_V3,
            ),
            (
                "quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))",
                SEL_QUOTE_EXACT_INPUT_SINGLE_V4,
            ),
            ("token0()", SEL_TOKEN0),
            ("getReserves()", SEL_GET_RESERVES),
            ("getPool(address,address,uint24)", SEL_GET_POOL),
        ] {
            assert_eq!(selector(sig), expected, "{sig}");
        }
        let fx = fixtures();
        for (sig, hex) in fx["selectors"].as_object().unwrap() {
            assert_eq!(
                hex::encode(selector(sig)),
                hex.as_str().unwrap(),
                "fixture {sig}"
            );
        }
    }

    #[test]
    fn topics_match_keccak() {
        assert_eq!(
            keccak256(
                b"TokenCreated(address,address,string,string,address,string,string,string,string)"
            ),
            TOPIC_TOKEN_CREATED
        );
        assert_eq!(
            keccak256(b"Transfer(address,address,uint256)"),
            TOPIC_TRANSFER
        );
        assert_eq!(
            keccak256(b"SwappedWithToll(address,address,address,uint256,uint256,uint256,uint8)"),
            TOPIC_SWAPPED_WITH_TOLL
        );
        let fx = fixtures();
        assert_eq!(
            fx["topics"]["TokenCreated(address,address,string,string,address,string,string,string,string)"],
            hex0x(&TOPIC_TOKEN_CREATED)
        );
        assert_eq!(
            fx["topics"]["Transfer(address,address,uint256)"],
            hex0x(&TOPIC_TRANSFER)
        );
    }

    #[test]
    fn golden_create_token_matches_the_frontend_case() {
        let fx = fixtures();
        let case = &fx["createToken_moss"];
        let inputs = &case["inputs"];
        let meta = TokenMeta {
            image_uri: inputs["meta"]["imageURI"].as_str().unwrap().into(),
            website: inputs["meta"]["website"].as_str().unwrap().into(),
            twitter: inputs["meta"]["twitter"].as_str().unwrap().into(),
            telegram: inputs["meta"]["telegram"].as_str().unwrap().into(),
        };
        let calldata = pad_create_token(
            "Moss Coin",
            "MOSS",
            &meta,
            [0x11u8; 32],
            U256::from(5_000_000u64),
        );
        assert_eq!(hex0x(&calldata), case["calldata"].as_str().unwrap());

        let bare = &fx["createToken_no_socials"];
        let meta = TokenMeta {
            image_uri: "ipfs://bafy".into(),
            website: String::new(),
            twitter: String::new(),
            telegram: String::new(),
        };
        let calldata = pad_create_token("Bare", "BARE", &meta, [0x22u8; 32], U256::ZERO);
        assert_eq!(hex0x(&calldata), bare["calldata"].as_str().unwrap());
    }

    #[test]
    fn golden_swap_and_view_vectors() {
        let fx = fixtures();
        let usdc = addr("0x3600000000000000000000000000000000000000");
        let token = addr("0x4753c45fb550fecaa143a47968659117e6ffc2ce");
        let wallet = addr("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let multi = addr("0xf28c138a39c234554c847dbef467b073ddbd7451");
        let factory = addr("0xfafafafafafafafafafafafafafafafafafafafa");
        let zero = Address::ZERO;

        assert_eq!(
            hex0x(&swap_router02_exact_input_single(
                usdc,
                token,
                10_000,
                wallet,
                u("25000000"),
                u("1234567890123456789")
            )),
            fx["exactInputSingle_buy"]["calldata"]
        );
        assert_eq!(
            hex0x(&multi_router_swap_with_toll(
                usdc,
                token,
                500,
                u("25000000"),
                u("999999999999999999999")
            )),
            fx["swapWithToll_buy"]["calldata"]
        );
        assert_eq!(
            hex0x(&multi_router_swap_with_toll_v2(
                usdc,
                token,
                factory,
                30,
                u("25000000"),
                U256::from(1u64)
            )),
            fx["swapWithTollV2_buy"]["calldata"]
        );
        assert_eq!(
            hex0x(&multi_router_toll_for(usdc, token, u("25000000"))),
            fx["tollFor"]["calldata"]
        );
        assert_eq!(
            hex0x(&erc20_approve(multi, u("25000000"))),
            fx["approve"]["calldata"]
        );
        assert_eq!(
            hex0x(&erc20_allowance(wallet, multi)),
            fx["allowance"]["calldata"]
        );
        assert_eq!(
            hex0x(&erc20_balance_of(wallet)),
            fx["balanceOf"]["calldata"]
        );
        assert_eq!(hex0x(&pad_tokens(token)), fx["tokens"]["calldata"]);
        assert_eq!(
            hex0x(&quoter_v2_quote_exact_input_single(
                usdc,
                token,
                u("24950000"),
                500
            )),
            fx["quoteExactInputSingle_v3"]["calldata"]
        );
        let key = PoolKey {
            currency0: zero,
            currency1: token,
            fee: 2500,
            tick_spacing: 25,
            hooks: zero,
        };
        assert_eq!(
            hex0x(&v4_quoter_quote_exact_input_single(
                &key,
                true,
                u("24950000000000000000")
            )),
            fx["quoteExactInputSingle_v4_native_buy"]["calldata"]
        );
        let key = PoolKey {
            currency0: zero,
            currency1: token,
            fee: 3000,
            tick_spacing: -60,
            hooks: zero,
        };
        assert_eq!(
            hex0x(&v4_quoter_quote_exact_input_single(
                &key,
                false,
                U256::from(1u64)
            )),
            fx["quoteExactInputSingle_v4_negative_tick"]["calldata"]
        );
        assert_eq!(
            hex0x(&v3_factory_get_pool(token, usdc, 10_000)),
            fx["getPool"]["calldata"]
        );
        assert_eq!(hex0x(&erc20_decimals()), "0x313ce567");
        assert_eq!(hex0x(&v2_pair_token0()), "0x0dfe1681");
        assert_eq!(hex0x(&v2_pair_get_reserves()), "0x0902f1ac");
    }

    #[test]
    fn golden_decoders() {
        let fx = fixtures();
        // tokens(address): pool is word index 1 (critique M4).
        let ret = decode_hex0x(fx["tokens"]["return_pad_token"].as_str().unwrap()).unwrap();
        let (creator, pool, lp) = decode_pad_token_info(&ret).unwrap();
        assert_eq!(
            format!("{creator:?}"),
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            format!("{pool:?}"),
            fx["tokens"]["return_pad_token_pool"].as_str().unwrap()
        );
        assert_eq!(lp, U256::from(77u64));
        let ret = decode_hex0x(fx["tokens"]["return_external_token"].as_str().unwrap()).unwrap();
        assert_eq!(decode_pad_token_info(&ret).unwrap().1, Address::ZERO);

        let ret = decode_hex0x(fx["quoteExactInputSingle_v3"]["return"].as_str().unwrap()).unwrap();
        assert_eq!(
            u256_at(&ret, 0).unwrap(),
            u(fx["quoteExactInputSingle_v3"]["return_amountOut"]
                .as_str()
                .unwrap())
        );
        let ret = decode_hex0x(
            fx["quoteExactInputSingle_v4_native_buy"]["return"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            u256_at(&ret, 0).unwrap(),
            u(
                fx["quoteExactInputSingle_v4_native_buy"]["return_amountOut"]
                    .as_str()
                    .unwrap()
            )
        );
        let ret = decode_hex0x(fx["getReserves_return"]["return"].as_str().unwrap()).unwrap();
        assert_eq!(
            u256_at(&ret, 0).unwrap(),
            u(fx["getReserves_return"]["reserve0"].as_str().unwrap())
        );
        assert_eq!(
            u256_at(&ret, 1).unwrap(),
            u(fx["getReserves_return"]["reserve1"].as_str().unwrap())
        );
        let ret = decode_hex0x(fx["symbol_return"]["return"].as_str().unwrap()).unwrap();
        assert_eq!(decode_string(&ret).as_deref(), Some("BARC"));
        assert!(decode_string(&[0u8; 16]).is_none());
        assert!(
            address_at(&[0xffu8; 32], 0).is_none(),
            "dirty upper bytes are not an address"
        );
        assert_eq!(
            bool_at(&U256::from(1u64).to_be_bytes::<32>(), 0),
            Some(true)
        );
        assert_eq!(bool_at(&U256::from(2u64).to_be_bytes::<32>(), 0), None);
        assert!(word_at(&[0u8; 40], 1).is_none());
    }

    #[test]
    fn int_words_are_twos_complement() {
        assert_eq!(int_word(25), U256::from(25u64).to_be_bytes::<32>());
        let minus_one = int_word(-1);
        assert!(minus_one.iter().all(|b| *b == 0xff));
        let minus_sixty = U256::from_be_bytes(int_word(-60));
        assert_eq!(minus_sixty, U256::MAX - U256::from(59u64));
    }
}
