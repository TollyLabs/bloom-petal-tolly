# tolly — Bloom walletFS Petal for TOLLY (Arc)

A Bloom Petal that exposes the TOLLY launchpad and DEX as virtual files:
market discovery and token detail from the TOLLY public API, best-execution
quotes across a token's venues from on-chain simulations, and buy / sell /
launch operations staged into the owner's Bloom outbox. The Petal never signs
and never broadcasts. Agent-facing semantics live in [AGENTS.md](AGENTS.md);
this file is for developers.

Day-1 targets the STAGE API host (`https://stage.tollylabs.com/api`). Stage is
only an API host: it indexes Arc mainnet (chain id 5042), so every staged
transaction spends real USDC once the owner confirms it.

## Layout

```
petal.toml               package manifest: caps ceiling, net.allow, store policy
petal-build.toml         route build config; SDK pinned by full commit SHA; no extra crate deps
route/Cargo.toml         shared route crate (same SDK pin)
route/src/
  constants.rs           GENERATED from the frontend sources (scripts/gen-constants.mjs)
  policy.rs              day-1 limits and the write gate
  abi.rs amount.rs fee.rs           pure encoders and arithmetic
  api.rs                 fixed API targets + projections (venuesForToken port)
  chain.rs               the four allowlisted bloom:chain reads
  quote.rs               per-venue quoting, ranking, protection
  ops.rs                 operation record + state machine + reconciliation (run from the staging route's read)
  trace.rs               every write leaves a readable trace (record refusals, last-write marker)
  tx.rs swap.rs launch.rs positions.rs wallet.rs   the write/step flows
  host.rs                the only host seam; fake_host.rs under cfg(test)
  route_tests.rs         fake-host tests of every route flow (cfg(test))
route/files/             21 route files, one component each (see AGENTS.md table)
route/tests/fixtures/    stage API captures, calldata golden vectors
chain/arc.testnet.json   vendored copy of public/testnet.json (digest in constants.rs)
scripts/                 build.sh, check-route-architecture.sh, generators
```

Route files are controllers only: parameter validation, typed calls into
`crate::*`, projection. Shared code never inspects route identity.

## Build and test

```sh
bash scripts/check-route-architecture.sh
cargo test --manifest-path route/Cargo.toml --locked
petal build --root .            # or scripts/build.sh (installs the pinned CLI)
petal check --root .
wasm-tools component wit petal/tolly/<route>.wasm | grep import
petal package --root . --out dist/tolly-v0.1.2.petal.tar.gz
bloom petals build . && bloom petals install .    # needs a Bloom daemon
```

The SDK is pinned to `bloom-directory/petal` rev
`73c5b06a77599368fbc79fb7947a629b5b4c630e` in `petal-build.toml`,
`route/Cargo.toml` and `scripts/build.sh`; `build.sh` refuses drift.

Expected route count: 21.

### Generators (commit their output)

- `node scripts/gen-constants.mjs` — reads `public/testnet.json`,
  `src/data/chains.ts`, `src/data/venues.ts`, `src/data/v4Execution.ts` from
  the monorepo (or `TOLLY_REPO`) and writes `route/src/constants.rs` plus
  `chain/arc.testnet.json`. `constants_tests` re-checks the vendored file's
  digest and, when the monorepo file is present, that the two are identical.
- `python3 scripts/gen-calldata-fixtures.py` — writes
  `route/tests/fixtures/calldata.json` with the Python `eth_abi` codec
  (patched to the spec for empty dynamic values) and cross-checks every
  calldata vector with Foundry's `cast` when installed. The `createToken`
  case is the frontend's own no-broadcast E2E case
  (`scripts/test-launch-calldata.mjs`). `abi::tests` asserts byte equality and
  re-derives every selector/topic from keccak.

### Tests

- Pure cores: amount grammar, fee matrix and decimal invariance, protection
  floors, ABI golden vectors and decoders (`tokens(address)` pool at word 1),
  `venuesForToken` on the captured BARC detail (V4 + two V3), a pad detail
  without `pools`, a recoverable V2, state-machine transitions (every
  `tx_inspect` state), non-regression of terminal states, idempotency digest.
- Route flows against the fake host (`route/src/route_tests.rs`): status,
  markets, token detail, buy/sell quotes (V4 best but unsupported, QuoterV2
  revert, sell normalisation), the buy walk (writes disabled → -2 and a
  `failed/writes-disabled` record plus `last_write`, cap → -3,
  approve-then-swap with gross `swapWithToll` and a fresh floor, bound-id
  mismatch → -3, venue pin rules, stage denial and error classification,
  persist failure after stage → `stage_in_flight` → refuse → acknowledge,
  claim race, pending dedupe, completion by balance delta, zero-delta buys
  stay `confirmed`, a forgotten outbox entry never regresses a receipt),
  reconciliation from the staging route (`operations/<id>.json` is a pure
  store projection: no `tx_inspect`, chain, HTTP or save, and a `refresh`
  hint; a `buy.json` read reconciles a staged buy to `confirmed`/`completed`,
  persists it and lists it in `reconciled[]` with `changed: true`; a sell is
  not reconciled by the buy route; the 8-operation bound with
  `reconcile_truncated`; an invalid network setting skips reconciliation
  with a `reconcile_error` and never inspects),
  sell "all", sell completion net of gas, launch with a frozen salt and index
  completion, launch completion under an API outage, V4 pool-key and
  quote-representation tickets, positions bounds, the B1 chain allowlist on
  every flow (`assert_chain_calls_allowlisted`), bad/oversized/unknown
  bodies, backend failures, the write trace (invalid bodies leave a marker
  and no record; a parsed refusal creates an unbound record that the first
  valid write binds, even when the tuple was computable, so a corrected body
  keeps its id; refusals on a live or terminal record are appended to a
  bounded `refusals[]` with the status kept; a refusal on a bound record
  with nothing staged replaces its stale error; unrecorded-stage and
  live-entry refusals; unknown-token, prod and invalid-network refusals, and
  the record's `network` rewritten to the resolved one on the first stage;
  sell ownership via planned decimals; launch refusals; a no-op re-POST
  refreshes `last_write_ms`), and the secret boundary (no URL/key ever reaches a
  record, a marker or a response; no route file references the secret
  namespace).

No test contacts a network or a Bloom daemon.

## Decisions (fixed for day-1)

- **D1** V4 = quote yes, execute no (`execution_reason: "v4-follow-up"`). A
  write whose best venue is unsupported requires `allow_worse_venue: true` and
  surfaces `worse_than_best_pct`.
- **D2** Fee mirrored exactly: external-token buys through
  `MULTI_ROUTER.swapWithToll` / `swapWithTollV2` with the GROSS amount (fee
  banked atomically); pad tokens and all sells through SwapRouter02 /
  the multi router with no fee; exact-amount approvals only; `tollFor`
  cross-check on every external buy (mismatch → write refuses).
- **D3** `markets.json` = `scope=ours`, sort volume, 50 rows; any token is
  addressable via `tokens/<address>.json`.
- **D4** Slippage default 500 bps, accepted 50–5000; quotes expose `impact_pct`.
- **D5** Launch dev buy default 0, max 140 USDC, staged as approve → createToken.
- **D6** Writes gated by the USER's runtime setting `tolly_writes = "enabled"`
  (default disabled; any Bloom user can flip it — it is not a founder gate) plus
  `MAX_OP_USDC = 250` on buys and on the QUOTED USDC output of sells.
- **D13** Every write leaves a readable trace (`trace.rs`). Bloom delivers
  mounted Petal writes asynchronously and never returns the route's answer
  to the writer, so a refused write persists its outcome: the operation
  record is created unbound (a refusal never binds an `operationId`) or
  advanced to `failed` (or, when the record is live or terminal, the
  refusal is appended to its bounded `refusals[]` and the status kept), and
  a per-wallet `tolly/lastwrite/<wallet>` marker is written on every write,
  parsed or not. Agents read the marker first (`body_sha256`,
  `record_effect`), then the record it names. The route response is
  unchanged; the successful path stages exactly as before.
- **D7** Wallet address via `vfs_read("wallets/{wallet}/address")`.
- **D8** No logo pinning; the agent supplies a pinned `imageURI`.
- **D9** `max_fee_per_gas` / `max_priority_fee_per_gas` left `None` (the
  TxEngine sets fees and estimates gas); the `eth_call{from}` pre-flight is
  mandatory and a hard refuse (`-4 preflight-reverted`).
- **D10** Prod is a separate later manifest (`api.tollylabs.com`, no `/api`
  prefix). The stage `[[net.allow]] binding` only re-points the HTTPS
  authority; the path policy stays, so it is NOT a prod switch.
- **D11** No `/api/swaps` widening: buy/sell completion = balance delta of the
  output token (frozen at stage vs read after success); launch completion =
  `GET /api/tokens?creator=<wallet>&scope=ours` matched on `created_block`.
- **D12** `tx_confirm` is never called: under `agent_autonomy = under_policy`
  it would broadcast without a prompt. The owner confirms at
  `/bloom/wallets/<wallet>/chains/arc/outbox/pending/<outbox_id>/confirm`.
- **D14** Reconciliation runs from the READ of the route that staged the
  entry (`ops::route_read_side`, called by `buy.json`, `sell.json` and
  `launch.json`), never from the record: Bloom binds outbox inspection to
  the staging route (host fact below), and a side-effecting read would be
  unreadable on the mount (host fact below). `operations/[id].json` is a
  pure store projection (`account_read_spec`, caps `bloom:store` only, 5 s
  cache) with a `refresh` hint. Each route read reconciles at most
  `RECONCILE_MAX_OPS = 8` in-flight operations of its kind, newest first, out
  of the `recent` scan, persists every advance, and reports `reconciled[]` /
  `reconcile_truncated`; `recent` is projected after reconciliation. The
  write handlers reconcile only what they did before (their own record and
  `live_conflict`).

## Host facts the implementation relies on

- Mounted writes are asynchronous (verified on Bloom v0.2.1 / Ubuntu 24.04,
  2026-09-10): a `write()` to a Petal route on the NFS mount returns success
  to the writer (exit 0, empty stderr) before the route runs; the route's
  error is logged by the daemon as
  `WARN mount.adapter.async_command_outcome_deferred path=… error="…"` and
  nothing about it is visible on the mount. `bloom vfs write` returns the
  same error synchronously (exit 1). The SDK makes this unavoidable:
  `write_spec()` is `RouteSpec::writable().caps(..).ttl(None).write_async(true)`
  and every builder except `caps()` is crate-private, so a Petal cannot
  declare a synchronous writable route. Hence D13 and the "Read after every
  write" rule in AGENTS.md.
- Side-effecting reads are unreadable on the NFS mount (verified on Bloom
  v0.2.1 against the daemon and its source, 2026-09-11):
  `bloom-mount/src/adapter.rs` `should_render_for_attrs` returns false when
  `vfs.is_read_side_effecting(path)`, so GETATTR reports `st_size = 0` and
  `cat` short-circuits at 0 bytes; only `bloom vfs cat` (CLI/IPC) returns
  the body. The SDK's `chain_read_spec()` sets `side_effecting_read(true)`;
  `write_spec()`, `account_read_spec()`, `store_read_spec()` and
  `http_read_spec()` leave it false. Parameterized routes (`[wallet]`,
  `[id]`) get an install-time `side_effecting_read = true` ceiling, but the
  route's own spec narrows it at lookup (`bloom-petals/src/runner.rs`
  `petal_route_effective_metadata`), so a parameterized route with a
  non-side-effecting spec renders. Measured on the live daemon (petal
  v0.1.0): `wallets/main/buy.json` (write spec) stat 1577 bytes, `cat`
  works; `wallets/main/operations/buy-tolly-1.json` (then `chain_read_spec`)
  stat 0 / `cat` 0 bytes while `bloom vfs cat` returned 3012 bytes. Hence no
  route uses `chain_read_spec` (enforced by `check-route-architecture.sh`).
- Outbox inspection is bound to the STAGING ROUTE, not just the package
  (`bloom-daemon/src/lib.rs`): `tx_inspect` and `tx_confirm` compute
  `origin = petal_execution_origin(context)` = `ExecutionOrigin { petal_id,
  petal_digest = package_hash, route_id = context.route_id }` and answer
  `HostError::Denied("outbox entry was not staged by this trusted Petal")`
  when `entry.staged.resolved_execution_origin() != origin`. `route_id` is
  the route file, so an entry staged by `wallets/[wallet]/buy.json` can be
  inspected only from that route's handlers (read or write). Before D14 the
  record's read was always denied and every record degraded to `unknown`
  with "outbox inspection: denied". `tx_inspect` is read-only on the host
  side (test `daemon_petal_outbox_inspection_is_read_only_and_origin_bound`).
  A `Denied` from the staging route is now an anomaly (a rebuilt package
  hash, an entry from another route); `ops::classify_error` keeps it a
  non-regressing `unknown` with the note "outbox inspection: <reason> (entry
  not staged by this route?)".
- `bloom:chain` allowlist is exactly `eth_chainId`, `eth_getBalance`,
  `eth_getCode`, `eth_call` at the latest block. No receipts, no gas
  estimation, no block number are requested; funding uses a fixed native
  reserve (0.05 USDC) instead of a computed gas budget.
- `tx_stage` never returns an approval; identical pending requests (same
  to/value/data, unexpired) are de-duplicated by the host.
- `tx_inspect.state` is the receipt `outcome` (`success`|`reverted`) when a
  receipt exists, else `pending|sent|success|reverted|failed|cancelled`;
  `Denied`/`NotFound` map to a non-regressing `unknown` — and once a
  `success` outcome is recorded the entry is never inspected again. Every
  `tx_inspect` this Petal makes runs from the handlers of the route that
  staged the entry (the route's read via D14, its write via the record
  refresh and `live_conflict`).
- `tx_stage` errors reach the guest as `backend: stage EVM outbox: <engine
  error>`; the SDK's `host_err` turns any message containing "denied" into
  `HostStatus::Denied` (→ `policy-denied`), `valuation unavailable: …` is
  matched by wording (→ `valuation-unavailable`), everything else is a
  retryable `stage-failed`. The host does not de-duplicate a re-quoted swap
  (different calldata), hence the `stage_in_flight` marker.
- `store_put_new` on an existing key is reported as a message containing
  "already exists" (not a status); `ops::claim` treats it as "exists".
- Store keys: `tolly/ops/<wallet>/<id>` (records),
  `tolly/live/<wallet>/<kind>/<subject>` (the live-entry index that the M1
  check reads instead of scanning records) and `tolly/lastwrite/<wallet>`
  (the last-write marker, D13). All live in the `state` namespace; nothing
  secret is stored.
- Runtime settings read through `bloom:env`: `tolly_writes` (the write
  gate) and `tolly_network` (`stage` default; `prod` refused until D10).
- Route cache TTLs are the SDK's: quotes `http_read_spec(2_000)` (2 s, a
  pure read), `positions.json` and `operations/[id].json`
  `account_read_spec()` (5 s; the record is a pure store projection, D14),
  `operations/` listing and `wallets/` the 30 s store default; the writable
  routes (`write_spec`) are uncached, so every read of them reconciles.
- Wallet ids may contain `/` per the SDK grammar; this Petal additionally
  requires a single safe segment ≤ 64 bytes (store keys).
- The `[wallet]` param is the Bloom wallet id; `[usdc]`/`[amount]`/`[id]`
  params arrive with the `.json` suffix stripped.

## Deviations from the day-1 design document

- Status vocabulary drops `quoted`, `approval_pending` and `expired`
  (critique B2/B3): there is no approval object at stage time and expiry is
  reported by the host as `failed` → `error.code = "expired-or-dropped"`.
- The quote file carries no `block` (critique B1: `eth_blockNumber` is not
  allowlisted) and the record carries no `gas_estimate`/`gas_budget`.
- The fee "decimal invariance" (critique M3) holds exactly for inputs that are
  multiples of 500 raw; otherwise the 18-decimal fee exceeds the scaled 6-dec
  fee by less than one 6-dec unit. The Petal computes the fee once in the
  ERC-20 unit the router charges and scales `net` for native-quote V4 quoting;
  `fee::tests` pins both facts.
- `operations/[id].json` declares only `bloom:store`: launch completion
  (the creator index, D11) and swap completion (balance deltas) are read by
  the staging route's read, which already holds `bloom:http`, `bloom:chain`
  and `bloom:tx.outbox` (D14).
- Sell completion is reported as `balance_delta_net_of_gas`: on Arc the
  ERC-20 USDC view is the gas balance, and the receipt exposes no `gas_used`.
- A V4 venue whose quote representation (native 18 / ERC-20 6) differs from
  the API's `quoteDecimals` is ticketed `quote-decimals-mismatch` (the site's
  `venueMatchesQuoteDecimals`), and one whose `currency0/1` are not the sorted
  `(token, quote)` pair is ticketed `v4-pool-key-mismatch`; neither can be
  `best`.
- Calldata golden vectors are produced with Python `eth_abi` + Foundry `cast`
  rather than the repo's viem (no `node_modules` in the worktree); the inputs
  are the frontend's.
- V2 fee recovery (`getAmountsOut` search) is not implemented: a V2 venue
  with `feeBps: null` is listed as `recoverable_v2` but `v2-fee-unsolved`.

## Not implemented (follow-ups)

- V4 execution (TollyV4Router / UniversalRouter / Permit2 paths).
- Prod manifest release; `markets/all.json` (scope=all with the spam filter).
- Runtime smoke on a daemon from this environment: none here. On a Bloom
  v0.2.1 host the petal installs and its read routes work on the mounted
  VFS (status/markets/tokens/quote verified 2026-09-10; the 2026-09-11 stat
  measurements above were taken on petal v0.1.0); the write path's
  asynchronous delivery is what D13 answers, and the two host facts above
  are what D14 answers. A mounted smoke of the post-write flow after this
  release (write → read `buy.json` → `reconciled[]` → read the record, and
  `stat` of the record showing a non-zero size) is still to do.
- Release workflow (`release-petal.yml`, `expected-route-count: 21`) and the
  GitHub extraction (`git subtree split -P petals/tolly`).
