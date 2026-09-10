//! Route-flow tests against the recording fake host: exactly what the route
//! files call, with scripted API/chain/outbox replies, asserting on the host
//! calls that left the Petal and the durable records it wrote.

use alloy_primitives::{Address, U256};
use petal::{DispatchResponse, HostStatus, SdkError};
use serde_json::{Value, json};

use crate::abi;
use crate::amount::{addr_hex, usdc6_to_native18};
use crate::api::{ApiRoute, Network, fetch_json, status_document, token_detail};
use crate::constants::{MULTI_ROUTER, PAD, QUOTER_V2, SWAP_ROUTER02, USDC, V4_QUOTER};
use crate::fake_host::{self, FakeHost};
use crate::fee;
use crate::launch::route_launch;
use crate::ops::{self, Kind, NextAction, Status, Step};
use crate::policy;
use crate::positions::positions_document;
use crate::quote::{self, Side};
use crate::swap::{buy_description, route_buy, route_sell};

const WALLET: &str = "main";
const NOW: u64 = 1_789_070_000_000;
const HEALTH_URL: &str = "https://stage.tollylabs.com/api/health";
const MARKETS_URL: &str =
    "https://stage.tollylabs.com/api/tokens?scope=ours&sort=volume&dir=desc&limit=50";
const BARC_URL: &str =
    "https://stage.tollylabs.com/api/token/0x4753c45fb550fecaa143a47968659117e6ffc2ce";
const CALENDAR_URL: &str =
    "https://stage.tollylabs.com/api/token/0x2005cd22ea3c1acfaa9e01d3a178f356bb03c81c";

fn wallet_address() -> Address {
    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        .parse()
        .unwrap()
}
fn barc() -> Address {
    "0x4753c45fb550fecaa143a47968659117e6ffc2ce"
        .parse()
        .unwrap()
}
fn calendar() -> Address {
    "0x2005cd22ea3c1acfaa9e01d3a178f356bb03c81c"
        .parse()
        .unwrap()
}
fn barc_detail() -> Value {
    serde_json::from_str(include_str!("../tests/fixtures/stage-token-barc.json")).unwrap()
}
fn calendar_detail() -> Value {
    let list: Value =
        serde_json::from_str(include_str!("../tests/fixtures/stage-tokens-ours.json")).unwrap();
    json!({ "token": list["tokens"][0].clone(), "buys24h": 9, "sells24h": 3, "quoteDecimals": 6, "collisions": [], "rank": 1 })
}
fn health() -> Value {
    serde_json::from_str(include_str!("../tests/fixtures/stage-health.json")).unwrap()
}
fn markets() -> Value {
    serde_json::from_str(include_str!("../tests/fixtures/stage-tokens-ours.json")).unwrap()
}
fn u(v: u128) -> U256 {
    U256::from(v)
}
fn hex(data: &[u8]) -> String {
    hex::encode(data)
}
fn code(response: &DispatchResponse) -> i32 {
    match response {
        DispatchResponse::Error { code, .. } => *code,
        DispatchResponse::Write => 0,
        DispatchResponse::Read(_) => 1,
    }
}
fn message(response: &DispatchResponse) -> String {
    match response {
        DispatchResponse::Error { message, .. } => message.clone(),
        other => format!("{other:?}"),
    }
}
fn read_json(response: DispatchResponse) -> Value {
    match response {
        DispatchResponse::Read(bytes) => {
            serde_json::from_slice(&bytes).expect("read response is JSON")
        }
        other => panic!("expected a read response, got {other:?}"),
    }
}
fn record(id: &str) -> Value {
    fake_host::with(|h| {
        h.state_json(&ops::store_key(WALLET, id))
            .expect("operation record exists")
    })
}
fn word_ret(value: U256) -> Vec<u8> {
    value.to_be_bytes::<32>().to_vec()
}

/// Gross 25 USDC, external token: fee 50_000, net 24_950_000.
const GROSS: u128 = 25_000_000;
const NET: u128 = 24_950_000;
/// Token outputs (18-dec) for the scripted venues.
const V3_500_OUT: u128 = 1_200_000_000_000_000_000_000; // 1200 BARC
const V3_3000_OUT: u128 = 1_100_000_000_000_000_000_000; // 1100 BARC
const V4_OUT: u128 = 1_300_000_000_000_000_000_000; // 1300 BARC (best, unsupported)

/// A host that serves the stage API fixtures, the wallet, and a BARC buy of
/// 25 USDC across the three venues.
fn host_for_barc_buy() -> FakeHost {
    let mut host = FakeHost::new(NOW);
    host.set_setting(policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE);
    host.seed_vfs(
        &format!("wallets/{WALLET}/address"),
        b"0xAAaAaAaaAaAaaaaAaAAaAaaAaAAAAaAAAAAAAAAA\n",
    );
    host.reply_http(HEALTH_URL, 200, &health());
    host.reply_http(MARKETS_URL, 200, &markets());
    host.reply_http(BARC_URL, 200, &barc_detail());
    // Quotes at NET (never gross); reference quotes hit the selector-only fallbacks.
    host.call_u256(
        QUOTER_V2,
        &hex(&abi::quoter_v2_quote_exact_input_single(
            USDC,
            barc(),
            u(NET),
            500,
        )),
        u(V3_500_OUT),
    );
    host.call_u256(
        QUOTER_V2,
        &hex(&abi::quoter_v2_quote_exact_input_single(
            USDC,
            barc(),
            u(NET),
            3000,
        )),
        u(V3_3000_OUT),
    );
    host.call_u256(QUOTER_V2, "c6a5026a", u(1_300_000_000_000_000_000)); // reference (1/1000 size)
    let key = abi::PoolKey {
        currency0: Address::ZERO,
        currency1: barc(),
        fee: 2500,
        tick_spacing: 25,
        hooks: Address::ZERO,
    };
    host.call_u256(
        V4_QUOTER,
        &hex(&abi::v4_quoter_quote_exact_input_single(
            &key,
            true,
            usdc6_to_native18(u(NET)),
        )),
        u(V4_OUT),
    );
    host.call_u256(V4_QUOTER, "aa9d21cb", u(1_400_000_000_000_000_000));
    host.call_u256(MULTI_ROUTER, "0d9a9972", u(50_000)); // tollFor == local fee
    host.call_bytes(PAD, "e4860339", &[0u8; 96]); // PAD.tokens(BARC): external (pool == 0)
    host.balance(wallet_address(), u(100_000_000_000_000_000_000)); // 100 USDC native
    host.call_u256(
        USDC,
        &hex(&abi::erc20_balance_of(wallet_address())),
        u(100_000_000),
    ); // 100 USDC erc20
    host.call_u256(USDC, "dd62ed3e", U256::ZERO); // allowance 0
    host.call_u256(
        barc(),
        &hex(&abi::erc20_balance_of(wallet_address())),
        U256::ZERO,
    ); // BARC balance before
    host.call_u256(USDC, "095ea7b3", u(1)); // approve pre-flight returns true
    host.call_u256(MULTI_ROUTER, "1747063e", u(V3_500_OUT)); // swapWithToll pre-flight
    host
}

fn buy_body(id: &str, amount: &str, extra: Value) -> Vec<u8> {
    let mut body = json!({ "operationId": id, "token": addr_hex(barc()), "amount_usdc": amount });
    if let Value::Object(map) = extra {
        for (k, v) in map {
            body[k] = v;
        }
    }
    serde_json::to_vec(&body).unwrap()
}

// ---- reads ----

#[test]
fn status_projects_health_and_write_gate() {
    fake_host::install(host_for_barc_buy());
    let health = fetch_json(Network::Stage, &ApiRoute::Health).unwrap();
    let doc = status_document(Network::Stage, &health, policy::writes_enabled(), NOW);
    assert_eq!(doc["schema"], "tolly.status.v1");
    assert_eq!(doc["network"], "stage");
    assert_eq!(doc["chain_id"], 5042);
    assert_eq!(doc["writes_enabled"], true);
    assert_eq!(doc["pad_matches_constants"], true);
    fake_host::with(|h| assert_eq!(h.http_calls[0].url, HEALTH_URL));
}

#[test]
fn api_failures_map_to_backend_and_not_found() {
    let mut host = FakeHost::new(NOW);
    host.reply_http_bytes(HEALTH_URL, 200, b"<html>not json</html>");
    host.reply_http(MARKETS_URL, 500, &json!({"error": "boom"}));
    host.reply_http(BARC_URL, 404, &json!({"error": "unknown token"}));
    fake_host::install(host);
    let err = fetch_json(Network::Stage, &ApiRoute::Health).unwrap_err();
    assert_eq!(code(&err.response()), -4);
    let err = fetch_json(Network::Stage, &ApiRoute::Markets).unwrap_err();
    assert_eq!(code(&err.response()), -4);
    assert_eq!(code(&token_detail(Network::Stage, barc()).unwrap_err()), -1);
    // No reply scripted at all -> backend, sanitized.
    assert_eq!(
        code(&token_detail(Network::Stage, calendar()).unwrap_err()),
        -4
    );
}

#[test]
fn prod_network_is_refused_until_its_manifest_release() {
    let mut host = FakeHost::new(NOW);
    host.set_setting(crate::api::NETWORK_SETTING, "prod");
    fake_host::install(host);
    assert_eq!(code(&Network::current().unwrap_err()), -2);
    let mut host = FakeHost::new(NOW);
    host.set_setting(crate::api::NETWORK_SETTING, "mainnet");
    fake_host::install(host);
    assert_eq!(code(&Network::current().unwrap_err()), -3);
    fake_host::install(FakeHost::new(NOW));
    assert_eq!(Network::current().unwrap(), Network::Stage);
}

#[test]
fn token_detail_refuses_an_address_mismatch() {
    let mut host = FakeHost::new(NOW);
    host.reply_http(CALENDAR_URL, 200, &barc_detail());
    fake_host::install(host);
    let err = token_detail(Network::Stage, calendar()).unwrap_err();
    assert_eq!(code(&err), -4);
    assert!(message(&err).contains("does not match"));
}

#[test]
fn buy_quote_ranks_v4_best_but_executes_v3_and_quotes_at_net() {
    fake_host::install(host_for_barc_buy());
    let detail = token_detail(Network::Stage, barc()).unwrap();
    let q = quote::quote(&detail, Side::Buy, u(GROSS), 500);
    assert_eq!(q.fee_raw, u(50_000));
    assert_eq!(q.amount_to_pool_raw, u(NET));
    assert_eq!(q.cross_check.label(), "tollFor-matched");
    assert_eq!(q.venues[0].venue.kind.name(), "v4");
    assert_eq!(q.venues[0].out_raw, Some(u(V4_OUT)));
    assert!(!q.venues[0].execution.supported);
    assert_eq!(
        q.venues[0].amount_in_raw,
        usdc6_to_native18(u(NET)),
        "native-quote V4 is asked in 18 decimals (M3)"
    );
    assert_eq!(q.venues[1].venue.fee, Some(500));
    assert_eq!(q.best, Some(0));
    assert_eq!(q.best_executable, Some(1));
    assert_eq!(q.amount_out_minimum_raw, fee::protect(u(V3_500_OUT), 500));
    let worse = q.worse_than_best_pct.unwrap();
    assert!(
        (worse - (1.0 - 1200.0 / 1300.0) * 100.0).abs() < 1e-6,
        "{worse}"
    );
    assert!(
        q.warnings.iter().any(|w| w.contains("allow_worse_venue")),
        "{:?}",
        q.warnings
    );
    let doc = quote::quote_document(&q);
    assert_eq!(doc["schema"], "tolly.quote.v1");
    assert_eq!(doc["amount_in"]["raw"], GROSS.to_string());
    assert_eq!(
        doc["best_executable"]["id"],
        "0xd4b57ff3ed12c5ee9082891aeced9db702f868aa"
    );
    assert_eq!(doc["best_executable"]["router_call"], "swapWithToll");
    assert_eq!(doc["best"]["execution"], "unsupported");
    assert!(
        doc.get("block").is_none(),
        "no block number is available through bloom:chain"
    );
    fake_host::with(|h| {
        let quoter_calls = h.eth_calls_to(QUOTER_V2);
        assert_eq!(quoter_calls.len(), 4, "two venues x (full + reference)");
        for call in &quoter_calls {
            assert!(
                !call.data().unwrap().contains(&format!("{:064x}", GROSS)),
                "quotes must be at net, never gross"
            );
        }
        assert!(
            h.chain_methods()
                .iter()
                .all(|m| matches!(*m, "eth_call" | "eth_getBalance")),
            "only allowlisted methods: {:?}",
            h.chain_methods()
        );
    });
}

#[test]
fn quoter_revert_means_no_fill_and_pad_tokens_skip_the_fee() {
    let mut host = FakeHost::new(NOW);
    host.reply_http(CALENDAR_URL, 200, &calendar_detail());
    host.call_error(
        QUOTER_V2,
        "c6a5026a",
        "eth_call: execution reverted https://rpc.example/v3/SECRET",
    );
    fake_host::install(host);
    let detail = token_detail(Network::Stage, calendar()).unwrap();
    let q = quote::quote(&detail, Side::Buy, u(GROSS), 500);
    assert_eq!(q.fee_raw, U256::ZERO, "pad tokens pay no interface fee");
    assert_eq!(q.amount_to_pool_raw, u(GROSS));
    assert_eq!(q.cross_check.label(), "skipped");
    assert_eq!(q.venues.len(), 1);
    assert_eq!(q.venues[0].out_raw, None);
    assert_eq!(q.venues[0].error.as_deref(), Some("no-fill-at-size"));
    assert_eq!(q.best, None);
    assert!(q.warnings[0].contains("no venue can fill"));
    let doc = serde_json::to_string(&quote::quote_document(&q)).unwrap();
    assert!(!doc.contains("SECRET"), "quote never echoes RPC internals");
    fake_host::with(|h| {
        assert!(
            h.eth_calls_to(MULTI_ROUTER).is_empty(),
            "no tollFor cross-check for a pad token"
        )
    });
}

#[test]
fn sell_quote_normalises_native_v4_output_to_six_decimals() {
    let mut host = FakeHost::new(NOW);
    host.reply_http(BARC_URL, 200, &barc_detail());
    let amount = u(1_000_000_000_000_000_000_000); // 1000 BARC
    // V4 native sell pays 30 USDC in 18-dec native units; V3 500 pays 29.5 USDC in 6-dec.
    host.call_u256(V4_QUOTER, "aa9d21cb", u(30_000_000_000_000_000_000));
    host.call_u256(
        QUOTER_V2,
        &hex(&abi::quoter_v2_quote_exact_input_single(
            barc(),
            USDC,
            amount,
            500,
        )),
        u(29_500_000),
    );
    host.call_u256(QUOTER_V2, "c6a5026a", u(29_600));
    fake_host::install(host);
    let detail = token_detail(Network::Stage, barc()).unwrap();
    let q = quote::quote(&detail, Side::Sell, amount, 500);
    assert_eq!(q.fee_raw, U256::ZERO);
    assert_eq!(q.venues[0].venue.kind.name(), "v4");
    assert_eq!(
        q.venues[0].out_raw,
        Some(u(30_000_000)),
        "normalised to 6 decimals before ranking"
    );
    assert_eq!(
        q.venues[0].out_raw_native,
        Some(u(30_000_000_000_000_000_000))
    );
    assert_eq!(q.venues[1].out_raw, Some(u(29_500_000)));
    assert_eq!(q.best_executable, Some(1));
    let doc = quote::quote_document(&q);
    assert_eq!(doc["amount_out_asset"]["decimals"], 6);
    assert_eq!(doc["best_executable"]["out_human"], "29.5");
    fake_host::with(|h| {
        let v4 = h.eth_calls_to(V4_QUOTER);
        assert!(
            v4[0].data().unwrap().contains(&format!("{:064x}", amount)),
            "a sell asks the V4 quoter in token units"
        );
    });
}

// ---- buy walk ----

#[test]
fn writes_are_denied_until_the_owner_enables_them() {
    let mut host = host_for_barc_buy();
    host.set_setting(policy::WRITES_SETTING, "no");
    fake_host::install(host);
    let r = route_buy(WALLET, &buy_body("buy-1", "25", json!({})));
    assert_eq!(code(&r), -2, "{}", message(&r));
    assert!(message(&r).contains("writes-disabled"));
    fake_host::with(|h| {
        assert!(h.state_keys().is_empty(), "nothing is recorded");
        assert!(h.staged.is_empty());
        assert!(h.http_calls.is_empty(), "no upstream call before the gate");
    });
}

#[test]
fn buy_input_validation() {
    fake_host::install(host_for_barc_buy());
    assert_eq!(code(&route_buy(WALLET, b"not json")), -3);
    assert_eq!(
        code(&route_buy(
            WALLET,
            &buy_body("buy-1", "25", json!({"extra": 1}))
        )),
        -3,
        "unknown fields are rejected"
    );
    assert_eq!(
        code(&route_buy(WALLET, &vec![b' '; policy::MAX_BODY_BYTES + 1])),
        -3,
        "oversized body"
    );
    assert_eq!(
        code(&route_buy(WALLET, &buy_body("Bad Id", "25", json!({})))),
        -3
    );
    assert_eq!(
        code(&route_buy(WALLET, &buy_body("buy-1", "0", json!({})))),
        -3
    );
    assert_eq!(
        code(&route_buy(
            WALLET,
            &buy_body("buy-1", "250.000001", json!({}))
        )),
        -3,
        "cap"
    );
    assert_eq!(
        code(&route_buy(
            WALLET,
            &buy_body("buy-1", "25", json!({"slippage_bps": 10}))
        )),
        -3
    );
    assert_eq!(
        code(&route_buy(
            WALLET,
            &buy_body("buy-1", "25", json!({"venue": "nope"}))
        )),
        -3
    );
    assert_eq!(
        code(&route_buy(
            WALLET,
            &buy_body("buy-1", "25", json!({"min_out_raw": "1.5"}))
        )),
        -3
    );
    let mut bad_token =
        json!({ "operationId": "buy-1", "token": addr_hex(USDC), "amount_usdc": "25" });
    assert_eq!(
        code(&route_buy(WALLET, &serde_json::to_vec(&bad_token).unwrap())),
        -3
    );
    bad_token["token"] = json!("0x12");
    assert_eq!(
        code(&route_buy(WALLET, &serde_json::to_vec(&bad_token).unwrap())),
        -3
    );
    assert_eq!(
        code(&route_buy(
            "team/alice",
            &buy_body("buy-1", "25", json!({}))
        )),
        -3,
        "wallet ids are single segments"
    );
    assert_eq!(
        code(&route_buy(
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &buy_body("buy-1", "25", json!({}))
        )),
        -3
    );
    fake_host::with(|h| assert!(h.staged.is_empty()));
}

#[test]
fn buy_refuses_a_better_unsupported_venue_unless_accepted() {
    fake_host::install(host_for_barc_buy());
    let r = route_buy(WALLET, &buy_body("buy-1", "25", json!({})));
    assert_eq!(code(&r), -3, "{}", message(&r));
    assert!(message(&r).contains("better-venue-unsupported"));
    let rec = record("buy-1");
    assert_eq!(rec["status"], "failed");
    assert_eq!(rec["error"]["code"], "better-venue-unsupported");
    assert_eq!(rec["error"]["retryable"], true);
    assert_eq!(rec["next_action"], "retry");
    fake_host::with(|h| assert!(h.staged.is_empty()));
}

#[test]
fn buy_walk_approve_then_swap_with_gross_and_fresh_floor() {
    fake_host::install(host_for_barc_buy());
    let body = buy_body("buy-1", "25", json!({"allow_worse_venue": true}));

    // 1. Allowance short: exactly one approve is staged.
    let r = route_buy(WALLET, &body);
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 1);
        let tx = &h.staged[0];
        assert_eq!(tx.wallet, WALLET);
        assert_eq!(tx.chain, "arc");
        assert_eq!(tx.to, addr_hex(USDC));
        assert_eq!(tx.value_wei, "0");
        assert_eq!(
            tx.data_hex,
            abi::hex0x(&abi::erc20_approve(MULTI_ROUTER, u(GROSS))),
            "exact gross approval to the multi router"
        );
        assert_eq!(tx.nonce, None);
        assert_eq!(tx.max_fee_per_gas, None);
        assert_eq!(tx.max_priority_fee_per_gas, None);
        let preflight = h
            .chain_calls
            .iter()
            .find(|c| c.data().is_some_and(|d| d.starts_with("0x095ea7b3")))
            .expect("approve pre-flight");
        assert_eq!(
            preflight.from().as_deref(),
            Some(addr_hex(wallet_address()).as_str()),
            "pre-flight simulates from the wallet"
        );
    });
    let rec = record("buy-1");
    assert_eq!(rec["schema"], "tolly.operation.v1");
    assert_eq!(rec["status"], "staged");
    assert_eq!(rec["step"], "approve");
    assert_eq!(rec["next_action"], "confirm_in_bloom");
    assert_eq!(
        rec["confirm_path"],
        "/bloom/wallets/main/chains/arc/outbox/pending/ob-1/confirm"
    );
    assert_eq!(rec["wallet_address"], addr_hex(wallet_address()));
    assert_eq!(
        rec["plan"]["venue"]["id"],
        "0xd4b57ff3ed12c5ee9082891aeced9db702f868aa"
    );
    assert_eq!(rec["plan"]["router_call"], "swapWithToll");
    assert_eq!(rec["plan"]["interface_fee_raw"], "50000");
    assert_eq!(rec["plan"]["amount_to_pool_raw"], NET.to_string());
    assert_eq!(rec["txs"][0]["role"], "approve");
    assert_eq!(rec["txs"][0]["spender"], addr_hex(MULTI_ROUTER));
    assert_eq!(rec["txs"][0]["attempt_params"]["allow_worse_venue"], true);
    assert_eq!(
        rec["result"],
        Value::Null,
        "a write never claims completion"
    );

    // 2. Re-POST while the entry is pending: no-op refresh, no second stage.
    assert_eq!(route_buy(WALLET, &body), DispatchResponse::Write);
    fake_host::with(|h| assert_eq!(h.staged.len(), 1));
    assert_eq!(record("buy-1")["status"], "staged");

    // 3. A different economic tuple under the same id is refused.
    let r = route_buy(
        WALLET,
        &buy_body("buy-1", "26", json!({"allow_worse_venue": true})),
    );
    assert_eq!(code(&r), -3);
    assert!(message(&r).contains("already bound"));
    // Execution parameters may change (M2).
    assert_eq!(
        route_buy(
            WALLET,
            &buy_body(
                "buy-1",
                "25",
                json!({"allow_worse_venue": true, "slippage_bps": 300})
            )
        ),
        DispatchResponse::Write
    );

    // 4. Owner confirmed; the approve mined. Re-POST stages the swap with GROSS amount.
    fake_host::with(|h| {
        h.set_outbox(
            "ob-1",
            "success",
            Some("0xa1"),
            Some(&json!({"outcome": "success", "tx_hash": "0xa1", "block_number": 20185400})),
        );
        h.reply_chain(
            "eth_call",
            Some(USDC),
            "dd62ed3e",
            Ok(serde_json::to_string(&format!("0x{:064x}", GROSS)).unwrap()),
        );
    });
    let r = route_buy(WALLET, &body);
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    let floor = fee::protect(u(V3_500_OUT), 500).unwrap();
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 2);
        let tx = &h.staged[1];
        assert_eq!(tx.to, addr_hex(MULTI_ROUTER));
        assert_eq!(tx.value_wei, "0");
        assert_eq!(
            tx.data_hex,
            abi::hex0x(&abi::multi_router_swap_with_toll(
                USDC,
                barc(),
                500,
                u(GROSS),
                floor
            )),
            "gross amountIn, fresh protected floor"
        );
        let preflight = h
            .chain_calls
            .iter()
            .filter(|c| c.data().is_some_and(|d| d.starts_with("0x1747063e")))
            .count();
        assert_eq!(preflight, 1, "swap pre-flight ran exactly once");
    });
    let rec = record("buy-1");
    assert_eq!(rec["status"], "staged");
    assert_eq!(rec["step"], "swap");
    assert_eq!(rec["txs"][0]["outcome"], "success");
    assert_eq!(rec["txs"][0]["block_number"], 20185400);
    assert_eq!(rec["txs"][1]["role"], "swap");
    assert_eq!(rec["txs"][1]["balance_before_raw"], "0");
    assert_eq!(rec["plan"]["amount_out_minimum_raw"], floor.to_string());

    // 5. Swap mined: the read reconciles to confirmed, then completes from the balance delta.
    fake_host::with(|h| {
        h.set_outbox(
            "ob-2",
            "success",
            Some("0xb2"),
            Some(&json!({"outcome": "success", "tx_hash": "0xb2", "block_number": 20185500})),
        );
        h.reply_chain(
            "eth_call",
            Some(barc()),
            &hex(&abi::erc20_balance_of(wallet_address())),
            Ok(serde_json::to_string(&format!("0x{:064x}", V3_500_OUT)).unwrap()),
        );
    });
    let doc = read_json(ops::read_operation(WALLET, "buy-1", Network::Stage));
    assert_eq!(doc["status"], "completed");
    assert_eq!(doc["next_action"], "none");
    assert_eq!(doc["result"]["method"], "balance_delta");
    assert_eq!(doc["result"]["amount_out_raw"], V3_500_OUT.to_string());
    assert_eq!(doc["result"]["amount_out_human"], "1200");
    assert_eq!(doc["result"]["tx_hash"], "0xb2");
    // Terminal: a re-POST is a no-op, and a host regression cannot undo it.
    assert_eq!(route_buy(WALLET, &body), DispatchResponse::Write);
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 2);
        h.set_outbox("ob-2", "pending", None, None);
    });
    assert_eq!(
        read_json(ops::read_operation(WALLET, "buy-1", Network::Stage))["status"],
        "completed"
    );
}

#[test]
fn buy_walk_venue_changed_after_approval_needs_an_explicit_pin() {
    fake_host::install(host_for_barc_buy());
    let body = buy_body("buy-2", "25", json!({"allow_worse_venue": true}));
    assert_eq!(route_buy(WALLET, &body), DispatchResponse::Write);
    fake_host::with(|h| {
        h.set_outbox(
            "ob-1",
            "success",
            Some("0xa1"),
            Some(&json!({"outcome": "success", "tx_hash": "0xa1", "block_number": 1})),
        );
        h.reply_chain(
            "eth_call",
            Some(USDC),
            "dd62ed3e",
            Ok(serde_json::to_string(&format!("0x{:064x}", GROSS)).unwrap()),
        );
        // The 3000 tier now pays more than the 500 tier.
        h.call_u256(
            QUOTER_V2,
            &hex(&abi::quoter_v2_quote_exact_input_single(
                USDC,
                barc(),
                u(NET),
                3000,
            )),
            u(V3_500_OUT + 1),
        );
    });
    let r = route_buy(WALLET, &body);
    assert_eq!(code(&r), -3, "{}", message(&r));
    assert!(message(&r).contains("venue-changed"));
    let rec = record("buy-2");
    assert_eq!(rec["status"], "failed");
    assert_eq!(rec["error"]["code"], "venue-changed");
    assert_eq!(rec["error"]["retryable"], true);
    assert_eq!(
        rec["txs"][0]["outcome"], "success",
        "the approve stays recorded"
    );
    assert_eq!(rec["txs"][0]["superseded"], false);
    fake_host::with(|h| assert_eq!(h.staged.len(), 1));

    // Pin the new winner: the swap is staged on the 3000 tier.
    let pinned = buy_body(
        "buy-2",
        "25",
        json!({"allow_worse_venue": true, "venue": "0x27b5ad78eb1705417ac0f2e769a02aadc4663185"}),
    );
    let r = route_buy(WALLET, &pinned);
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 2);
        assert!(
            h.staged[1].data_hex.contains(&format!("{:064x}", 3000)),
            "fee tier 3000 in the calldata"
        );
    });
    assert_eq!(record("buy-2")["plan"]["venue"]["fee"], 3000);
}

#[test]
fn stage_denial_is_terminal_and_backend_failure_is_retryable() {
    let mut host = host_for_barc_buy();
    host.fail_next_stage(SdkError::Host(HostStatus::Denied));
    fake_host::install(host);
    let body = buy_body("buy-3", "25", json!({"allow_worse_venue": true}));
    let r = route_buy(WALLET, &body);
    assert_eq!(code(&r), -2, "{}", message(&r));
    let rec = record("buy-3");
    assert_eq!(rec["error"]["code"], "policy-denied");
    assert_eq!(rec["error"]["retryable"], false);
    assert_eq!(rec["next_action"], "none");
    assert_eq!(
        route_buy(WALLET, &body),
        DispatchResponse::Write,
        "terminal: no-op"
    );
    fake_host::with(|h| assert!(h.staged.is_empty()));

    let mut host = host_for_barc_buy();
    host.fail_next_stage(SdkError::Message(
        "stage EVM outbox: structured valuation unavailable for staged tx".into(),
    ));
    fake_host::install(host);
    let r = route_buy(WALLET, &body);
    assert_eq!(code(&r), -2);
    assert_eq!(record("buy-3")["error"]["code"], "valuation-unavailable");

    let mut host = host_for_barc_buy();
    host.fail_next_stage(SdkError::Message(
        "stage EVM outbox: provider https://arc.example/rpc/KEY123 timed out".into(),
    ));
    fake_host::install(host);
    let r = route_buy(WALLET, &body);
    assert_eq!(code(&r), -4);
    let rec = record("buy-3");
    assert_eq!(rec["error"]["code"], "stage-failed");
    assert_eq!(rec["error"]["retryable"], true);
    assert!(
        !rec.to_string().contains("KEY123"),
        "no RPC key in the record"
    );
    assert!(!message(&r).contains("KEY123"));
    // The retry stages normally.
    assert_eq!(route_buy(WALLET, &body), DispatchResponse::Write);
    fake_host::with(|h| assert_eq!(h.staged.len(), 1));
    assert_eq!(record("buy-3")["status"], "staged");
}

#[test]
fn persist_failure_after_stage_reports_the_outbox_id() {
    let mut host = host_for_barc_buy();
    host.fail_store_after = Some(2); // claim + pre-stage save succeed, the post-stage save fails
    fake_host::install(host);
    let r = route_buy(
        WALLET,
        &buy_body("buy-4", "25", json!({"allow_worse_venue": true})),
    );
    assert_eq!(code(&r), -4);
    assert!(message(&r).contains("outbox_id=ob-1"), "{}", message(&r));
    fake_host::with(|h| assert_eq!(h.staged.len(), 1));
    assert_eq!(
        record("buy-4")["status"],
        "created",
        "the record never claims the stage it could not persist"
    );
}

#[test]
fn preflight_revert_refuses_before_any_stage() {
    let mut host = host_for_barc_buy();
    host.replace_chain(
        "eth_call",
        Some(USDC),
        "095ea7b3",
        Err(SdkError::Message("eth_call: execution reverted".into())),
    );
    fake_host::install(host);
    let r = route_buy(
        WALLET,
        &buy_body("buy-5", "25", json!({"allow_worse_venue": true})),
    );
    assert_eq!(code(&r), -4, "{}", message(&r));
    assert!(message(&r).contains("preflight-reverted"));
    let rec = record("buy-5");
    assert_eq!(rec["error"]["code"], "preflight-reverted");
    assert_eq!(rec["error"]["retryable"], true);
    fake_host::with(|h| assert!(h.staged.is_empty()));
}

#[test]
fn toll_for_mismatch_refuses_the_write() {
    let mut host = host_for_barc_buy();
    host.reply_chain(
        "eth_call",
        Some(MULTI_ROUTER),
        "0d9a9972",
        Ok(serde_json::to_string(&format!("0x{:064x}", 0)).unwrap()),
    );
    fake_host::install(host);
    // The last scripted reply repeats, so tollFor now answers 0 (the contract thinks it is ours).
    fake_host::with(|h| {
        let _ = h; // first scripted reply (50_000) is consumed by the quote below only once
    });
    let detail = token_detail(Network::Stage, barc()).unwrap();
    let _ = quote::quote(&detail, Side::Buy, u(GROSS), 500); // consumes the 50_000 reply
    let r = route_buy(
        WALLET,
        &buy_body("buy-6", "25", json!({"allow_worse_venue": true})),
    );
    assert_eq!(code(&r), -4, "{}", message(&r));
    assert!(message(&r).contains("fee-mismatch"));
    assert_eq!(record("buy-6")["error"]["retryable"], false);
    fake_host::with(|h| assert!(h.staged.is_empty()));
}

#[test]
fn provenance_mismatch_refuses_the_write() {
    let mut host = host_for_barc_buy();
    let mut pad_says_ours = vec![0u8; 96];
    pad_says_ours[44..64].copy_from_slice(&[0x50u8; 20]); // pool != 0 at word 1
    host.reply_chain(
        "eth_call",
        Some(PAD),
        "e4860339",
        Ok(serde_json::to_string(&format!("0x{}", hex(&pad_says_ours))).unwrap()),
    );
    fake_host::install(host);
    fake_host::with(|h| {
        // consume the first (external) reply so the second (ours) is served to the write
        let _ = h.chain("eth_call", &json!([{ "to": addr_hex(PAD), "data": abi::hex0x(&abi::pad_tokens(barc())) }, "latest"]).to_string());
    });
    let r = route_buy(
        WALLET,
        &buy_body("buy-7", "25", json!({"allow_worse_venue": true})),
    );
    assert_eq!(code(&r), -4, "{}", message(&r));
    assert!(message(&r).contains("provenance-mismatch"));
}

#[test]
fn insufficient_funds_is_a_retryable_refusal() {
    let mut host = host_for_barc_buy();
    host.reply_chain(
        "eth_call",
        Some(USDC),
        &hex(&abi::erc20_balance_of(wallet_address())),
        Ok(serde_json::to_string(&format!("0x{:064x}", 1_000_000)).unwrap()),
    );
    fake_host::install(host);
    fake_host::with(|h| {
        let _ = h.chain("eth_call", &json!([{ "to": addr_hex(USDC), "data": abi::hex0x(&abi::erc20_balance_of(wallet_address())) }, "latest"]).to_string());
    });
    let r = route_buy(
        WALLET,
        &buy_body("buy-8", "25", json!({"allow_worse_venue": true})),
    );
    assert_eq!(code(&r), -3, "{}", message(&r));
    assert!(message(&r).contains("insufficient-funds"));
    assert_eq!(record("buy-8")["error"]["retryable"], true);
    fake_host::with(|h| assert!(h.staged.is_empty()));

    let mut host = host_for_barc_buy();
    host.balance(wallet_address(), u(1)); // later rule for the same address wins? no: first match wins, so rebuild
    let _ = host;
    let mut host = FakeHost::new(NOW);
    host.set_setting(policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE);
    host.seed_vfs(
        &format!("wallets/{WALLET}/address"),
        addr_hex(wallet_address()).as_bytes(),
    );
    host.reply_http(BARC_URL, 200, &barc_detail());
    host.call_u256(QUOTER_V2, "c6a5026a", u(V3_500_OUT));
    host.call_u256(V4_QUOTER, "aa9d21cb", U256::ZERO);
    host.call_u256(MULTI_ROUTER, "0d9a9972", u(50_000));
    host.call_bytes(PAD, "e4860339", &[0u8; 96]);
    host.balance(wallet_address(), u(1)); // 1 wei of native USDC: cannot cover the amount + gas reserve
    fake_host::install(host);
    let r = route_buy(WALLET, &buy_body("buy-9", "25", json!({})));
    assert_eq!(code(&r), -3, "{}", message(&r));
    assert!(message(&r).contains("gas reserve"));
}

#[test]
fn one_live_entry_per_wallet_and_token() {
    fake_host::install(host_for_barc_buy());
    assert_eq!(
        route_buy(
            WALLET,
            &buy_body("buy-a", "25", json!({"allow_worse_venue": true}))
        ),
        DispatchResponse::Write
    );
    let r = route_buy(
        WALLET,
        &buy_body("buy-b", "10", json!({"allow_worse_venue": true})),
    );
    assert_eq!(code(&r), -2, "{}", message(&r));
    assert!(message(&r).contains("buy-a"));
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 1);
        // Once the first entry is no longer pending the second may proceed.
        h.set_outbox("ob-1", "cancelled", None, None);
        h.call_u256(
            QUOTER_V2,
            &hex(&abi::quoter_v2_quote_exact_input_single(
                USDC,
                barc(),
                u(9_980_000),
                500,
            )),
            u(V3_500_OUT),
        );
        h.call_u256(
            QUOTER_V2,
            &hex(&abi::quoter_v2_quote_exact_input_single(
                USDC,
                barc(),
                u(9_980_000),
                3000,
            )),
            u(V3_3000_OUT),
        );
        let key = abi::PoolKey {
            currency0: Address::ZERO,
            currency1: barc(),
            fee: 2500,
            tick_spacing: 25,
            hooks: Address::ZERO,
        };
        h.call_u256(
            V4_QUOTER,
            &hex(&abi::v4_quoter_quote_exact_input_single(
                &key,
                true,
                usdc6_to_native18(u(9_980_000)),
            )),
            u(V4_OUT),
        );
        h.call_u256(MULTI_ROUTER, "0d9a9972", u(20_000));
    });
    // The other operation's tollFor rule now answers 20_000; buy-b re-quotes at 10 USDC (fee 20_000).
    let r = route_buy(
        WALLET,
        &buy_body("buy-b", "10", json!({"allow_worse_venue": true})),
    );
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    let a = read_json(ops::read_operation(WALLET, "buy-a", Network::Stage));
    assert_eq!(a["status"], "failed");
    assert_eq!(a["error"]["code"], "cancelled");
    assert_eq!(a["next_action"], "retry");
}

#[test]
fn reverted_swap_is_retried_with_a_superseded_attempt() {
    fake_host::install(host_for_barc_buy());
    let body = buy_body("buy-r", "25", json!({"allow_worse_venue": true}));
    fake_host::with(|h| {
        h.reply_chain(
            "eth_call",
            Some(USDC),
            "dd62ed3e",
            Ok(serde_json::to_string(&format!("0x{:064x}", GROSS)).unwrap()),
        );
    });
    // Allowance is sufficient: the first stage is already the swap.
    fake_host::with(|h| {
        let _ = h.chain("eth_call", &json!([{ "to": addr_hex(USDC), "data": abi::hex0x(&abi::erc20_allowance(wallet_address(), MULTI_ROUTER)) }, "latest"]).to_string());
    });
    assert_eq!(route_buy(WALLET, &body), DispatchResponse::Write);
    assert_eq!(record("buy-r")["step"], "swap");
    fake_host::with(|h| {
        h.set_outbox("ob-1", "reverted", Some("0xc3"), Some(&json!({"outcome": "reverted", "tx_hash": "0xc3", "block_number": 5, "revert_reason": "Too little received"})));
    });
    let doc = read_json(ops::read_operation(WALLET, "buy-r", Network::Stage));
    assert_eq!(doc["status"], "failed");
    assert_eq!(doc["error"]["code"], "reverted");
    assert_eq!(doc["error"]["message"], "Too little received");
    assert_eq!(doc["next_action"], "retry");
    let r = route_buy(WALLET, &body);
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    let rec = record("buy-r");
    assert_eq!(rec["txs"].as_array().unwrap().len(), 2);
    assert_eq!(rec["txs"][0]["superseded"], true);
    assert_eq!(rec["txs"][0]["outcome"], "reverted");
    assert_eq!(rec["txs"][1]["superseded"], false);
    assert_eq!(rec["status"], "staged");
    assert_eq!(
        rec["confirm_path"],
        "/bloom/wallets/main/chains/arc/outbox/pending/ob-2/confirm"
    );
}

#[test]
fn unknown_outbox_entries_never_regress_or_restage() {
    fake_host::install(host_for_barc_buy());
    let body = buy_body("buy-u", "25", json!({"allow_worse_venue": true}));
    assert_eq!(route_buy(WALLET, &body), DispatchResponse::Write);
    fake_host::with(|h| {
        h.remove_outbox("ob-1");
    });
    let doc = read_json(ops::read_operation(WALLET, "buy-u", Network::Stage));
    assert_eq!(doc["status"], "unknown");
    assert_eq!(doc["next_action"], "inspect");
    assert_eq!(doc["error"], Value::Null);
    assert_eq!(route_buy(WALLET, &body), DispatchResponse::Write);
    fake_host::with(|h| assert_eq!(h.staged.len(), 1, "nothing is re-staged while unknown"));
}

#[test]
fn buy_description_lists_recent_operations_and_the_gate() {
    fake_host::install(host_for_barc_buy());
    assert_eq!(
        route_buy(
            WALLET,
            &buy_body("buy-d", "25", json!({"allow_worse_venue": true}))
        ),
        DispatchResponse::Write
    );
    let doc = read_json(buy_description(WALLET));
    assert_eq!(doc["writes_enabled"], true);
    assert_eq!(doc["limits"]["max_op_usdc"], "250");
    assert_eq!(doc["recent"][0]["id"], "buy-d");
    assert_eq!(doc["recent"][0]["status"], "staged");
    assert_eq!(ops::list_ids(WALLET).unwrap(), vec!["buy-d".to_string()]);
    assert_eq!(ops::list_wallets().unwrap(), vec![WALLET.to_string()]);
}

// ---- pad token buy ----

#[test]
fn pad_token_buy_goes_straight_to_swap_router02_without_a_fee() {
    let mut host = FakeHost::new(NOW);
    host.set_setting(policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE);
    host.seed_vfs(
        &format!("wallets/{WALLET}/address"),
        addr_hex(wallet_address()).as_bytes(),
    );
    host.reply_http(CALENDAR_URL, 200, &calendar_detail());
    let pool: Address = "0x95bd2ec82e4442903ffe8635f01ac0066812a1b0"
        .parse()
        .unwrap();
    host.call_u256(
        QUOTER_V2,
        &hex(&abi::quoter_v2_quote_exact_input_single(
            USDC,
            calendar(),
            u(GROSS),
            10_000,
        )),
        u(V3_500_OUT),
    );
    host.call_u256(QUOTER_V2, "c6a5026a", u(1_300_000_000_000_000_000));
    let mut info = vec![0u8; 96];
    info[44..64].copy_from_slice(pool.as_slice());
    host.call_bytes(PAD, "e4860339", &info);
    host.balance(wallet_address(), u(100_000_000_000_000_000_000));
    host.call_u256(
        USDC,
        &hex(&abi::erc20_balance_of(wallet_address())),
        u(100_000_000),
    );
    host.call_u256(USDC, "dd62ed3e", u(GROSS)); // already approved to SwapRouter02
    host.call_u256(
        calendar(),
        &hex(&abi::erc20_balance_of(wallet_address())),
        u(7),
    );
    host.call_u256(SWAP_ROUTER02, "04e45aaf", u(V3_500_OUT));
    fake_host::install(host);
    let body = serde_json::to_vec(
        &json!({ "operationId": "buy-cal", "token": addr_hex(calendar()), "amount_usdc": "25" }),
    )
    .unwrap();
    let r = route_buy(WALLET, &body);
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    let floor = fee::protect(u(V3_500_OUT), 500).unwrap();
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 1);
        assert_eq!(h.staged[0].to, addr_hex(SWAP_ROUTER02));
        assert_eq!(
            h.staged[0].data_hex,
            abi::hex0x(&abi::swap_router02_exact_input_single(
                USDC,
                calendar(),
                10_000,
                wallet_address(),
                u(GROSS),
                floor
            )),
            "no fee: amountIn == gross, recipient == wallet, 1% tier"
        );
        assert!(
            h.eth_calls_to(MULTI_ROUTER).is_empty(),
            "pad tokens never touch the toll router"
        );
    });
    let rec = record("buy-cal");
    assert_eq!(rec["plan"]["interface_fee_raw"], "0");
    assert_eq!(rec["plan"]["spender"], addr_hex(SWAP_ROUTER02));
    assert_eq!(rec["txs"][0]["balance_before_raw"], "7");
}

// ---- sell ----

fn host_for_barc_sell(balance: u128) -> FakeHost {
    let mut host = FakeHost::new(NOW);
    host.set_setting(policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE);
    host.seed_vfs(
        &format!("wallets/{WALLET}/address"),
        addr_hex(wallet_address()).as_bytes(),
    );
    host.reply_http(BARC_URL, 200, &barc_detail());
    host.call_u256(
        QUOTER_V2,
        &hex(&abi::quoter_v2_quote_exact_input_single(
            barc(),
            USDC,
            u(balance),
            500,
        )),
        u(29_500_000),
    );
    host.call_u256(QUOTER_V2, "c6a5026a", u(29_600));
    host.call_u256(V4_QUOTER, "aa9d21cb", U256::ZERO); // V4 cannot fill
    host.call_bytes(PAD, "e4860339", &[0u8; 96]);
    host.balance(wallet_address(), u(1_000_000_000_000_000_000));
    host.call_u256(
        barc(),
        &hex(&abi::erc20_balance_of(wallet_address())),
        u(balance),
    );
    host.call_u256(barc(), "dd62ed3e", U256::ZERO);
    host.call_u256(barc(), "095ea7b3", u(1));
    host
}

#[test]
fn sell_all_freezes_the_balance_and_approves_the_token_exactly() {
    let balance = 1_000_000_000_000_000_000_000u128;
    fake_host::install(host_for_barc_sell(balance));
    let body = serde_json::to_vec(
        &json!({ "operationId": "sell-all", "token": addr_hex(barc()), "amount": "all" }),
    )
    .unwrap();
    let r = route_sell(WALLET, &body);
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    fake_host::with(|h| {
        assert_eq!(h.staged[0].to, addr_hex(barc()));
        assert_eq!(
            h.staged[0].data_hex,
            abi::hex0x(&abi::erc20_approve(MULTI_ROUTER, u(balance)))
        );
        assert!(
            h.eth_calls_to(MULTI_ROUTER).is_empty(),
            "no tollFor cross-check on a sell"
        );
    });
    let rec = record("sell-all");
    assert_eq!(rec["kind"], "sell");
    assert_eq!(rec["plan"]["amount_in_raw"], balance.to_string());
    assert_eq!(rec["plan"]["interface_fee_raw"], "0");
    assert_eq!(rec["plan"]["router_call"], "swapWithToll");
    assert_eq!(rec["request"]["amount"], "all");
}

#[test]
fn sell_is_capped_by_the_quoted_usdc_output() {
    let balance = 1_000_000_000_000_000_000_000u128;
    let mut host = host_for_barc_sell(balance);
    host.reply_chain(
        "eth_call",
        Some(QUOTER_V2),
        &hex(&abi::quoter_v2_quote_exact_input_single(
            barc(),
            USDC,
            u(balance),
            500,
        )),
        Ok(serde_json::to_string(&format!("0x{:064x}", 250_000_001u64)).unwrap()),
    );
    fake_host::install(host);
    fake_host::with(|h| {
        let _ = h.chain("eth_call", &json!([{ "to": addr_hex(QUOTER_V2), "data": abi::hex0x(&abi::quoter_v2_quote_exact_input_single(barc(), USDC, u(balance), 500)) }, "latest"]).to_string());
    });
    let body = serde_json::to_vec(
        &json!({ "operationId": "sell-big", "token": addr_hex(barc()), "amount": "1000" }),
    )
    .unwrap();
    let r = route_sell(WALLET, &body);
    assert_eq!(code(&r), -3, "{}", message(&r));
    assert!(message(&r).contains("cap-exceeded"));
    fake_host::with(|h| assert!(h.staged.is_empty()));
}

#[test]
fn sell_amount_uses_the_token_decimals_and_rejects_dust() {
    fake_host::install(host_for_barc_sell(1_000_000_000_000_000_000_000));
    let body = serde_json::to_vec(&json!({ "operationId": "sell-x", "token": addr_hex(barc()), "amount": "0.0000000000000000001" })).unwrap();
    assert_eq!(
        code(&route_sell(WALLET, &body)),
        -3,
        "19 fractional digits exceed the grammar"
    );
    let body = serde_json::to_vec(
        &json!({ "operationId": "sell-x", "token": addr_hex(barc()), "amount": "5000" }),
    )
    .unwrap();
    let r = route_sell(WALLET, &body);
    assert_eq!(code(&r), -3, "{}", message(&r));
    assert!(
        message(&r).contains("insufficient-funds") || message(&r).contains("quote-unavailable"),
        "{}",
        message(&r)
    );
}

// ---- launch ----

fn host_for_launch() -> FakeHost {
    let mut host = FakeHost::new(NOW);
    host.set_setting(policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE);
    host.seed_vfs(
        &format!("wallets/{WALLET}/address"),
        addr_hex(wallet_address()).as_bytes(),
    );
    host.balance(wallet_address(), u(100_000_000_000_000_000_000));
    host.call_u256(
        USDC,
        &hex(&abi::erc20_balance_of(wallet_address())),
        u(100_000_000),
    );
    host.call_u256(USDC, "dd62ed3e", U256::ZERO);
    host.call_u256(USDC, "095ea7b3", u(1));
    host.call_bytes(PAD, "b186badf", &[0u8; 32]); // createToken pre-flight
    host
}

fn launch_body(id: &str, dev_buy: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "operationId": id, "name": "  Moss Coin  ", "symbol": " moss ",
        "meta": { "imageURI": "ipfs://bafkreiccijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscii", "website": "https://moss.example", "twitter": "x.com/moss", "telegram": "t.me/moss" },
        "dev_buy_usdc": dev_buy
    }))
    .unwrap()
}

#[test]
fn launch_without_dev_buy_stages_create_token_with_a_frozen_salt() {
    fake_host::install(host_for_launch());
    let r = route_launch(WALLET, &launch_body("launch-moss", "0"));
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    let meta = abi::TokenMeta {
        image_uri: "ipfs://bafkreiccijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscijbeeqscii".into(),
        website: "https://moss.example".into(),
        twitter: "x.com/moss".into(),
        telegram: "t.me/moss".into(),
    };
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 1);
        assert_eq!(h.staged[0].to, addr_hex(PAD));
        assert_eq!(h.staged[0].value_wei, "0");
        assert_eq!(
            h.staged[0].data_hex,
            abi::hex0x(&abi::pad_create_token(
                "Moss Coin",
                "MOSS",
                &meta,
                [0x11u8; 32],
                U256::ZERO
            ))
        );
        assert!(
            h.eth_calls_to(USDC)
                .iter()
                .all(|c| !c.data().unwrap().starts_with("0x095ea7b3")),
            "no approve without a dev buy"
        );
    });
    let rec = record("launch-moss");
    assert_eq!(rec["kind"], "launch");
    assert_eq!(rec["step"], "create");
    assert_eq!(rec["plan"]["salt"], format!("0x{}", "11".repeat(32)));
    assert_eq!(rec["plan"]["launch_symbol"], "MOSS");
    // Re-POST while pending: no-op; after a cancel the SAME salt is reused.
    assert_eq!(
        route_launch(WALLET, &launch_body("launch-moss", "0")),
        DispatchResponse::Write
    );
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 1);
        h.random_fill = 0x22;
        h.set_outbox("ob-1", "cancelled", None, None);
    });
    assert_eq!(
        route_launch(WALLET, &launch_body("launch-moss", "0")),
        DispatchResponse::Write
    );
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 2);
        assert_eq!(
            h.staged[1].data_hex, h.staged[0].data_hex,
            "the salt is frozen in the record"
        );
    });
    assert_eq!(record("launch-moss")["txs"][0]["superseded"], true);
    // A different launch under the same id is refused; a bad body never reaches the store.
    assert_eq!(
        code(&route_launch(WALLET, &launch_body("launch-moss", "5"))),
        -3
    );
    assert_eq!(code(&route_launch(WALLET, b"{}")), -3);
}

#[test]
fn launch_with_dev_buy_approves_the_pad_first_and_completes_from_the_index() {
    fake_host::install(host_for_launch());
    let r = route_launch(WALLET, &launch_body("launch-dev", "5"));
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    fake_host::with(|h| {
        assert_eq!(h.staged[0].to, addr_hex(USDC));
        assert_eq!(
            h.staged[0].data_hex,
            abi::hex0x(&abi::erc20_approve(PAD, u(5_000_000))),
            "exact dev-buy approval to the pad"
        );
        h.set_outbox(
            "ob-1",
            "success",
            Some("0xd1"),
            Some(&json!({"outcome": "success", "tx_hash": "0xd1", "block_number": 20188700})),
        );
        h.reply_chain(
            "eth_call",
            Some(USDC),
            "dd62ed3e",
            Ok(serde_json::to_string(&format!("0x{:064x}", 5_000_000)).unwrap()),
        );
    });
    assert_eq!(record("launch-dev")["step"], "approve");
    let doc = read_json(ops::read_operation(WALLET, "launch-dev", Network::Stage));
    assert_eq!(doc["status"], "confirmed");
    assert_eq!(doc["next_action"], "repost");
    let r = route_launch(WALLET, &launch_body("launch-dev", "5"));
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    fake_host::with(|h| {
        assert_eq!(h.staged.len(), 2);
        assert_eq!(h.staged[1].to, addr_hex(PAD));
        assert!(
            h.staged[1]
                .data_hex
                .contains(&format!("{:064x}", 5_000_000))
        );
        h.set_outbox(
            "ob-2",
            "success",
            Some("0xd2"),
            Some(&json!({"outcome": "success", "tx_hash": "0xd2", "block_number": 20188730})),
        );
        let url = ApiRoute::LaunchesByCreator(wallet_address()).url(Network::Stage);
        h.reply_http(
            &url,
            200,
            &json!({ "tokens": [], "total": 0, "scope": "ours" }),
        );
        h.reply_http(&url, 200, &markets());
    });
    // First read: index has not listed it yet -> stays confirmed with a note.
    let doc = read_json(ops::read_operation(WALLET, "launch-dev", Network::Stage));
    assert_eq!(doc["status"], "confirmed");
    assert_eq!(doc["next_action"], "wait");
    assert!(doc["note"].as_str().unwrap().contains("not listed"));
    // Second read: the creator index carries a row at the receipt block.
    let doc = read_json(ops::read_operation(WALLET, "launch-dev", Network::Stage));
    assert_eq!(doc["status"], "completed");
    assert_eq!(doc["result"]["method"], "creator-index-block");
    assert_eq!(
        doc["result"]["token"],
        "0x2005cd22ea3c1acfaa9e01d3a178f356bb03c81c"
    );
    assert_eq!(
        doc["result"]["pool"],
        "0x95bd2ec82e4442903ffe8635f01ac0066812a1b0"
    );
    assert_eq!(doc["result"]["created_block"], 20188730);
    fake_host::with(|h| {
        let url = ApiRoute::LaunchesByCreator(wallet_address()).url(Network::Stage);
        assert!(url.contains("creator=0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(
            h.http_calls
                .iter()
                .all(|c| c.url.starts_with("https://stage.tollylabs.com/api/")),
            "only the stage API is reached"
        );
    });
}

#[test]
fn launch_input_limits() {
    fake_host::install(host_for_launch());
    let too_big = serde_json::to_vec(&json!({ "operationId": "l", "name": "A", "symbol": "B", "meta": { "imageURI": "ipfs://x" }, "dev_buy_usdc": "140.5" })).unwrap();
    assert_eq!(code(&route_launch(WALLET, &too_big)), -3);
    let no_logo = serde_json::to_vec(
        &json!({ "operationId": "l", "name": "A", "symbol": "B", "meta": { "imageURI": " " } }),
    )
    .unwrap();
    assert_eq!(code(&route_launch(WALLET, &no_logo)), -3);
    let long_meta = serde_json::to_vec(&json!({ "operationId": "l", "name": "A", "symbol": "B", "meta": { "imageURI": "ipfs://x", "website": "w".repeat(513) } })).unwrap();
    assert_eq!(code(&route_launch(WALLET, &long_meta)), -3);
    fake_host::with(|h| assert!(h.state_keys().is_empty() && h.staged.is_empty()));
}

// ---- positions ----

#[test]
fn positions_report_both_usdc_views_and_touched_tokens() {
    fake_host::install(host_for_barc_buy());
    assert_eq!(
        route_buy(
            WALLET,
            &buy_body("buy-p", "25", json!({"allow_worse_venue": true}))
        ),
        DispatchResponse::Write
    );
    fake_host::with(|h| {
        h.replace_chain(
            "eth_call",
            Some(barc()),
            &hex(&abi::erc20_balance_of(wallet_address())),
            Ok(
                serde_json::to_string(&format!("0x{:064x}", 5_000_000_000_000_000_000u128))
                    .unwrap(),
            ),
        );
    });
    let doc = read_json(positions_document(WALLET));
    assert_eq!(doc["schema"], "tolly.positions.v1");
    assert_eq!(doc["usdc"]["native_raw"], "100000000000000000000");
    assert_eq!(doc["usdc"]["erc20_raw"], "100000000");
    assert_eq!(doc["usdc"]["erc20_matches_native"], true);
    assert_eq!(doc["tokens"][0]["token"], addr_hex(barc()));
    assert_eq!(doc["tokens"][0]["symbol"], "BARC");
    assert_eq!(doc["tokens"][0]["balance_human"], "5");
    assert_eq!(doc["bounds"]["max_tokens"], policy::POSITIONS_MAX_TOKENS);
    fake_host::with(|h| {
        let symbol_calls = h
            .chain_calls
            .iter()
            .filter(|c| c.data().is_some_and(|d| d.starts_with("0x95d89b41")))
            .count();
        assert_eq!(
            symbol_calls, 0,
            "symbols come from the records, not the chain (M11)"
        );
    });
    assert_eq!(code(&positions_document("team/alice")), -3);
}

// ---- secret boundary ----

#[test]
fn no_route_file_touches_the_secret_namespace() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("files");
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert_eq!(files.len(), 21, "expected route count");
    for file in files {
        let source = std::fs::read_to_string(&file).unwrap();
        for forbidden in [
            "\"secrets\"",
            "secret_key",
            "load_secret",
            "store_get_secret",
            "tx_confirm",
            "sign_payload",
            "derive_key",
            "vfs_write",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} references {forbidden}",
                file.display()
            );
        }
        assert!(
            source.matches(".caps(&[").count() <= 1,
            "{} declares caps at most once",
            file.display()
        );
    }
}

#[test]
fn records_and_errors_never_carry_host_internals() {
    let mut host = host_for_barc_buy();
    host.call_error(
        QUOTER_V2,
        "c6a5026a",
        "eth_call: error sending request for url (https://arc.example/rpc/KEY-SECRET-42): timeout",
    );
    host.reply_chain(
        "eth_call",
        Some(QUOTER_V2),
        &hex(&abi::quoter_v2_quote_exact_input_single(
            USDC,
            barc(),
            u(NET),
            500,
        )),
        Err(SdkError::Message(
            "https://arc.example/rpc/KEY-SECRET-42 down".into(),
        )),
    );
    host.reply_chain(
        "eth_call",
        Some(QUOTER_V2),
        &hex(&abi::quoter_v2_quote_exact_input_single(
            USDC,
            barc(),
            u(NET),
            3000,
        )),
        Err(SdkError::Message(
            "https://arc.example/rpc/KEY-SECRET-42 down".into(),
        )),
    );
    host.reply_chain(
        "eth_call",
        Some(V4_QUOTER),
        "aa9d21cb",
        Err(SdkError::Message(
            "https://arc.example/rpc/KEY-SECRET-42 down".into(),
        )),
    );
    fake_host::install(host);
    // Exhaust the first (good) replies so the failing ones are served.
    let detail = token_detail(Network::Stage, barc()).unwrap();
    let _ = quote::quote(&detail, Side::Buy, u(GROSS), 500);
    let r = route_buy(
        WALLET,
        &buy_body("buy-s", "25", json!({"allow_worse_venue": true})),
    );
    assert_eq!(code(&r), -4, "{}", message(&r));
    assert!(!message(&r).contains("KEY-SECRET"), "{}", message(&r));
    let rec = record("buy-s").to_string();
    assert!(!rec.contains("KEY-SECRET"));
    assert!(!rec.contains("arc.example"));
    for key in ["private", "secret", "mnemonic", "seed"] {
        assert!(
            !rec.contains(&format!("\"{key}")),
            "record carries no {key}-like field"
        );
    }
}

#[test]
fn operation_read_validates_and_reports_not_found() {
    fake_host::install(host_for_barc_buy());
    assert_eq!(
        code(&ops::read_operation(WALLET, "missing", Network::Stage)),
        -1
    );
    assert_eq!(
        code(&ops::read_operation(WALLET, "Bad Id", Network::Stage)),
        -3
    );
    assert_eq!(
        code(&ops::read_operation("team/alice", "x", Network::Stage)),
        -3
    );
    assert_eq!(ops::list_ids(WALLET).unwrap(), Vec::<String>::new());
    // Kind names used in records are stable.
    assert_eq!(serde_json::to_value(Kind::Launch).unwrap(), "launch");
    assert_eq!(
        serde_json::to_value(Status::Broadcast).unwrap(),
        "broadcast"
    );
    assert_eq!(serde_json::to_value(Step::Create).unwrap(), "create");
    assert_eq!(
        serde_json::to_value(NextAction::ConfirmInBloom).unwrap(),
        "confirm_in_bloom"
    );
}

#[test]
fn every_staged_transaction_is_zero_value_on_arc_with_no_fee_overrides() {
    fake_host::install(host_for_barc_buy());
    assert_eq!(
        route_buy(
            WALLET,
            &buy_body("buy-z", "25", json!({"allow_worse_venue": true}))
        ),
        DispatchResponse::Write
    );
    fake_host::install(host_for_launch());
    assert_eq!(
        route_launch(WALLET, &launch_body("launch-z", "0")),
        DispatchResponse::Write
    );
    fake_host::with(|h| {
        for tx in &h.staged {
            assert_eq!(tx.value_wei, "0");
            assert_eq!(tx.chain, "arc");
            assert!(
                tx.data_hex.starts_with("0x") && tx.data_hex == tx.data_hex.to_ascii_lowercase()
            );
            assert!(
                tx.max_fee_per_gas.is_none()
                    && tx.max_priority_fee_per_gas.is_none()
                    && tx.nonce.is_none()
            );
        }
    });
}

#[test]
fn v2_venue_buys_route_through_swap_with_toll_v2_with_a_non_zero_floor() {
    let token: Address = "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap();
    let pair: Address = "0x2222222222222222222222222222222222222222"
        .parse()
        .unwrap();
    let factory: Address = "0x3333333333333333333333333333333333333333"
        .parse()
        .unwrap();
    let detail = json!({
        "token": { "address": addr_hex(token), "symbol": "WARP", "name": "Warp", "decimals": 18, "external": true, "supply": 1e9, "dex": "v2", "pool": addr_hex(pair) },
        "pools": [ { "pool": addr_hex(pair), "kind": "v2", "fee": null, "feeBps": 30, "factory": addr_hex(factory), "router": null, "supportsFot": false, "tradeable": true, "liquidity": 18187.0 } ],
        "quoteDecimals": 6
    });
    let mut host = FakeHost::new(NOW);
    host.set_setting(policy::WRITES_SETTING, policy::WRITES_ENABLED_VALUE);
    host.seed_vfs(
        &format!("wallets/{WALLET}/address"),
        addr_hex(wallet_address()).as_bytes(),
    );
    host.reply_http(&ApiRoute::Token(token).url(Network::Stage), 200, &detail);
    // pair.token0() == USDC; reserves (USDC 18187e6, WARP 1.23456789e24)
    let mut token0 = [0u8; 32];
    token0[12..].copy_from_slice(USDC.as_slice());
    host.call_bytes(pair, "0dfe1681", &token0);
    let mut reserves = Vec::new();
    reserves.extend(word_ret(u(18_187_000_000)));
    reserves.extend(word_ret(u(1_234_567_890_000_000_000_000_000)));
    reserves.extend(word_ret(u(1_789_000_000)));
    host.call_bytes(pair, "0902f1ac", &reserves);
    host.call_u256(MULTI_ROUTER, "0d9a9972", u(50_000));
    host.call_bytes(PAD, "e4860339", &[0u8; 96]);
    host.balance(wallet_address(), u(100_000_000_000_000_000_000));
    host.call_u256(
        USDC,
        &hex(&abi::erc20_balance_of(wallet_address())),
        u(100_000_000),
    );
    host.call_u256(USDC, "dd62ed3e", u(GROSS));
    host.call_u256(
        token,
        &hex(&abi::erc20_balance_of(wallet_address())),
        U256::ZERO,
    );
    host.call_u256(MULTI_ROUTER, "2c29c054", u(1));
    fake_host::install(host);
    let body = serde_json::to_vec(
        &json!({ "operationId": "buy-warp", "token": addr_hex(token), "amount_usdc": "25" }),
    )
    .unwrap();
    let r = route_buy(WALLET, &body);
    assert_eq!(r, DispatchResponse::Write, "{}", message(&r));
    let out = quote::v2_amount_out(
        u(NET),
        u(18_187_000_000),
        u(1_234_567_890_000_000_000_000_000),
        30,
    );
    let floor = fee::protect(out, 500).unwrap();
    assert!(floor > U256::ZERO);
    fake_host::with(|h| {
        assert_eq!(h.staged[0].to, addr_hex(MULTI_ROUTER));
        assert_eq!(
            h.staged[0].data_hex,
            abi::hex0x(&abi::multi_router_swap_with_toll_v2(
                USDC,
                token,
                factory,
                30,
                u(GROSS),
                floor
            )),
            "factory from the API, gross amountIn, non-zero floor"
        );
    });
    assert_eq!(record("buy-warp")["plan"]["router_call"], "swapWithTollV2");
}
