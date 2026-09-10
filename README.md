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
petal-build.toml         route build config; SDK pinned by full commit SHA
route/Cargo.toml         shared route crate (same SDK pin)
route/src/
  constants.rs           GENERATED from the frontend sources (scripts/gen-constants.mjs)
  policy.rs              day-1 limits and the write gate
  abi.rs amount.rs fee.rs           pure encoders and arithmetic
  api.rs                 fixed API targets + projections (venuesForToken port)
  chain.rs               the four allowlisted bloom:chain reads
  quote.rs               per-venue quoting, ranking, protection
  ops.rs                 operation record + state machine + reconciliation
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
petal package --root . --out dist/tolly-v0.1.0.petal.tar.gz
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
  revert, sell normalisation), the buy walk (writes disabled → -2, cap → -3,
  approve-then-swap with gross `swapWithToll` and a fresh floor, bound-id
  mismatch → -3, venue pin rules, stage denial, persist failure after stage,
  pending dedupe, completion by balance delta), sell "all", launch with a
  frozen salt and index completion, positions bounds, bad/oversized/unknown
  bodies, backend failures, and the secret boundary (no URL/key ever reaches a
  record or response; no route file references the secret namespace).

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

## Host facts the implementation relies on

- `bloom:chain` allowlist is exactly `eth_chainId`, `eth_getBalance`,
  `eth_getCode`, `eth_call` at the latest block. No receipts, no gas
  estimation, no block number are requested; funding uses a fixed native
  reserve (0.05 USDC) instead of a computed gas budget.
- `tx_stage` never returns an approval; identical pending requests (same
  to/value/data, unexpired) are de-duplicated by the host.
- `tx_inspect.state` is the receipt `outcome` (`success`|`reverted`) when a
  receipt exists, else `pending|sent|success|reverted|failed|cancelled`;
  `Denied`/`NotFound` map to a non-regressing `unknown`.
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
- `operations/[id].json` also declares `bloom:http` (launch completion reads
  the creator index, D11).
- Calldata golden vectors are produced with Python `eth_abi` + Foundry `cast`
  rather than the repo's viem (no `node_modules` in the worktree); the inputs
  are the frontend's.
- V2 fee recovery (`getAmountsOut` search) is not implemented: a V2 venue
  with `feeBps: null` is listed as `recoverable_v2` but `v2-fee-unsolved`.

## Not implemented (follow-ups)

- V4 execution (TollyV4Router / UniversalRouter / Permit2 paths).
- Prod manifest release; `markets/all.json` (scope=all with the spam filter).
- Runtime VFS smoke tests (`bloom vfs ls/cat/write`) and `bloom petals
  build/install`: no daemon in this environment. Before the first live write
  verify once that `-2 writes-disabled` is returned synchronously through
  `bloom vfs write` (`write_spec` implies `write_async`).
- Release workflow (`release-petal.yml`, `expected-route-count: 21`) and the
  GitHub extraction (`git subtree split -P petals/tolly`).
