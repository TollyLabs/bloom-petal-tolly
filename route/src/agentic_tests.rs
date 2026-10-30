//! Route-flow tests for the agentic routes against the recording fake host.
//!
//! `agent-*.json` fixtures are captures of the public free endpoints of
//! `https://agent.tollylabs.com` (2026-10-29). Tolly OS is not deployed on
//! the gateway yet (its endpoints answer a plain 404), so the live ACP and
//! spending-rule bodies and the board job with deliveries are
//! `synthetic-*.json`: shaped exactly as the gateway source answers them
//! (tolly-x402 `tolly-os/acp-integration` `apps/gateway/src/acp.ts`,
//! `tolly-os/spend-policy-integration` `apps/gateway/src/nft-agents.ts`,
//! `main` `apps/gateway/src/jobs.ts`), with made-up addresses.

use petal::DispatchResponse;
use serde_json::{Value, json};

use crate::agentic::{self, AgentRoute};
use crate::fake_host::{self, FakeHost};

const NOW: u64 = 1_793_000_000_000;
const NFT: &str = "9c2dbad7-349d-47c3-a2a9-e4bec825aeaf";
const BOARD_JOB: &str = "jb_0123456789abcdef0123";

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_slice(&std::fs::read(&path).expect("fixture exists")).expect("fixture is JSON")
}

fn url(route: AgentRoute) -> String {
    route.url()
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
        DispatchResponse::Read(bytes) => serde_json::from_slice(&bytes).expect("JSON read"),
        other => panic!("expected a read, got {other:?}"),
    }
}

fn host() -> FakeHost {
    let mut host = FakeHost::new(NOW);
    host.reply_http(
        &url(AgentRoute::Stats),
        200,
        &fixture("agent-exchange-stats.json"),
    );
    host.reply_http(&url(AgentRoute::Agents), 200, &fixture("agent-agents.json"));
    host.reply_http(
        &url(AgentRoute::Agent("tolly-data".into())),
        200,
        &fixture("agent-agent-tolly-data.json"),
    );
    host.reply_http(
        &url(AgentRoute::Agent("nobody".into())),
        404,
        &fixture("agent-not-found-404.json"),
    );
    host.reply_http(
        &url(AgentRoute::Services),
        200,
        &fixture("agent-services.json"),
    );
    host.reply_http(
        &url(AgentRoute::Service("token.risk.v2".into())),
        200,
        &fixture("agent-service-token-risk-v2.json"),
    );
    host.reply_http(
        &url(AgentRoute::Service("nope.v9".into())),
        404,
        &fixture("agent-service-unknown-404.json"),
    );
    host.reply_http(&url(AgentRoute::Routed), 200, &fixture("agent-routed.json"));
    host.reply_http(
        &url(AgentRoute::RoutedOne("2207ee27d8c6d46e".into())),
        200,
        &fixture("agent-routed-one.json"),
    );
    host.reply_http(
        &url(AgentRoute::Jobs),
        200,
        &fixture("agent-jobs-open.json"),
    );
    host
}

/// Tolly OS as the live gateway serves it today: every endpoint a plain 404.
fn os_not_served(host: &mut FakeHost) {
    let not_found = fixture("agent-not-found-404.json");
    for route in [
        AgentRoute::Acp,
        AgentRoute::AcpJobs,
        AgentRoute::AcpJob("7".into()),
        AgentRoute::SpendPolicy,
        AgentRoute::AgentPolicy(NFT.into()),
    ] {
        host.reply_http(&url(route), 404, &not_found);
    }
}

fn os_live(host: &mut FakeHost) {
    host.reply_http(
        &url(AgentRoute::Acp),
        200,
        &fixture("synthetic-acp-live.json"),
    );
    host.reply_http(
        &url(AgentRoute::AcpJobs),
        200,
        &fixture("synthetic-acp-jobs.json"),
    );
    host.reply_http(
        &url(AgentRoute::AcpJob("7".into())),
        200,
        &fixture("synthetic-acp-job-7.json"),
    );
    host.reply_http(
        &url(AgentRoute::AcpJob("8".into())),
        404,
        &fixture("synthetic-acp-job-unknown-404.json"),
    );
    host.reply_http(
        &url(AgentRoute::SpendPolicy),
        200,
        &fixture("synthetic-spend-policy-live.json"),
    );
    host.reply_http(
        &url(AgentRoute::AgentPolicy(NFT.into())),
        200,
        &fixture("synthetic-nft-agent-policy.json"),
    );
    host.reply_http(
        &url(AgentRoute::AgentPolicy(
            "00000000-0000-0000-0000-000000000000".into(),
        )),
        404,
        &fixture("synthetic-nft-agent-unknown-404.json"),
    );
}

/// Every request this host saw was a GET to the agent exchange.
fn assert_only_agent_gets() {
    fake_host::with(|h| {
        assert!(!h.http_calls.is_empty());
        for call in &h.http_calls {
            assert_eq!(call.method, "GET");
            assert!(
                call.url.starts_with("https://agent.tollylabs.com/v1/"),
                "{}",
                call.url
            );
            assert!(call.body.is_empty());
            assert!(
                call.headers
                    .iter()
                    .all(|(k, _)| !k.eq_ignore_ascii_case("authorization")
                        && !k.to_ascii_lowercase().contains("payment")),
                "no session or payment header: {:?}",
                call.headers
            );
        }
        assert!(
            h.chain_calls.is_empty(),
            "agentic routes never read the chain"
        );
        assert!(h.staged.is_empty(), "agentic routes never stage");
        assert_eq!(h.store_writes(), 0, "agentic routes never write the store");
    });
}

// ---- overview ----

#[test]
fn overview_projects_stats_agents_and_the_paying_note() {
    fake_host::install(host());
    let doc = read_json(agentic::overview_read(NOW));
    assert_eq!(doc["schema"], "tolly.agentic.overview.v1");
    assert_eq!(doc["network"], "eip155:5042");
    assert_eq!(doc["period"], "30d");
    assert_eq!(doc["stats"][0]["id"], "revenue");
    assert_eq!(doc["stats"].as_array().unwrap().len(), 6);
    let agents = doc["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 4);
    assert_eq!(agents[0]["id"], "tolly-data");
    assert_eq!(agents[0]["profile"], "agentic/agents/tolly-data.json");
    assert_eq!(
        agents[0]["token"]["trade"],
        "tokens/0xbc43ce8dec648ea298c4275559b81d6261c90b67.json"
    );
    // Lazarus is an agent NFT: its spending-rules path comes from /r/agents/<uuid>.
    let lazarus = agents.iter().find(|a| a["id"] == "lazarus").unwrap();
    assert_eq!(
        lazarus["spending_rules"],
        format!("agentic/spending/{NFT}.json")
    );
    assert_eq!(agents[0]["spending_rules"], Value::Null);
    assert_eq!(doc["agents_error"], Value::Null);
    assert!(
        doc["paying"]["from_bloom"]
            .as_str()
            .unwrap()
            .starts_with("requests/ (Bloom v0.4.1")
    );
    assert_eq!(doc["paying"]["payee"], agentic::TOLLY_X402_PAYEE);
    assert_eq!(
        doc["paying"]["outside_bloom"]["mcp"],
        "npx -y @tollylabs/x402-mcp"
    );
    assert_eq!(doc["next"]["tolly_os"], "agentic/os.json");
    assert_only_agent_gets();
}

#[test]
fn overview_survives_a_directory_outage_but_not_a_stats_outage() {
    let mut h = host();
    h.forget_http(&url(AgentRoute::Agents));
    h.reply_http(&url(AgentRoute::Agents), 500, &json!({"error": "internal"}));
    fake_host::install(h);
    let doc = read_json(agentic::overview_read(NOW));
    assert_eq!(doc["agents"], json!([]));
    assert_eq!(doc["agents_error"], "HTTP 500");

    let mut h = host();
    h.forget_http(&url(AgentRoute::Stats));
    fake_host::install(h);
    let response = agentic::overview_read(NOW);
    assert_eq!(code(&response), -4);
    assert!(message(&response).contains("TOLLY agent exchange"));
}

// ---- services ----

#[test]
fn service_catalog_lists_prices_versions_and_mcp_tools() {
    fake_host::install(host());
    let doc = read_json(agentic::services_read(NOW));
    assert_eq!(doc["schema"], "tolly.agentic.services.v1");
    let services = doc["services"].as_array().unwrap();
    assert_eq!(services.len(), 23);
    assert_eq!(doc["count"], 23);
    let balances = &services[0];
    assert_eq!(balances["id"], "wallet.balances.v1");
    assert_eq!(balances["price"]["atomic"], "20000");
    assert_eq!(balances["price"]["usd"], "0.02");
    assert_eq!(balances["price"]["network"], "eip155:5042");
    assert_eq!(
        balances["price"]["asset"],
        "0x3600000000000000000000000000000000000000"
    );
    assert_eq!(
        balances["detail"],
        "agentic/services/wallet.balances.v1.json"
    );
    assert_eq!(balances["mcp_tool"], "tolly_wallet_balances_v1");
    // Older versions stay listed but are not `latest`.
    let by_id = |id: &str| services.iter().find(|s| s["id"] == id).unwrap().clone();
    assert_eq!(by_id("markets.search.v1")["latest"], false);
    assert_eq!(by_id("markets.search.v2")["latest"], true);
    assert_eq!(by_id("markets.search.v2")["price"]["usd"], "0.006");
    assert_eq!(by_id("token.intelligence.v3")["latest"], true);
    // No schema bloat in the list: schemas live in the detail.
    assert!(balances.get("input_schema").is_none());
    assert_only_agent_gets();
}

#[test]
fn service_detail_carries_price_schema_limits_and_how_to_buy() {
    fake_host::install(host());
    let doc = read_json(agentic::service_read("token.risk.v2", NOW));
    assert_eq!(doc["schema"], "tolly.agentic.service.v1");
    assert_eq!(doc["id"], "token.risk.v2");
    assert_eq!(doc["price"]["atomic"], "15000");
    assert_eq!(doc["price"]["usd"], "0.015");
    assert!(doc["input_schema"]["properties"].is_object());
    assert!(doc["output_schema"].is_object());
    assert!(doc["limits"]["max_input_bytes"].is_u64());
    assert!(doc["limits"]["quote_validity_seconds"].is_u64());
    assert!(!doc["limitations"].as_array().unwrap().is_empty());
    assert!(doc["example_input"].is_object());
    assert_eq!(doc["how_to_buy"]["mcp_tool"], "tolly_token_risk_v2");
    assert_eq!(
        doc["how_to_buy"]["invoke"],
        "https://agent.tollylabs.com/v1/services/token.risk.v2/invoke"
    );
    let from_bloom = &doc["how_to_buy"]["from_bloom"];
    assert_eq!(from_bloom["write_to"], "requests/new");
    assert!(from_bloom["request"].as_str().unwrap().starts_with(
        "POST https://agent.tollylabs.com/v1/services/token.risk.v2/invoke wallet=<your wallet> max_amount_usd=<the price>\ncontent-type: application/json"
    ));
    assert_only_agent_gets();
}

#[test]
fn service_detail_errors() {
    fake_host::install(host());
    let unknown = agentic::service_read("nope.v9", NOW);
    assert_eq!(code(&unknown), -1);
    assert!(message(&unknown).contains("unknown service"));
    // An invalid id never reaches the host.
    for bad in ["../usage", "Token.Risk", "a/b", "x?y=1"] {
        assert_eq!(code(&agentic::service_read(bad, NOW)), -3, "{bad}");
    }
    fake_host::with(|h| assert_eq!(h.http_calls.len(), 1));
}

#[test]
fn service_listing_names_one_file_per_service() {
    fake_host::install(host());
    let children = agentic::services_children().expect("listing");
    assert_eq!(children.len(), 23);
    assert_eq!(children[0], petal::file("wallet.balances.v1.json"));
}

// ---- routed ----

#[test]
fn routed_listing_projects_prices_and_profiles() {
    fake_host::install(host());
    let doc = read_json(agentic::routed_read(None, NOW));
    assert_eq!(doc["schema"], "tolly.agentic.routed.v1");
    assert_eq!(doc["count"], 91);
    assert_eq!(doc["search"], Value::Null);
    let first = &doc["services"][0];
    assert_eq!(first["route_id"], "2207ee27d8c6d46e");
    assert_eq!(first["all_in"]["atomic"], "6000");
    assert_eq!(first["all_in"]["usd"], "0.006");
    assert_eq!(first["seller_price"]["usd"], "0.005");
    assert_eq!(first["fee"]["usd"], "0.001");
    assert_eq!(first["agent_profile"], "agentic/agents/apex.json");
    assert_eq!(first["detail"], "agentic/routed/2207ee27d8c6d46e.json");
    assert_eq!(doc["fee"]["percent"], 10);
    assert_only_agent_gets();
}

#[test]
fn routed_search_filters_locally_and_validates_the_term() {
    fake_host::install(host());
    let doc = read_json(agentic::routed_read(Some("APEX"), NOW));
    assert_eq!(doc["search"], "apex");
    let count = doc["count"].as_u64().unwrap();
    assert!((1..91).contains(&count), "{count}");
    for row in doc["services"].as_array().unwrap() {
        let hay = format!(
            "{} {} {} {}",
            row["title"], row["description"], row["seller"], row["agent_id"]
        )
        .to_lowercase();
        assert!(hay.contains("apex"), "{hay}");
    }
    let none = read_json(agentic::routed_read(Some("zz-no-such-seller"), NOW));
    assert_eq!(none["count"], 0);
    // One upstream URL for every search: the term never reaches the gateway.
    fake_host::with(|h| {
        assert!(
            h.http_calls
                .iter()
                .all(|c| c.url == url(AgentRoute::Routed))
        );
    });
    assert_eq!(code(&agentic::routed_read(Some("a/b"), NOW)), -3);
    assert_eq!(code(&agentic::routed_read(Some("   "), NOW)), -3);
}

#[test]
fn routed_one_and_listing() {
    fake_host::install(host());
    let doc = read_json(agentic::routed_one_read("2207ee27d8c6d46e", NOW));
    assert_eq!(doc["schema"], "tolly.agentic.routed-listing.v1");
    assert_eq!(doc["seller"], "APEX");
    assert_eq!(
        doc["resource"],
        "https://apexfaucet.xyz/api/x402/agent-trust"
    );
    assert_eq!(code(&agentic::routed_one_read("XYZ", NOW)), -3);
    let children = agentic::routed_children().unwrap();
    assert_eq!(children[0], petal::dir("search"));
    assert_eq!(children.len(), 92);
}

// ---- agents ----

#[test]
fn agent_directory_and_profile() {
    fake_host::install(host());
    let doc = read_json(agentic::agents_read(NOW));
    assert_eq!(doc["schema"], "tolly.agentic.agents.v1");
    assert_eq!(doc["count"], 4);
    assert!(
        doc["evidence"]
            .as_str()
            .unwrap()
            .starts_with("Observed by TOLLY")
    );

    let profile = read_json(agentic::agent_read("tolly-data", NOW));
    assert_eq!(profile["schema"], "tolly.agentic.agent.v1");
    assert_eq!(profile["is_tolly"], true);
    assert_eq!(profile["wallet"], agentic::TOLLY_X402_PAYEE);
    assert_eq!(profile["services"].as_array().unwrap().len(), 19);
    assert_eq!(profile["services"][0]["id"], "markets.search.v2");
    assert_eq!(
        profile["services"][0]["catalog"],
        "agentic/services/markets.search.v2.json"
    );
    assert_eq!(profile["routed"], "agentic/routed/search/tolly-data.json");
    assert_eq!(profile["mcp_endpoint"], "npx -y @tollylabs/x402-mcp");

    assert_eq!(code(&agentic::agent_read("nobody", NOW)), -1);
    assert_eq!(code(&agentic::agent_read("Bad/Id", NOW)), -3);
    let children = agentic::agents_children().unwrap();
    assert_eq!(children.len(), 4);
    assert_eq!(children[0], petal::file("tolly-data.json"));
    assert_only_agent_gets();
}

// ---- job board ----

#[test]
fn empty_job_board_is_a_readable_empty_list() {
    fake_host::install(host());
    let doc = read_json(agentic::jobs_read(NOW));
    assert_eq!(doc["schema"], "tolly.agentic.jobs.v1");
    assert_eq!(doc["count"], 0);
    assert_eq!(doc["jobs"], json!([]));
    assert_eq!(doc["max_budget"]["usd"], "1");
    assert_eq!(doc["fee"]["minimumAtomic"], "1000");
    assert_eq!(agentic::jobs_children().unwrap(), Vec::new());
    assert_only_agent_gets();
}

#[test]
fn job_board_posts_are_cleaned_untrusted_text() {
    let mut h = host();
    h.forget_http(&url(AgentRoute::Jobs));
    h.reply_http(
        &url(AgentRoute::Jobs),
        200,
        &fixture("synthetic-jobs-open.json"),
    );
    h.reply_http(
        &url(AgentRoute::Job(BOARD_JOB.into())),
        200,
        &fixture("synthetic-job-detail.json"),
    );
    h.reply_http(
        &url(AgentRoute::Job("jb_ffffffffffffffffffff".into())),
        404,
        &fixture("agent-not-found-404.json"),
    );
    fake_host::install(h);
    let doc = read_json(agentic::jobs_read(NOW));
    let job = &doc["jobs"][0];
    assert_eq!(job["id"], BOARD_JOB);
    assert_eq!(job["title"], "Find BARC whales");
    let brief = job["brief"].as_str().unwrap();
    assert!(
        !brief.contains('\u{202e}') && !brief.contains('\u{0007}'),
        "{brief}"
    );
    assert_eq!(job["budget"]["usd"], "0.5");
    assert_eq!(job["detail"], format!("agentic/jobs/{BOARD_JOB}.json"));
    assert!(doc["note"].as_str().unwrap().contains("untrusted"));

    let detail = read_json(agentic::job_read(BOARD_JOB, NOW));
    assert_eq!(detail["schema"], "tolly.agentic.job.v1");
    let delivery = &detail["delivery_list"][0];
    assert_eq!(delivery["all_in"]["usd"], "0.44");
    assert_eq!(delivery["fulfiller"]["id"], "lazarus");
    assert!(delivery.get("payoutTo").is_none());
    assert_eq!(code(&agentic::job_read("jb_ffffffffffffffffffff", NOW)), -1);
    // Purchase-job ids (token-gated) and anything else are refused offline.
    assert_eq!(code(&agentic::job_read("mine", NOW)), -3);
    assert_eq!(code(&agentic::job_read("0f3b2c1a-0000", NOW)), -3);
    assert_only_agent_gets();
}

// ---- Tolly OS ----

#[test]
fn os_status_when_the_gateway_does_not_serve_tolly_os_yet() {
    let mut h = host();
    os_not_served(&mut h);
    fake_host::install(h);
    let doc = read_json(agentic::os_read(NOW));
    assert_eq!(doc["schema"], "tolly.agentic.os.v1");
    assert_eq!(doc["acp"]["enabled"], false);
    assert_eq!(doc["acp"]["state"], "not on this gateway yet");
    assert_eq!(doc["spend_policy"]["enabled"], false);
    assert_eq!(doc["spend_policy"]["state"], "not on this gateway yet");
    // ACP list, job and spending rules read as the same off state, never an error.
    let jobs = read_json(agentic::acp_jobs_read(NOW));
    assert_eq!(jobs["enabled"], false);
    assert_eq!(jobs["state"], "not on this gateway yet");
    assert_eq!(jobs["status"], "agentic/os.json");
    let job = read_json(agentic::acp_job_read("7", NOW));
    assert_eq!(job["enabled"], false);
    let rules = read_json(agentic::spending_read(NFT, NOW));
    assert_eq!(rules["schema"], "tolly.agentic.spending.v1");
    assert_eq!(rules["enabled"], false);
    assert_only_agent_gets();
}

#[test]
fn os_status_when_built_but_not_deployed_or_disabled() {
    let mut h = host();
    h.reply_http(
        &url(AgentRoute::Acp),
        200,
        &fixture("synthetic-acp-not-deployed.json"),
    );
    h.reply_http(
        &url(AgentRoute::SpendPolicy),
        200,
        &fixture("synthetic-spend-policy-not-deployed.json"),
    );
    h.reply_http(
        &url(AgentRoute::AcpJobs),
        503,
        &fixture("synthetic-acp-disabled-503.json"),
    );
    h.reply_http(
        &url(AgentRoute::AcpJob("7".into())),
        503,
        &fixture("synthetic-acp-disabled-503.json"),
    );
    h.reply_http(
        &url(AgentRoute::AgentPolicy(NFT.into())),
        200,
        &fixture("synthetic-spend-policy-not-deployed.json"),
    );
    fake_host::install(h);
    let doc = read_json(agentic::os_read(NOW));
    assert_eq!(doc["acp"]["state"], "not deployed");
    assert!(
        doc["acp"]["note"]
            .as_str()
            .unwrap()
            .contains("not deployed yet")
    );
    assert_eq!(doc["spend_policy"]["state"], "not deployed");
    let jobs = read_json(agentic::acp_jobs_read(NOW));
    assert_eq!(jobs["state"], "disabled");
    assert!(jobs["note"].as_str().unwrap().contains("not enabled"));
    assert_eq!(
        read_json(agentic::acp_job_read("7", NOW))["state"],
        "disabled"
    );
    assert_eq!(
        read_json(agentic::spending_read(NFT, NOW))["state"],
        "not deployed"
    );
}

#[test]
fn os_status_when_live() {
    let mut h = host();
    os_live(&mut h);
    fake_host::install(h);
    let doc = read_json(agentic::os_read(NOW));
    assert_eq!(doc["acp"]["state"], "live");
    assert_eq!(
        doc["acp"]["address"],
        "0x1111111111111111111111111111111111111111"
    );
    assert_eq!(doc["acp"]["chain_id_is_arc"], true);
    assert_eq!(doc["acp"]["token_is_arc_usdc"], true);
    assert_eq!(doc["acp"]["fees"]["platformBp"], 500);
    assert_eq!(doc["spend_policy"]["state"], "live");
    assert_eq!(
        doc["spend_policy"]["defaults"]["dailyCapAtomic"],
        "10000000"
    );
    assert_eq!(doc["spend_policy"]["chain_id_is_arc"], true);

    let jobs = read_json(agentic::acp_jobs_read(NOW));
    assert_eq!(jobs["enabled"], true);
    let job = &jobs["jobs"][0];
    assert_eq!(job["job_id"], "7");
    assert_eq!(job["budget"]["usd"], "2.5");
    assert_eq!(job["detail"], "agentic/acp/jobs/7.json");
    let description = job["description"].as_str().unwrap();
    assert!(!description.contains('\u{202e}') && !description.contains('\u{0007}'));

    let detail = read_json(agentic::acp_job_read("7", NOW));
    assert_eq!(detail["next_actions"][0]["action"], "submit");
    assert_eq!(detail["timeline"][0]["event"], "JobCreated");
    assert_eq!(detail["delivery"]["matchesChain"], false);
    assert_eq!(code(&agentic::acp_job_read("8", NOW)), -1);
    assert_eq!(code(&agentic::acp_job_read("07", NOW)), -3);

    let rules = read_json(agentic::spending_read(NFT, NOW));
    assert_eq!(rules["enabled"], true);
    assert_eq!(rules["status"], "active");
    assert_eq!(rules["config"]["remainingTodayAtomic"], "9750000");
    assert_eq!(rules["payees"].as_array().unwrap().len(), 2);
    assert_eq!(
        code(&agentic::spending_read(
            "00000000-0000-0000-0000-000000000000",
            NOW
        )),
        -1
    );
    assert_eq!(code(&agentic::spending_read("lazarus", NOW)), -3);
    assert_only_agent_gets();
}

#[test]
fn os_status_reports_a_transport_failure_as_backend() {
    let mut h = host();
    os_not_served(&mut h);
    h.forget_http(&url(AgentRoute::Acp));
    fake_host::install(h);
    assert_eq!(code(&agentic::os_read(NOW)), -4);
}

// ---- transport ----

#[test]
fn oversized_rate_limited_and_non_json_answers_are_backend_errors() {
    let mut h = host();
    h.forget_http(&url(AgentRoute::Services));
    h.reply_http_bytes(&url(AgentRoute::Services), 200, b"<html>cdn check</html>");
    h.forget_http(&url(AgentRoute::Agents));
    h.reply_http(
        &url(AgentRoute::Agents),
        429,
        &json!({"error": "rate_limited"}),
    );
    h.forget_http(&url(AgentRoute::Stats));
    let big = json!({ "stats": "x".repeat(70 * 1024) });
    h.reply_http(&url(AgentRoute::Stats), 200, &big);
    fake_host::install(h);
    let services = agentic::services_read(NOW);
    assert_eq!(code(&services), -4);
    assert!(message(&services).contains("not JSON"));
    let agents = agentic::agents_read(NOW);
    assert_eq!(code(&agents), -4);
    assert!(message(&agents).contains("rate limited"));
    let overview = agentic::overview_read(NOW);
    assert_eq!(code(&overview), -4);
    assert!(message(&overview).contains("too large"));
}

#[test]
fn an_unreachable_gateway_error_carries_no_url() {
    let mut h = host();
    h.forget_http(&url(AgentRoute::Services));
    fake_host::install(h);
    let response = agentic::services_read(NOW);
    assert_eq!(code(&response), -4);
    assert!(
        !message(&response).contains("://"),
        "{}",
        message(&response)
    );
}

// ---- route files ----

fn route_sources(dir: &str) -> Vec<(std::path::PathBuf, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut out = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                out.push((path, source));
            }
        }
    }
    walk(&root, &mut out);
    out
}

#[test]
fn agentic_route_files_are_read_only_http_routes() {
    let files = route_sources("files/agentic");
    assert_eq!(files.len(), 23, "agentic route count");
    for (path, source) in files {
        for forbidden in [
            "write_spec",
            "write:",
            "tx_stage",
            "chain_read",
            "bloom:chain",
            "bloom:tx.outbox",
            "bloom:store",
            "bloom:vfs",
            "store_",
            "sign",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} must stay read only ({forbidden})",
                path.display()
            );
        }
        assert!(
            source.contains("petal::http_read_spec(")
                || source.contains("petal::http_dir_spec()")
                || source.contains("petal::static_dir_spec()"),
            "{} uses a read-only spec",
            path.display()
        );
    }
}

#[test]
fn guide_maps_every_route_file() {
    let guide = crate::docs::GUIDE_MD;
    for path in [
        "status.json",
        "markets.json",
        "tokens/<address>.json",
        "quote/<address>/buy/<usdc>.json",
        "quote/<address>/sell/<amount>.json",
        "wallets/<wallet>/<index>/buy.json",
        "wallets/<wallet>/<index>/sell.json",
        "wallets/<wallet>/<index>/launch.json",
        "wallets/<wallet>/<index>/operations/<id>.json",
        "wallets/<wallet>/<index>/positions.json",
        "agentic/overview.json",
        "agentic/services.json",
        "agentic/services/<id>.json",
        "agentic/routed.json",
        "agentic/routed/search/<term>.json",
        "agentic/routed/<routeId>.json",
        "agentic/agents.json",
        "agentic/agents/<id>.json",
        "agentic/jobs.json",
        "agentic/jobs/<jb_id>.json",
        "agentic/os.json",
        "agentic/acp/jobs.json",
        "agentic/acp/jobs/<jobId>.json",
        "agentic/spending/<agent NFT id>.json",
        "AGENTS.md",
    ] {
        assert!(
            guide.contains(&format!("`{path}`")),
            "guide.md misses {path}"
        );
    }
    // Every agentic read file is mentioned (by its route name).
    for (path, _) in route_sources("files/agentic") {
        let name = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".rs", "");
        if name == "$index" {
            continue;
        }
        let stem = name
            .replace("[id]", "")
            .replace("[route]", "")
            .replace("[query]", "")
            .replace("[agent]", "");
        assert!(
            guide.contains(stem.trim_start_matches('.')),
            "guide.md misses {name}"
        );
    }
    assert!(!guide.contains("/bloom/"), "paths are mount-relative");
}

#[test]
fn package_docs_point_agents_at_the_guide() {
    assert!(crate::docs::AGENTS_MD.contains("guide.md"));
    assert!(crate::docs::README_MD.contains("guide.md"));
    assert!(crate::docs::AGENTS_MD.contains("agentic/"));
}
