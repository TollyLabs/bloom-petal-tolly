# tolly — Bloom walletFS Petal for TOLLY (Arc)

A Bloom Petal that exposes the TOLLY launchpad and DEX as virtual files:
market discovery and token detail from the public TOLLY production API
(`https://api.tollylabs.com`, the index behind [tollylabs.com](https://tollylabs.com)),
best-execution quotes across a token's venues from on-chain simulations, and
buy / sell / launch operations staged into the owner's Bloom outbox. The Petal
never signs and never broadcasts: the owner confirms every transaction in
Bloom with their passkey. Agent-facing semantics live in
[AGENTS.md](AGENTS.md); the rest of this file is for developers.

TOLLY runs on Arc mainnet (chain id 5042, Bloom chain key `arc`). Every
staged transaction spends real USDC once the owner confirms it.

## Quickstart (Bloom owner)

Requires a running Bloom (v0.2.1 or later) with the `arc` chain configured
and a passkey wallet (`main` below). The mount root is the owner's mount point,
`~/bloom` on a default Linux install.

**1. Install the Petal.** From the release archive:

```sh
bloom petals install ./tolly-v0.2.0.petal.tar.gz
```

The archive and its `SHA256SUMS` are attached to each GitHub release of
[TollyLabs/bloom-petal-tolly](https://github.com/TollyLabs/bloom-petal-tolly).
Bloom v0.2.1 installs source repositories only from its own `bloom-directory`
organisation, so `bloom petals install <github-url> --ref <tag>` works for this
Petal once Bloom lists it there; until then install the release archive.

Nothing else to configure: the Petal reads production by default. Check it:

```sh
cat ~/bloom/petals/tolly/status.json     # network "prod", api_base "https://api.tollylabs.com", chain_id 5042
cat ~/bloom/petals/tolly/markets.json
```

**2. Enable writes** (reads work without this; every buy/sell/launch is
refused with `writes-disabled` until the owner opts in). In
`~/.bloom/config.toml`:

```toml
[petals.runtime.tolly.values]
tolly_writes = "enabled"
```

then restart the Bloom Machine service (`systemctl --user restart
bloom-machine.service` on a systemd install) and confirm
`status.json` shows `"writes_enabled": true`.

**3. Find the installed package hash.** Bloom identifies a Petal build by
its package hash; a fresh wallet policy allows no Petal packages and no
destinations, so a staged entry cannot be confirmed until the owner
allows both.

```sh
python3 -c 'import json,os; print(json.load(open(os.path.expanduser("~/.bloom/petals/store/owners/tolly.json")))["hash"])'
```

The release notes carry the archive's `package_hash` from `petal package`
for cross-checking; the hash Bloom reports for the INSTALLED package is the
one that counts.

**4. Update the wallet policy** (destinations + package). Start from the
current policy (`cat ~/bloom/wallets/main/policy.json`) and set exactly
these fields, keeping anything else Bloom already put there:

```json
{
  "wallet_id": "main",
  "allowed_destinations": [
    { "chain": "arc", "destination": "0x3600000000000000000000000000000000000000" },
    { "chain": "arc", "destination": "0x53bf6b0684ec7ef91e1387da3d1a1769bc5a6f77" },
    { "chain": "arc", "destination": "0xcad7ee36ac193bf2eddb7b3e2736c5bdb8269c8b" },
    { "chain": "arc", "destination": "0xf28c138a39c234554c847dbef467b073ddbd7451" }
  ],
  "allowed_petal_packages": ["<INSTALLED_TOLLY_PACKAGE_HASH>"],
  "required_verifiers": [],
  "maximum_approval_lifetime_ms": 604800000
}
```

The four destinations are everything this Petal ever stages a transaction
to: USDC (`0x3600…`, exact-amount approvals), Uniswap SwapRouter02
(`0x53bf…`, pad-token swaps), TollyPad (`0xcad7…`, launches) and the TOLLY
multi router (`0xf28c…`, external-token swaps). Replace the placeholder with
the hash from step 3 (keep any other package hashes you rely on). Then run
the policy ceremony:

```sh
cp tolly-policy.json ~/bloom/wallets/main/policy.json      # 1. propose: Bloom answers "permission denied" BY DESIGN
cat ~/bloom/wallets/main/policy-updates/latest/status.json  # 2. read ceremony_url, open it in a browser, approve with the passkey (5 min window)
cp tolly-policy.json ~/bloom/wallets/main/policy.json      # 3. commit the SAME bytes: accepted; policy_version advances
```

**Every reinstall of this Petal changes the package hash** (a new build, a
new version, even the same archive rebuilt) and needs this policy update
again; the Petal's private store also starts empty (see "Freshness" in
AGENTS.md). Without it, confirming a staged entry fails with
`POLICY_APPROVAL_REQUIRED` and Bloom auto-stages a packages-only policy
update of its own.

**5. Trade.** The agent writes a body to `wallets/main/buy.json` (or
`sell.json`, `launch.json`) and reads the same file back (AGENTS.md "Read
after every write"). Each accepted write stages ONE transaction in the
outbox; the owner confirms it per transaction with the passkey:

```sh
printf 'y\n' > ~/bloom/<confirm_path>       # first write: denied (EACCES) and a ceremony is opened
cat ~/bloom/wallets/main/chains/arc/outbox/pending/<outbox_id>/ceremony.json   # ceremony_url + expiry
                                             # open the URL, approve with the passkey (about 10 minutes)
printf 'y\n' > ~/bloom/<confirm_path>       # second write: broadcast
```

`confirm_path` is on the operation record and is RELATIVE to the mount
root. A buy of an external token is two entries (approve, then swap: re-POST
the same body after the approve mined); a pad-token buy or a sell without an
allowance is the same. To abandon a pending entry write `cancel` into the
same `confirm` file. Start with a 1 USDC buy.

## Layout

```
petal.toml               package manifest: caps ceiling, net.allow, store policy
petal-build.toml         route build config; SDK pinned by full commit SHA; no extra crate deps
route/Cargo.toml         shared route crate (same SDK pin)
route/src/
  constants.rs           GENERATED from the frontend sources (scripts/gen-constants.mjs)
  policy.rs              day-1 limits and the write gate
  abi.rs amount.rs fee.rs           pure encoders and arithmetic
  api.rs                 fixed API targets (the production host) + projections (venuesForToken port)
  chain.rs               the four allowlisted bloom:chain reads
  quote.rs               per-venue quoting, ranking, protection
  ops.rs                 operation record + state machine + reconciliation (run from the staging route's read)
  trace.rs               every write leaves a readable trace (record refusals, last-write marker)
  tx.rs swap.rs launch.rs positions.rs wallet.rs   the write/step flows
  host.rs                the only host seam; fake_host.rs under cfg(test)
  route_tests.rs         fake-host tests of every route flow (cfg(test))
route/files/             21 route files, one component each (see AGENTS.md table)
route/tests/fixtures/    production API captures, calldata golden vectors
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
petal package --root . --out dist/tolly-v0.2.0.petal.tar.gz
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
  API targets: every URL is built on `https://api.tollylabs.com` with no
  `/api` prefix, and `petal.toml` declares exactly that host; the
  production captures (`prod-health.json`, `prod-tokens-ours.json`,
  `prod-tokens-ours-page.json`, `prod-token-tolly.json`,
  `prod-token-barc.json`, 2026-09-11) project through `status.json`,
  `markets.json` and `tokens/<address>.json`.
- Route flows against the fake host (`route/src/route_tests.rs`): status,
  markets, token detail, buy/sell quotes (V4 best but unsupported, QuoterV2
  revert, sell normalisation), the buy walk (writes disabled → -2 and a
  `failed/writes-disabled` record plus `last_write`, cap → -3,
  approve-then-swap with gross `swapWithToll` and a fresh floor, bound-id
  mismatch → -3, venue pin rules, stage denial and error classification,
  persist failure after stage → `stage_in_flight` → refuse → acknowledge,
  claim race, pending dedupe, completion by balance delta, zero-delta buys
  stay `confirmed`, a forgotten outbox entry never regresses a receipt, a
  staged record's `confirm_path` is mount-relative with `confirm_path_note`
  and `cancel_hint` beside it, nothing in the record is `/bloom`-rooted, and
  the notes leave with the path once the entry is broadcast),
  reconciliation from the staging route (`operations/<id>.json` is a pure
  store projection: no `tx_inspect`, chain, HTTP or save, and a `refresh`
  hint; a `buy.json` read reconciles a staged buy to `confirmed`/`completed`,
  persists it and lists it in `reconciled[]` with `changed: true`; a sell is
  not reconciled by the buy route; the 8-operation bound with
  `reconcile_truncated`), the API host (production is the only network:
  no runtime setting selects one, every read of a write flow goes to
  `api.tollylabs.com` and the record carries `network: "prod"`),
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
  live-entry refusals; unknown-token and unreachable-host refusals, and
  the refused record binding on the first stage;
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
- **D10** One network: the Petal reads `https://api.tollylabs.com` (no
  `/api` prefix) and nothing else. The host is declared in `petal.toml` as
  the single `[[net.allow]]` rule (`tolly-prod`) with `GET` and the exact
  paths it serves (`/health`, `/tokens`, `/token/*`). Bloom matches a fetch
  against the URL's host, method and PATH only (query strings are not part
  of the rule; `*` is one path segment). The rule's `binding` lets an
  operator re-point its HTTPS authority only; methods and paths stay. The
  `Network` type is the only source of a base URL; no runtime setting
  selects a host, and a test pins the manifest to the host the route
  reaches.
- **D11** No `/swaps` widening: buy/sell completion = balance delta of the
  output token (frozen at stage vs read after success); launch completion =
  `GET /tokens?creator=<wallet>&scope=ours` matched on `created_block`.
- **D12** `tx_confirm` is never called: it would gain nothing and could
  only lose the simulation. Outbox confirms are passkey-per-transaction by
  construction on Bloom v0.2.1 (host fact below): every confirm mints a
  single-use Exact approval bound to {bloom-machine, transaction.confirm}
  and requires the owner's passkey, and no daemon setting (there is no
  gating `agent_autonomy`, and `bloom_proto::Policy` has no config loader)
  turns that into an unprompted broadcast. The one thing a `bloom:tx`
  `tx_confirm` from the Petal could change is with
  `acknowledge_warnings = true`, which would bypass simulation. The owner
  confirms by writing to the entry's confirm file, `confirm_path`
  (`wallets/<wallet>/chains/arc/outbox/pending/<outbox_id>/confirm`,
  RELATIVE to the Bloom mount root, host fact below); `confirm_path_note`
  and `cancel_hint` travel with it on the record.
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
- The Petal's private store is namespaced by PACKAGE hash
  (`bloom-petals/src/vm.rs` passes the package hash as the store's
  `petal_hash`; `private_store.rs` `store_is_namespaced_by_hash`): every
  new build starts with an empty store. Observed 2026-09-11 when v0.1.2
  replaced v0.1.0: `operations/` was empty and the v0.1.0 record
  `buy-tolly-1` was gone while its outbox entry `0001-08786` stayed
  pending (and is not inspectable by the new package). Documented in
  AGENTS.md "Freshness"; a store migration across package hashes is not
  available to a Petal.
- Directory listings report size 0 for parameterized files until their
  first lookup: the runtime metadata that narrows `side_effecting_read`
  is evaluated on LOOKUP of the concrete path, and READDIR entries come
  from the route index (size 0). Measured 2026-09-11 on v0.1.2: after
  `ls -l operations/` a never-opened record `stat`ed 0 bytes; opening it by
  exact path rendered 1713 bytes and the listing then showed 1713. The
  mount uses `actimeo=0`, so nothing stale is cached on the client side.
- Cancelling a pending outbox entry: the mount refuses writes to
  `…/outbox/pending/<id>/cancel` and `…/replace` (`bloom-mount/src/adapter.rs`
  `mount_write_path_uses_wallet_signer` → EPERM) and `bloom vfs write` to
  the same path answers "permission denied"; writing the word `cancel` into
  `…/pending/<id>/confirm` cancels (`handlers_wallets.rs`, "confirm —
  broadcast" arm) and moves the entry to `outbox/failed/`. Verified
  2026-09-11: entry `0002-71268` cancelled that way; the next `buy.json`
  read reconciled the record to `failed` / `cancelled` / `retryable: true`.
- The Bloom mount is NOT at `/bloom`. `mount_path = "/bloom"` in
  `~/.bloom/config.toml` is informational only (bloom-proto `config.rs`
  reads it into the config and nothing mounts there); the kernel mount is
  wherever the owner's fstab puts it, `~/bloom` (`/home/<user>/bloom`) on
  the reference Bloom v0.2.1 / Ubuntu 24.04 install (verified 2026-09-11).
  Hence every path this Petal emits (`confirm_path` on the record and in
  `txs[]`, the outbox directory in the `unrecorded-stage` message, the
  wallet-id refusal) is RELATIVE to the mount root and every `confirm_path`
  carries a `confirm_path_note` saying so; an agent that follows a literal
  `/bloom/...` gets ENOENT. Documented in AGENTS.md "Paths".
- Outbox confirms are passkey-per-transaction by construction: on Bloom
  v0.2.1 every outbox confirm mints a single-use Exact approval bound to
  {bloom-machine, transaction.confirm} and requires the owner's passkey
  (bloom-tx `tx_engine.rs` `triad_sign_evm_payload`, ~2864-2875 and
  ~3981-4006). The local autonomy branch there is non-gating (~3527-3547)
  and `bloom_proto::Policy` has no config loader at all, so a daemon setting
  `agent_autonomy = "under_policy"` does not exist as a gate and could not
  let `tx_confirm` broadcast without a prompt (an earlier D12 wording
  claimed it could; that was wrong). Calling `bloom:tx` `tx_confirm` from
  the Petal therefore gains nothing, and with `acknowledge_warnings = true`
  it would bypass simulation. Hence D12: the owner confirms by writing to
  the entry's confirm file.
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
- Runtime setting read through `bloom:env`: `tolly_writes` (the write
  gate). No setting selects a network (D10).
- A fresh Bloom wallet's policy (`wallets/<w>/policy.json`) has empty
  `allowed_destinations` and `allowed_petal_packages`. An empty destination
  set denies every recipient (the plan of a staged entry shows `[Deny]
  allowlists.recipients`), and a package hash missing from
  `allowed_petal_packages` makes the confirm fail with
  `POLICY_APPROVAL_REQUIRED` while Bloom auto-stages a packages-only policy
  update. Hence the Quickstart's single policy update covering both, and the
  note that every reinstall (new package hash) needs it again. Verified live
  2026-09-11 on Bloom v0.2.1 (policy_version 3 for wallet `main`: the four
  destinations + the installed package; the next staged buy showed `[Pass]
  allowlists.recipients`).
- A policy update is a two-write ceremony: the first `cp` onto
  `wallets/<w>/policy.json` is answered "permission denied" and creates
  `wallets/<w>/policy-updates/pending/<op>/` (`latest/status.json` carries
  `ceremony_url`, `expiry_ms`, `status`); after the passkey ceremony the
  same bytes are written again and accepted. Outbox confirms follow the
  same shape per transaction: the first write of `y` into an entry's
  `confirm` is denied and the entry's `ceremony.json` carries the
  `ceremony_url`; the second write broadcasts.
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
- Live smoke of v0.2.0 against production (a 1 USDC buy through the
  production API on Bloom v0.2.1, then a confirmed approve + swap under the
  owner's real policy). v0.1.2/v0.1.3 were smoked against the team's
  internal index only; the production API was verified 2026-09-11
  (`/health` 200, `/tokens` 200, `/token/<addr>` 200, `/api/...` 404, the
  JSON fields the fixtures show, same pad and chain).
- `markets/all.json` (scope=all with the spam filter).
- Mounted smoke of v0.1.2 on Bloom v0.2.1 (Ubuntu 24.04, wallet `main`,
  Arc, 2026-09-11): a refused write (`amount_usdc: 300`) left
  `last_write` (`outcome: refused`, `cap-exceeded`, `record_effect:
  created`) and a `failed`/`retry` record readable on the mount (1713
  bytes by exact path); an accepted 1 USDC TOLLY buy left `last_write`
  `accepted`, `reconciled[]` reported `staged` / `approve` /
  `confirm_in_bloom` and the record (3401 bytes on the mount) carried the
  outbox id, `confirm_path` and the plan (whose policy section showed the
  wallet's `[Deny] allowlists.recipients` — an empty `allowed_destinations`
  denies every recipient until the owner's policy update); cancelling that
  entry through its `confirm` file reconciled the record to `failed` /
  `cancelled` / `retryable: true` on the next `buy.json` read. Not yet
  exercised live: a confirmed approve + swap (needs the wallet policy's
  destinations allowlist), launches, sells.
- Release workflow (`release-petal.yml`, `expected-route-count: 21`) and the
  GitHub extraction (`git subtree split -P petals/tolly`).
