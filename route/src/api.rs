//! The TOLLY public read API and its projections.
//!
//! Targets are fixed: the single `Network` (production, `api.tollylabs.com`,
//! no `/api` prefix) supplies the base URL and an `ApiRoute` variant supplies
//! the path, so route code cannot direct a request at an arbitrary host or
//! path. The host is declared in `petal.toml` `[[net.allow]]`.

use alloy_primitives::Address;
use petal::{DispatchResponse, HostStatus, HttpRequest, SdkError};
use serde_json::{Value, json};

use crate::amount::{addr_hex, is_bytes32_hex, parse_any_address};
use crate::constants::{
    API_PROD, CHAIN, CHAIN_ID, INTERFACE_FEE_BPS, MULTI_ROUTER, PAD, PAD_TOKEN_DECIMALS,
    PAD_TOTAL_SUPPLY_RAW, POOL_FEE_PAD, SOURCE_DIGEST, SWAP_ROUTER02,
};
use crate::host;
use crate::policy::{MARKETS_LIMIT, MAX_OP_USDC_HUMAN};
use crate::sanitize_host_error;

const MAX_HEALTH_BYTES: usize = 16 * 1024;
const MAX_LIST_BYTES: usize = 512 * 1024;
const MAX_DETAIL_BYTES: usize = 64 * 1024;

/// The network this Petal serves. There is exactly one: the public
/// production API. Operation records carry its name so that they stay
/// self-describing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Prod,
}

impl Network {
    pub fn name(self) -> &'static str {
        match self {
            Self::Prod => "prod",
        }
    }

    pub fn api_base(self) -> &'static str {
        match self {
            Self::Prod => API_PROD,
        }
    }

    pub fn current() -> Self {
        Self::Prod
    }
}

/// The only endpoints this Petal reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiRoute {
    Health,
    /// `GET /tokens?scope=ours&sort=volume&dir=desc&limit=50`
    Markets,
    /// `GET /tokens?scope=ours&creator=<address>&limit=50` (launch completion, D11)
    LaunchesByCreator(Address),
    /// `GET /token/<address>`
    Token(Address),
}

impl ApiRoute {
    pub fn url(&self, network: Network) -> String {
        let base = network.api_base();
        match self {
            Self::Health => format!("{base}/health"),
            Self::Markets => {
                format!("{base}/tokens?scope=ours&sort=volume&dir=desc&limit={MARKETS_LIMIT}")
            }
            Self::LaunchesByCreator(creator) => {
                format!(
                    "{base}/tokens?scope=ours&sort=recent&dir=desc&limit={MARKETS_LIMIT}&creator={}",
                    addr_hex(*creator)
                )
            }
            Self::Token(address) => format!("{base}/token/{}", addr_hex(*address)),
        }
    }

    fn max_bytes(&self) -> usize {
        match self {
            Self::Health => MAX_HEALTH_BYTES,
            Self::Markets | Self::LaunchesByCreator(_) => MAX_LIST_BYTES,
            Self::Token(_) => MAX_DETAIL_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    NotFound,
    Upstream(String),
}

impl ApiError {
    pub fn response(&self) -> DispatchResponse {
        match self {
            Self::NotFound => petal::error(-1, "unknown token"),
            Self::Upstream(message) => petal::error(-4, format!("TOLLY API: {message}")),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotFound => "not found".into(),
            Self::Upstream(message) => message.clone(),
        }
    }
}

/// GET one of the fixed routes and parse the JSON body.
pub fn fetch_json(network: Network, route: &ApiRoute) -> Result<Value, ApiError> {
    let request = HttpRequest {
        method: "GET".into(),
        url: route.url(network),
        headers: vec![("accept".into(), "application/json".into())],
        body: Vec::new(),
    };
    let response = match host::http_fetch(&request, route.max_bytes()) {
        Ok(response) => response,
        Err(SdkError::Host(HostStatus::BufferTooSmall { needed })) => {
            return Err(ApiError::Upstream(format!(
                "response too large ({needed} bytes)"
            )));
        }
        Err(SdkError::Host(HostStatus::Denied)) => {
            return Err(ApiError::Upstream(
                "request denied by the host network policy".into(),
            ));
        }
        Err(e) => return Err(ApiError::Upstream(sanitize_host_error(&e.message()))),
    };
    if response.status == 404 {
        return Err(ApiError::NotFound);
    }
    if !(200..300).contains(&response.status) {
        return Err(ApiError::Upstream(format!("HTTP {}", response.status)));
    }
    serde_json::from_slice(&response.body)
        .map_err(|_| ApiError::Upstream("body is not JSON".into()))
}

// ---- helpers ----

fn f64_of(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn num_string(value: Option<f64>) -> Value {
    match value {
        Some(v) if v.is_finite() => Value::String(format!("{v}")),
        _ => Value::Null,
    }
}

fn str_of(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn ms_of_ts(value: &Value) -> Value {
    match value.as_f64() {
        Some(ts) if ts.is_finite() && ts > 0.0 => json!((ts * 1000.0) as u64),
        _ => Value::Null,
    }
}

fn addr_of(value: &Value) -> Option<Address> {
    value.as_str().and_then(parse_any_address)
}

fn addr_json(value: Option<Address>) -> Value {
    value
        .map(|a| Value::String(addr_hex(a)))
        .unwrap_or(Value::Null)
}

// ---- /health ----

#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    pub ok: bool,
    pub chain_id: Option<u64>,
    pub pad: Option<Address>,
    pub indexing: Value,
}

pub fn parse_health(value: &Value) -> Health {
    Health {
        ok: value["ok"].as_bool().unwrap_or(false),
        chain_id: value["chainId"].as_u64(),
        pad: addr_of(&value["pad"]),
        indexing: value.get("indexing").cloned().unwrap_or(Value::Null),
    }
}

/// `status.json`: `/health` projected plus what this build is.
pub fn status_document(
    network: Network,
    health: &Value,
    writes_enabled: bool,
    now_ms: u64,
) -> Value {
    let parsed = parse_health(health);
    let indexing = &parsed.indexing;
    json!({
        "schema": "tolly.status.v1",
        "petal": "tolly",
        "status": if parsed.ok { "ok" } else { "degraded" },
        "network": network.name(),
        "api_base": network.api_base(),
        "chain": CHAIN,
        "chain_id": CHAIN_ID,
        "writes_enabled": writes_enabled,
        "max_op_usdc": MAX_OP_USDC_HUMAN,
        "interface_fee_bps": INTERFACE_FEE_BPS,
        "api": {
            "ok": parsed.ok,
            "chainId": parsed.chain_id,
            "pad": addr_json(parsed.pad),
            "indexing": {
                "state": indexing.get("state").cloned().unwrap_or(Value::Null),
                "head": indexing.get("head").cloned().unwrap_or(Value::Null),
                "nativeLag": indexing.get("nativeLag").cloned().unwrap_or(Value::Null),
                "externalLag": indexing.get("externalLag").cloned().unwrap_or(Value::Null),
                "headAgeMs": indexing.get("headAgeMs").cloned().unwrap_or(Value::Null),
            }
        },
        "constants_digest": SOURCE_DIGEST,
        "pad": addr_hex(PAD),
        "pad_matches_constants": parsed.pad == Some(PAD),
        "chain_id_matches_constants": parsed.chain_id == Some(CHAIN_ID),
        "checked_ms": now_ms,
        "docs": ["README.md", "AGENTS.md"],
    })
}

// ---- /tokens ----

/// Mirror of `src/data/liquidityAuthority.ts`: a bytes32 V4 PoolId's liquidity
/// is attributable only with explicit Pools.trade provenance.
fn liquidity_attributable(
    pool: Option<&str>,
    liquidity: Option<f64>,
    source: Option<&str>,
) -> bool {
    match pool {
        Some(pool) if is_bytes32_hex(pool) => {
            liquidity.is_some() && source == Some("pools-trade-api")
        }
        _ => liquidity.is_some(),
    }
}

pub fn market_row(row: &Value) -> Option<Value> {
    let address = addr_of(&row["address"])?;
    let pool = row["pool"].as_str();
    let liquidity = f64_of(&row["liquidity"]);
    let source = row["liquiditySource"].as_str();
    let attributable = liquidity_attributable(pool, liquidity, source);
    let provenance = if row["external"].as_bool() == Some(true) {
        "external"
    } else {
        "pad"
    };
    Some(json!({
        "address": addr_hex(address),
        "symbol": str_of(&row["symbol"]),
        "name": str_of(&row["name"]),
        "provenance": provenance,
        "price_usdc": num_string(f64_of(&row["price"])),
        "market_cap_usdc": num_string(f64_of(&row["marketCap"])),
        "liquidity_usdc": if attributable { num_string(liquidity) } else { Value::Null },
        "liquidity_attributable": attributable,
        "volume_24h_usdc": num_string(f64_of(&row["volume24h"])),
        "change_1h_pct": row.get("change1h").cloned().unwrap_or(Value::Null),
        "change_24h_pct": row.get("change24h").cloned().unwrap_or(Value::Null),
        "txns_24h": row.get("txns24h").cloned().unwrap_or(Value::Null),
        "traders_24h": row.get("traders24h").cloned().unwrap_or(Value::Null),
        "last_trade_ms": ms_of_ts(&row["lastTradeTs"]),
        "created_ms": ms_of_ts(&row["created_ts"]),
        "created_block": row.get("created_block").cloned().unwrap_or(Value::Null),
        "creator": addr_json(addr_of(&row["creator"])),
        "sym_rank": row.get("sym_rank").cloned().unwrap_or(Value::Null),
        "exit_check": row.get("exitCheck").cloned().unwrap_or(Value::Null),
        "detail": format!("tokens/{}.json", addr_hex(address)),
    }))
}

/// `markets.json`
pub fn markets_document(list: &Value, now_ms: u64) -> Result<Value, String> {
    let rows = list["tokens"].as_array().ok_or("tokens is not an array")?;
    let tokens: Vec<Value> = rows.iter().filter_map(market_row).collect();
    Ok(json!({
        "schema": "tolly.markets.v1",
        "scope": list.get("scope").cloned().unwrap_or(json!("ours")),
        "sort": "volume",
        "limit": MARKETS_LIMIT,
        "total": list.get("total").cloned().unwrap_or(Value::Null),
        "degraded": list.get("degraded").and_then(Value::as_bool).unwrap_or(false),
        "fetched_ms": now_ms,
        "note": "any token, listed here or not, is addressable as tokens/<address>.json; degraded:true means the external index was unavailable, treat the list as unknown rather than empty",
        "tokens": tokens,
    }))
}

/// Lowercase addresses of the listed rows (for `tokens/`).
pub fn market_addresses(list: &Value) -> Vec<String> {
    list["tokens"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| addr_of(&row["address"]).map(addr_hex))
                .collect()
        })
        .unwrap_or_default()
}

/// One of a creator's pad launches (D11 launch completion).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRow {
    pub address: Address,
    pub symbol: Option<String>,
    pub name: Option<String>,
    pub pool: Option<Address>,
    pub created_block: Option<u64>,
}

pub fn launch_rows(list: &Value) -> Vec<LaunchRow> {
    list["tokens"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(LaunchRow {
                        address: addr_of(&row["address"])?,
                        symbol: str_of(&row["symbol"]),
                        name: str_of(&row["name"]),
                        pool: addr_of(&row["pool"]),
                        created_block: row["created_block"].as_u64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- /token/:address ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    Pad,
    External,
}

impl Provenance {
    pub fn is_external(self) -> bool {
        self == Self::External
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Pad => "pad",
            Self::External => "external",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VenueKind {
    V3,
    V2,
    V4,
    Pump,
    Dyor,
}

impl VenueKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "v3" => Some(Self::V3),
            "v2" => Some(Self::V2),
            "v4" => Some(Self::V4),
            "pump" => Some(Self::Pump),
            "dyor" => Some(Self::Dyor),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::V3 => "v3",
            Self::V2 => "v2",
            Self::V4 => "v4",
            Self::Pump => "pump",
            Self::Dyor => "dyor",
        }
    }
}

/// A candidate venue, the one shape every quote/execution surface uses
/// (port of `venues.ts#venuesForToken`).
#[derive(Clone, Debug, PartialEq)]
pub struct Venue {
    /// Address for V2/V3/custom venues; bytes32 PoolId for V4.
    pub id: String,
    pub kind: VenueKind,
    /// V3/V4 tier in hundredths of a bip.
    pub fee: Option<u32>,
    /// V2 swap fee in bps; `None` = unsolved.
    pub fee_bps: Option<u16>,
    pub factory: Option<Address>,
    pub router: Option<Address>,
    pub liquidity_usdc: Option<f64>,
    pub liquidity_source: Option<String>,
    pub tradeable: bool,
    /// A verified ordinary V2 identity whose fee is still unsolved.
    pub recoverable_v2: bool,
    pub quote_token: Option<Address>,
    pub native_quote: bool,
    pub currency0: Option<Address>,
    pub currency1: Option<Address>,
    pub tick_spacing: Option<i32>,
    pub hooks: Option<Address>,
    pub launchpad: Option<String>,
    pub supports_fot: Option<bool>,
}

impl Venue {
    pub fn pool_address(&self) -> Option<Address> {
        if self.kind == VenueKind::V4 {
            None
        } else {
            parse_any_address(&self.id)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TokenDetail {
    pub address: Address,
    pub symbol: String,
    pub name: String,
    pub decimals: u32,
    pub provenance: Provenance,
    pub launchpad: Option<String>,
    pub source: Option<String>,
    pub image_uri: Option<String>,
    pub website: Option<String>,
    pub twitter: Option<String>,
    pub telegram: Option<String>,
    pub creator: Option<Address>,
    pub created_ms: Option<u64>,
    pub created_block: Option<u64>,
    /// Whole tokens, as the API serves it (pad launches are exactly 1e9).
    pub supply: Option<String>,
    pub liquidity_usdc: Option<f64>,
    pub liquidity_source: Option<String>,
    pub liquidity_attributable: bool,
    pub quote_decimals: u32,
    pub pool: Option<String>,
    pub dex: Option<String>,
    pub supports_fot: Option<bool>,
    pub venues: Vec<Venue>,
}

fn bad_pool_id(kind: VenueKind, pool: &str) -> bool {
    match kind {
        VenueKind::V4 => !is_bytes32_hex(pool),
        _ => parse_any_address(pool).is_none(),
    }
}

/// Exact port of `venuesForToken`: keep a pool iff its id shape is valid and
/// it is tradeable, or it is the one recoverable ordinary-V2 identity;
/// otherwise (pad launches have no `pools`) the fallback single V3 venue.
pub fn venues_for_token(detail: &Value) -> Vec<Venue> {
    let token = &detail["token"];
    let external = token["external"].as_bool() == Some(true);
    let indexed: Vec<Venue> = detail["pools"]
        .as_array()
        .map(|pools| {
            pools
                .iter()
                .filter_map(|pool| {
                    let kind = VenueKind::parse(pool["kind"].as_str()?)?;
                    let id = pool["pool"].as_str()?;
                    if bad_pool_id(kind, id) {
                        return None;
                    }
                    let tradeable = pool["tradeable"].as_bool() == Some(true);
                    let fee_bps = pool["feeBps"].as_u64().and_then(|f| u16::try_from(f).ok());
                    let factory = addr_of(&pool["factory"]);
                    let router = addr_of(&pool["router"]);
                    let recoverable_v2 = !tradeable
                        && kind == VenueKind::V2
                        && fee_bps.is_none()
                        && factory.is_some()
                        && router.is_some();
                    if !tradeable && !recoverable_v2 {
                        return None;
                    }
                    let id = if kind == VenueKind::V4 {
                        id.to_ascii_lowercase()
                    } else {
                        addr_hex(parse_any_address(id)?)
                    };
                    Some(Venue {
                        id,
                        kind,
                        fee: pool["fee"].as_u64().and_then(|f| u32::try_from(f).ok()),
                        fee_bps,
                        factory,
                        router,
                        liquidity_usdc: f64_of(&pool["liquidity"]),
                        liquidity_source: str_of(&pool["liquiditySource"]),
                        tradeable: tradeable || recoverable_v2,
                        recoverable_v2,
                        quote_token: addr_of(&pool["quoteToken"]),
                        native_quote: pool["nativeQuote"].as_bool() == Some(true),
                        currency0: addr_of(&pool["currency0"]),
                        currency1: addr_of(&pool["currency1"]),
                        tick_spacing: pool["tickSpacing"]
                            .as_i64()
                            .and_then(|t| i32::try_from(t).ok()),
                        hooks: addr_of(&pool["hooks"]),
                        launchpad: str_of(&pool["launchpad"]),
                        supports_fot: pool["supportsFot"].as_bool(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if !indexed.is_empty() {
        return indexed;
    }
    let fee = token["fee"].as_u64().and_then(|f| u32::try_from(f).ok());
    let fallback_fee = if external {
        fee
    } else {
        fee.or(Some(POOL_FEE_PAD))
    };
    let dex = token["dex"].as_str();
    let pool = token["pool"]
        .as_str()
        .and_then(parse_any_address)
        .filter(|p| *p != Address::ZERO);
    match (pool, fallback_fee) {
        (Some(pool), Some(fee)) if dex.is_none() || dex == Some("v3") => vec![Venue {
            id: addr_hex(pool),
            kind: VenueKind::V3,
            fee: Some(fee),
            fee_bps: None,
            factory: None,
            router: None,
            liquidity_usdc: f64_of(&token["liquidity"]),
            liquidity_source: str_of(&token["liquiditySource"]),
            tradeable: true,
            recoverable_v2: false,
            quote_token: None,
            native_quote: false,
            currency0: None,
            currency1: None,
            tick_spacing: None,
            hooks: None,
            launchpad: None,
            supports_fot: None,
        }],
        _ => Vec::new(),
    }
}

/// Parse a `/token/:address` document. Refuses a detail whose `token.address`
/// differs from the one requested (`api.ts:780`).
pub fn parse_token_detail(detail: &Value, requested: Address) -> Result<TokenDetail, String> {
    let token = &detail["token"];
    let address = addr_of(&token["address"]).ok_or("detail has no token.address")?;
    if address != requested {
        return Err("detail token.address does not match the requested address".into());
    }
    let external = token["external"].as_bool() == Some(true);
    let provenance = if external {
        Provenance::External
    } else {
        Provenance::Pad
    };
    let decimals = if external {
        token["decimals"]
            .as_u64()
            .and_then(|d| u32::try_from(d).ok())
            .ok_or("external token has no decimals")?
    } else {
        PAD_TOKEN_DECIMALS
    };
    if decimals > 36 {
        return Err("token decimals out of range".into());
    }
    let supply = if external {
        f64_of(&token["supply"]).map(|s| format!("{s}"))
    } else {
        Some("1000000000".into())
    };
    let pool = str_of(&token["pool"]);
    let liquidity = f64_of(&token["liquidity"]);
    let liquidity_source = str_of(&token["liquiditySource"]);
    let quote_decimals = detail["quoteDecimals"]
        .as_u64()
        .and_then(|d| u32::try_from(d).ok())
        .unwrap_or(6);
    Ok(TokenDetail {
        address,
        symbol: str_of(&token["symbol"]).unwrap_or_default(),
        name: str_of(&token["name"]).unwrap_or_default(),
        decimals,
        provenance,
        launchpad: str_of(&token["launchpad"]),
        source: str_of(&detail["source"]),
        image_uri: str_of(&token["image_uri"]),
        website: str_of(&token["website"]),
        twitter: str_of(&token["twitter"]),
        telegram: str_of(&token["telegram"]),
        creator: addr_of(&token["creator"]),
        created_ms: token["created_ts"].as_u64().map(|ts| ts * 1000),
        created_block: token["created_block"].as_u64(),
        supply,
        liquidity_attributable: liquidity_attributable(
            pool.as_deref(),
            liquidity,
            liquidity_source.as_deref(),
        ),
        liquidity_usdc: liquidity,
        liquidity_source,
        quote_decimals,
        pool,
        dex: str_of(&token["dex"]),
        supports_fot: token["supportsFot"].as_bool(),
        venues: venues_for_token(detail),
    })
}

/// Fetch and parse a token detail in one step.
pub fn token_detail(network: Network, address: Address) -> Result<TokenDetail, DispatchResponse> {
    let detail = fetch_json(network, &ApiRoute::Token(address)).map_err(|e| e.response())?;
    parse_token_detail(&detail, address).map_err(|e| petal::error(-4, format!("TOLLY API: {e}")))
}

/// How (and whether) the day-1 Petal can execute on a venue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Execution {
    pub supported: bool,
    pub spender: Option<Address>,
    pub router_call: Option<&'static str>,
    pub reason: Option<&'static str>,
}

impl Execution {
    fn supported(spender: Address, router_call: &'static str) -> Self {
        Self {
            supported: true,
            spender: Some(spender),
            router_call: Some(router_call),
            reason: None,
        }
    }
    fn unsupported(reason: &'static str) -> Self {
        Self {
            supported: false,
            spender: None,
            router_call: None,
            reason: Some(reason),
        }
    }
}

/// Day-1 execution matrix (contracts extraction, derived table): pad tokens
/// go straight to SwapRouter02 with no fee; external V3 through
/// `MULTI_ROUTER.swapWithToll`; external V2 through `swapWithTollV2` only
/// with a solved fee; V4 and custom curves are quoted for honesty but not
/// executed (D1).
pub fn execution_for(venue: &Venue, provenance: Provenance) -> Execution {
    match venue.kind {
        VenueKind::V3 => {
            if venue.fee.is_none() {
                return Execution::unsupported("v3-missing-fee-tier");
            }
            match provenance {
                Provenance::Pad => Execution::supported(SWAP_ROUTER02, "exactInputSingle"),
                Provenance::External => Execution::supported(MULTI_ROUTER, "swapWithToll"),
            }
        }
        VenueKind::V2 => {
            if provenance == Provenance::Pad {
                return Execution::unsupported("pad-token-v2-unsupported");
            }
            if venue.fee_bps.is_none() {
                return Execution::unsupported("v2-fee-unsolved");
            }
            if venue.factory.is_none() {
                return Execution::unsupported("v2-factory-unknown");
            }
            if venue.supports_fot == Some(true) {
                return Execution::unsupported("fee-on-transfer-unsupported");
            }
            Execution::supported(MULTI_ROUTER, "swapWithTollV2")
        }
        VenueKind::V4 => Execution::unsupported("v4-follow-up"),
        VenueKind::Pump | VenueKind::Dyor => Execution::unsupported("custom-curve"),
    }
}

fn venue_json(venue: &Venue, provenance: Provenance) -> Value {
    let execution = execution_for(venue, provenance);
    json!({
        "id": venue.id,
        "kind": venue.kind.name(),
        "fee": venue.fee,
        "fee_bps": venue.fee_bps,
        "factory": addr_json(venue.factory),
        "router": addr_json(venue.router),
        "liquidity_usdc": num_string(venue.liquidity_usdc),
        "liquidity_source": venue.liquidity_source,
        "tradeable": venue.tradeable,
        "recoverable_v2": venue.recoverable_v2,
        "native_quote": venue.native_quote,
        "quote_token": addr_json(venue.quote_token),
        "currency0": addr_json(venue.currency0),
        "currency1": addr_json(venue.currency1),
        "tick_spacing": venue.tick_spacing,
        "hooks": addr_json(venue.hooks),
        "launchpad": venue.launchpad,
        "supports_fot": venue.supports_fot,
        "execution": if execution.supported { "supported" } else { "unsupported" },
        "execution_reason": execution.reason,
        "spender": addr_json(execution.spender),
        "router_call": execution.router_call,
    })
}

/// `tokens/[address].json`
pub fn token_document(detail: &TokenDetail) -> Value {
    let address = addr_hex(detail.address);
    json!({
        "schema": "tolly.token.v1",
        "address": address,
        "symbol": detail.symbol,
        "name": detail.name,
        "decimals": detail.decimals,
        "provenance": detail.provenance.name(),
        "launchpad": detail.launchpad,
        "source": detail.source,
        "image_uri": detail.image_uri,
        "website": detail.website,
        "twitter": detail.twitter,
        "telegram": detail.telegram,
        "creator": addr_json(detail.creator),
        "created_ms": detail.created_ms,
        "created_block": detail.created_block,
        "supply": detail.supply,
        "supply_raw_pad": if detail.provenance == Provenance::Pad { Value::String(PAD_TOTAL_SUPPLY_RAW.into()) } else { Value::Null },
        "price_usdc": Value::Null,
        "liquidity_usdc": if detail.liquidity_attributable { num_string(detail.liquidity_usdc) } else { Value::Null },
        "liquidity_attributable": detail.liquidity_attributable,
        "canonical_pool": detail.pool,
        "canonical_dex": detail.dex,
        "quote_decimals": detail.quote_decimals,
        "supports_fot": detail.supports_fot,
        "interface_fee_bps": INTERFACE_FEE_BPS,
        "interface_fee_applies_on_buy": detail.provenance.is_external(),
        "venues": detail.venues.iter().map(|v| venue_json(v, detail.provenance)).collect::<Vec<_>>(),
        "quote_paths": {
            "buy": format!("quote/{address}/buy/<usdc>.json"),
            "sell": format!("quote/{address}/sell/<amount>.json"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn barc_detail() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/prod-token-barc.json")).unwrap()
    }

    #[test]
    fn urls_are_fixed_per_route() {
        let barc: Address = "0x4753c45fb550fecaa143a47968659117e6ffc2ce"
            .parse()
            .unwrap();
        assert_eq!(Network::current(), Network::Prod);
        assert_eq!(Network::current().name(), "prod");
        assert_eq!(
            ApiRoute::Token(barc).url(Network::current()),
            "https://api.tollylabs.com/token/0x4753c45fb550fecaa143a47968659117e6ffc2ce"
        );
        assert_eq!(
            ApiRoute::LaunchesByCreator(barc).url(Network::current()),
            "https://api.tollylabs.com/tokens?scope=ours&sort=recent&dir=desc&limit=50&creator=0x4753c45fb550fecaa143a47968659117e6ffc2ce"
        );
    }

    #[test]
    fn prod_urls_have_no_api_prefix() {
        let tolly: Address = "0xbc43ce8dec648ea298c4275559b81d6261c90b67"
            .parse()
            .unwrap();
        assert_eq!(Network::Prod.api_base(), "https://api.tollylabs.com");
        assert_eq!(
            ApiRoute::Health.url(Network::Prod),
            "https://api.tollylabs.com/health"
        );
        assert_eq!(
            ApiRoute::Markets.url(Network::Prod),
            "https://api.tollylabs.com/tokens?scope=ours&sort=volume&dir=desc&limit=50"
        );
        assert_eq!(
            ApiRoute::Token(tolly).url(Network::Prod),
            "https://api.tollylabs.com/token/0xbc43ce8dec648ea298c4275559b81d6261c90b67"
        );
        assert_eq!(
            ApiRoute::LaunchesByCreator(tolly).url(Network::Prod),
            "https://api.tollylabs.com/tokens?scope=ours&sort=recent&dir=desc&limit=50&creator=0xbc43ce8dec648ea298c4275559b81d6261c90b67"
        );
        for route in [
            ApiRoute::Health,
            ApiRoute::Markets,
            ApiRoute::Token(tolly),
            ApiRoute::LaunchesByCreator(tolly),
        ] {
            let prod = route.url(Network::Prod);
            assert!(prod.starts_with("https://api.tollylabs.com/"), "{prod}");
            assert!(!prod.contains("/api/"), "{prod}");
        }
    }

    /// The manifest declares exactly the host the `Network` reaches.
    #[test]
    fn manifest_allows_only_the_production_host() {
        let manifest = include_str!("../../petal.toml");
        assert_eq!(manifest.matches("[[net.allow]]").count(), 1);
        assert!(manifest.contains("host = \"api.tollylabs.com\""));
        let host = Network::current()
            .api_base()
            .trim_start_matches("https://")
            .to_owned();
        assert!(manifest.contains(&format!("host = \"{host}\"")));
    }

    /// Captured from `https://api.tollylabs.com` on 2026-09-11: health, the
    /// markets page and a pad token detail.
    #[test]
    fn prod_fixtures_parse() {
        let health: Value =
            serde_json::from_str(include_str!("../tests/fixtures/prod-health.json")).unwrap();
        let doc = status_document(Network::Prod, &health, false, 5);
        assert_eq!(doc["network"], "prod");
        assert_eq!(doc["api_base"], "https://api.tollylabs.com");
        assert_eq!(doc["status"], "ok");
        assert_eq!(doc["pad_matches_constants"], true);
        assert_eq!(doc["chain_id_matches_constants"], true);
        assert_eq!(doc["api"]["indexing"]["state"], "live");

        let list: Value =
            serde_json::from_str(include_str!("../tests/fixtures/prod-tokens-ours.json")).unwrap();
        let doc = markets_document(&list, 1).unwrap();
        assert_eq!(doc["scope"], "ours");
        assert_eq!(doc["degraded"], false);
        let rows = doc["tokens"].as_array().unwrap();
        assert_eq!(rows.len(), MARKETS_LIMIT as usize);
        assert!(
            rows.iter()
                .all(|row| row["detail"].as_str().unwrap().starts_with("tokens/0x"))
        );
        assert_eq!(market_addresses(&list).len(), MARKETS_LIMIT as usize);
        assert!(!launch_rows(&list).is_empty());

        let detail: Value =
            serde_json::from_str(include_str!("../tests/fixtures/prod-token-tolly.json")).unwrap();
        let tolly: Address = "0xbc43ce8dec648ea298c4275559b81d6261c90b67"
            .parse()
            .unwrap();
        let parsed = parse_token_detail(&detail, tolly).unwrap();
        assert_eq!(parsed.symbol, "TOLLY");
        assert_eq!(parsed.provenance, Provenance::Pad);
        assert_eq!(parsed.decimals, PAD_TOKEN_DECIMALS);
        assert_eq!(parsed.quote_decimals, 6);
        assert_eq!(parsed.created_block, Some(13_570_662));
        assert_eq!(
            parsed.venues.len(),
            1,
            "pad detail without pools: the fallback V3 venue"
        );
        let venue = &parsed.venues[0];
        assert_eq!(venue.kind, VenueKind::V3);
        assert_eq!(venue.fee, Some(POOL_FEE_PAD));
        assert_eq!(venue.id, "0x162df51c504e7b8321e07387932f333d9be16a72");
        let ex = execution_for(venue, Provenance::Pad);
        assert!(ex.supported);
        assert_eq!(ex.spender, Some(SWAP_ROUTER02));
        let doc = token_document(&parsed);
        assert_eq!(doc["interface_fee_applies_on_buy"], false);
        assert_eq!(doc["supply_raw_pad"], PAD_TOTAL_SUPPLY_RAW);
    }

    #[test]
    fn barc_fixture_yields_three_venues_with_v4_unsupported() {
        let detail = barc_detail();
        let address: Address = "0x4753c45fb550fecaa143a47968659117e6ffc2ce"
            .parse()
            .unwrap();
        let parsed = parse_token_detail(&detail, address).unwrap();
        assert_eq!(parsed.provenance, Provenance::External);
        assert_eq!(parsed.decimals, 18);
        assert_eq!(parsed.quote_decimals, 18);
        assert_eq!(parsed.supply.as_deref(), Some("1000000000"));
        assert!(
            parsed.liquidity_attributable,
            "pools-trade-api provenance is attributable"
        );
        assert_eq!(parsed.venues.len(), 3);
        let v4 = &parsed.venues[0];
        assert_eq!(v4.kind, VenueKind::V4);
        assert!(v4.native_quote);
        assert_eq!(v4.tick_spacing, Some(25));
        assert_eq!(v4.currency0, Some(Address::ZERO));
        assert_eq!(
            v4.id,
            "0x5e61e0abb3fa7a794b2c7c233f290a832803d4030b1c113200fa37c9472d79f8"
        );
        let ex = execution_for(v4, Provenance::External);
        assert!(!ex.supported);
        assert_eq!(ex.reason, Some("v4-follow-up"));
        let v3 = &parsed.venues[1];
        assert_eq!(v3.kind, VenueKind::V3);
        assert_eq!(v3.fee, Some(500));
        let ex = execution_for(v3, Provenance::External);
        assert!(ex.supported);
        assert_eq!(ex.spender, Some(MULTI_ROUTER));
        assert_eq!(ex.router_call, Some("swapWithToll"));
        assert_eq!(parsed.venues[2].fee, Some(3000));

        let doc = token_document(&parsed);
        assert_eq!(doc["schema"], "tolly.token.v1");
        assert_eq!(doc["interface_fee_applies_on_buy"], true);
        assert_eq!(doc["venues"][0]["execution"], "unsupported");
        assert_eq!(
            doc["venues"][1]["spender"],
            "0xf28c138a39c234554c847dbef467b073ddbd7451"
        );
        assert_eq!(
            doc["quote_paths"]["buy"],
            "quote/0x4753c45fb550fecaa143a47968659117e6ffc2ce/buy/<usdc>.json"
        );
    }

    #[test]
    fn address_mismatch_is_refused() {
        let detail = barc_detail();
        let other: Address = "0x2005cd22ea3c1acfaa9e01d3a178f356bb03c81c"
            .parse()
            .unwrap();
        assert!(parse_token_detail(&detail, other).is_err());
    }

    #[test]
    fn pad_detail_without_pools_gets_the_fallback_venue() {
        let detail = json!({
            "token": {
                "address": "0x2005cd22ea3c1acfaa9e01d3a178f356bb03c81c",
                "symbol": "CALENDAR", "name": "Calendar",
                "pool": "0x95bd2ec82e4442903ffe8635f01ac0066812a1b0",
                "liquidity": 56.79, "creator": "0x45ac5d219c26e1d40ab675eb0c295baca9be40c9",
                "created_block": 20188730, "created_ts": 1789068749
            },
            "quoteDecimals": 6
        });
        let address: Address = "0x2005cd22ea3c1acfaa9e01d3a178f356bb03c81c"
            .parse()
            .unwrap();
        let parsed = parse_token_detail(&detail, address).unwrap();
        assert_eq!(parsed.provenance, Provenance::Pad);
        assert_eq!(parsed.decimals, 18);
        assert_eq!(parsed.venues.len(), 1);
        let venue = &parsed.venues[0];
        assert_eq!(venue.kind, VenueKind::V3);
        assert_eq!(venue.fee, Some(10_000));
        assert_eq!(venue.id, "0x95bd2ec82e4442903ffe8635f01ac0066812a1b0");
        let ex = execution_for(venue, Provenance::Pad);
        assert_eq!(ex.spender, Some(SWAP_ROUTER02));
        assert_eq!(ex.router_call, Some("exactInputSingle"));
        let doc = token_document(&parsed);
        assert_eq!(doc["interface_fee_applies_on_buy"], false);
        assert_eq!(doc["supply_raw_pad"], PAD_TOTAL_SUPPLY_RAW);
    }

    #[test]
    fn recoverable_v2_is_kept_but_not_executable() {
        let detail = json!({
            "token": { "address": "0x1111111111111111111111111111111111111111", "symbol": "WARP", "name": "Warp", "decimals": 18, "external": true, "supply": 1e9, "dex": "v2" },
            "pools": [
                { "pool": "0x2222222222222222222222222222222222222222", "kind": "v2", "fee": null, "feeBps": null,
                  "factory": "0x3333333333333333333333333333333333333333", "router": "0x4444444444444444444444444444444444444444", "tradeable": false, "liquidity": 18187.0 },
                { "pool": "0x5555555555555555555555555555555555555555", "kind": "v2", "fee": null, "feeBps": 30,
                  "factory": "0x3333333333333333333333333333333333333333", "router": null, "tradeable": true, "liquidity": 10.0 },
                { "pool": "0x6666666666666666666666666666666666666666", "kind": "v2", "fee": null, "feeBps": null, "factory": null, "router": null, "tradeable": false },
                { "pool": "not-an-address", "kind": "v3", "fee": 3000, "tradeable": true },
                { "pool": "0x7777777777777777777777777777777777777777", "kind": "pump", "feeBps": 100, "router": "0x8888888888888888888888888888888888888888", "factory": "0x9999999999999999999999999999999999999999", "tradeable": true }
            ],
            "quoteDecimals": 6
        });
        let address: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let parsed = parse_token_detail(&detail, address).unwrap();
        assert_eq!(parsed.venues.len(), 3, "recoverable v2, solved v2, pump");
        let recoverable = &parsed.venues[0];
        assert!(recoverable.recoverable_v2 && recoverable.tradeable);
        assert_eq!(
            execution_for(recoverable, Provenance::External).reason,
            Some("v2-fee-unsolved")
        );
        let solved = &parsed.venues[1];
        let ex = execution_for(solved, Provenance::External);
        assert!(ex.supported);
        assert_eq!(ex.router_call, Some("swapWithTollV2"));
        assert_eq!(
            execution_for(&parsed.venues[2], Provenance::External).reason,
            Some("custom-curve")
        );
    }

    #[test]
    fn markets_projection_and_liquidity_authority() {
        let list: Value =
            serde_json::from_str(include_str!("../tests/fixtures/prod-tokens-ours-page.json"))
                .unwrap();
        let doc = markets_document(&list, 1).unwrap();
        assert_eq!(doc["schema"], "tolly.markets.v1");
        assert_eq!(doc["tokens"].as_array().unwrap().len(), 5);
        assert_eq!(doc["degraded"], false);
        let first = &doc["tokens"][0];
        assert_eq!(
            first["address"],
            "0x2005cd22ea3c1acfaa9e01d3a178f356bb03c81c"
        );
        assert_eq!(first["provenance"], "pad");
        assert_eq!(first["liquidity_attributable"], true);
        assert_eq!(first["liquidity_usdc"], "56.794526");
        assert_eq!(
            first["detail"],
            "tokens/0x2005cd22ea3c1acfaa9e01d3a178f356bb03c81c.json"
        );
        assert!(
            first.get("pool").is_none(),
            "list pool ids are never exposed"
        );
        assert_eq!(market_addresses(&list).len(), 5);

        let v4_row = json!({ "tokens": [{ "address": "0x4753c45fb550fecaa143a47968659117e6ffc2ce", "external": true,
            "pool": "0x5e61e0abb3fa7a794b2c7c233f290a832803d4030b1c113200fa37c9472d79f8", "liquidity": 100.0 }] , "degraded": true });
        let doc = markets_document(&v4_row, 1).unwrap();
        assert_eq!(doc["degraded"], true);
        assert_eq!(doc["tokens"][0]["liquidity_attributable"], false);
        assert_eq!(doc["tokens"][0]["liquidity_usdc"], Value::Null);
        assert_eq!(doc["tokens"][0]["provenance"], "external");
    }

    #[test]
    fn status_projection_flags_pad_drift() {
        let health: Value =
            serde_json::from_str(include_str!("../tests/fixtures/prod-health.json")).unwrap();
        let doc = status_document(Network::Prod, &health, false, 5);
        assert_eq!(doc["status"], "ok");
        assert_eq!(doc["pad_matches_constants"], true);
        assert_eq!(doc["chain_id_matches_constants"], true);
        assert_eq!(doc["writes_enabled"], false);
        assert_eq!(doc["api"]["indexing"]["state"], "live");
        let drifted = json!({ "ok": true, "chainId": 5042, "pad": "0x0000000000000000000000000000000000000001" });
        assert_eq!(
            status_document(Network::Prod, &drifted, true, 5)["pad_matches_constants"],
            false
        );
        assert_eq!(
            status_document(Network::Prod, &json!({"ok": false}), true, 5)["status"],
            "degraded"
        );
    }

    #[test]
    fn launch_rows_parse_creator_lists() {
        let list: Value =
            serde_json::from_str(include_str!("../tests/fixtures/prod-tokens-ours-page.json"))
                .unwrap();
        let rows = launch_rows(&list);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].created_block, Some(20188730));
        assert_eq!(rows[0].symbol.as_deref(), Some("CALENDAR"));
        assert!(rows[0].pool.is_some());
    }
}
