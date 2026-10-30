# Changelog

## v0.3.0

Agentic side, read only. Requires Bloom v0.3.2 or later (explicit HD
accounts); paying TOLLY's services through Bloom needs v0.4.1, which
prices Arc USDC correctly (bloom-directory/bloom#397). The Petal SDK pin stays at
`bloom-directory/petal@73c5b06a` (same `bloom:route@0.1.0` WIT as the newer
SDK revisions, so the package keeps the same WIT digest).

### Added

- Buying from Bloom: every service file's `how_to_buy.from_bloom` gives the exact
  `requests/new` request (POST with the JSON input, wallet, `max_amount_usd`),
  and `guide.md` section 5 walks through plan, confirm and response. The
  Petal itself still never signs or pays.

- `guide.md`: the navigation map of every route, trading and agentic
  (static, embedded from `docs/GUIDE.md`). AGENTS.md now starts from it.
- `agentic/`: 23 read-only routes over TOLLY's agent exchange at
  `agent.tollylabs.com`:
  - `overview.json`: 30-day exchange stats, the listed agents, how paying
    works;
  - `services.json`, `services/<id>.json`: TOLLY's pay-per-call catalog with
    exact USDC prices, `latest` per family, input/output schemas, example
    input, limits, limitations, refund policy and `how_to_buy`;
  - `routed.json`, `routed/<routeId>.json`, `routed/search/<term>.json`:
    other sellers TOLLY resells (all-in price, seller price, fee, paused
    sellers), searched locally;
  - `agents.json`, `agents/<id>.json`: the agent directory and profiles;
  - `jobs.json`, `jobs/<jb_id>.json`: the open job board and delivery
    previews;
  - `os.json`: Tolly OS status (ACP escrow and agent spending rules: live,
    not deployed, disabled or not on this gateway yet; address, codehash,
    fees and limits when live);
  - `acp/jobs.json`, `acp/jobs/<jobId>.json`: ERC-8183 escrow jobs with next
    actions by role and the event timeline;
  - `spending/<agent NFT id>.json`: an agent wallet's on-chain spending
    rules.
- `petal.toml`: a second `[[net.allow]]` rule, binding `tolly-agent`, host
  `agent.tollylabs.com`, `GET` only, 14 exact public paths. No new
  capability: the agentic routes use `bloom:http` only. The consent summary
  says what is read and that nothing is paid, posted or changed.
- `docs/BLOOM_AGENT_GUIDE.md`: the operator contract for a Claude agent
  driving Bloom with this Petal (trading loop, agentic navigation, money
  rules).
- `CHANGELOG.md`.

### Not in this release (by decision)

- Paying for services (x402). Bloom can sign x402 / EIP-3009 payments, but
  its paid-HTTP valuation does not recognise Arc USDC yet, so TOLLY prices
  read as dollars and are refused (README "x402 in Bloom"). The Petal
  points at `npx -y @tollylabs/x402-mcp` instead.
- Staged ACP actions and spending-policy owner actions: the contracts are
  not deployed or audited, so there is no address or codehash to pin
  (README D17).

### Changed

- `api::http_get` is the shared fixed-target GET transport (same error
  mapping as before).
- Route count 22 -> 46; CI and the release workflow assert 46 (the release
  workflow still said 19).
- README: Bloom requirement, layout, tests, decisions D10 (Bloom's glob
  semantics: a trailing `*` is a prefix) and D15 to D17.

### Upgrade

Every new build changes the package hash: after installing v0.3.0 the owner
re-runs the wallet policy ceremony (README Quickstart, steps 2 and 3) so
staged trades can be confirmed. Finish or cancel pending v0.2.x outbox
entries first: a new package cannot inspect entries staged by the old one.

## v0.2.x (main after v0.2.0, unreleased)

- HD accounts (josh-richardson, PRs #1 and #2): account-scoped wallet
  routes under `wallets/<wallet>/<index>/`, Bloom supplies the wallet and
  account inventory; requires Bloom v0.3.2.

## v0.2.0 (2026-09-11)

- Production API only (`api.tollylabs.com`); quotes, buy, sell, launch,
  operations and positions.
