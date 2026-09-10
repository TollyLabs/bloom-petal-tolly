# TOLLY Petal — agent operating contract

This Petal exposes TOLLY (launchpad + DEX on Arc, chain id 5042, chain key
`arc`) as files under `/bloom/petals/tolly/`. It reads the TOLLY public API
(stage host) and the chain through Bloom, and it STAGES transactions into the
wallet owner's Bloom outbox. It never signs, never broadcasts, never calls
`confirm`. The owner confirms every transaction in Bloom.

**Money is real.** "Stage" is only the API host; both stage and prod index Arc
mainnet. Every buy, sell and launch you stage spends the owner's USDC once the
owner confirms it.

## Paths

| Path | Read | Write |
|---|---|---|
| `status.json` | API health, network, chain, `writes_enabled`, constants digest, `pad_matches_constants` | — |
| `markets.json` | TOLLY launches by 24h volume (50 rows); `degraded:true` means unknown, not empty | — |
| `tokens/<address>.json` | identity, `provenance` (`pad`/`external`), `venues[]` with `execution` support, quote paths. Any lowercase address works, listed or not | — |
| `quote/<address>/buy/<usdc>.json` | best-execution BUY quote at a USDC size (e.g. `25`, `0.5`) | — |
| `quote/<address>/sell/<amount>.json` | best-execution SELL quote at a token size | — |
| `wallets/<wallet>/buy.json` | body schema, limits, recent buys | BuyRequest |
| `wallets/<wallet>/sell.json` | body schema, limits, recent sells | SellRequest |
| `wallets/<wallet>/launch.json` | body schema, pad address, limits, recent launches | LaunchRequest |
| `wallets/<wallet>/operations/<operationId>.json` | the operation record, reconciled on every read | — |
| `wallets/<wallet>/positions.json` | native + ERC-20 USDC and the tokens this wallet's operations touched | — |

`<wallet>` is a Bloom wallet id (the directory name under `/bloom/wallets/`),
never a `0x` address. `<address>` is a lowercase `0x` token address.

## Read before you write

1. `status.json` — `writes_enabled` must be `true`; otherwise every write
   returns `-2 writes-disabled`. It is the OWNER's runtime setting
   (`tolly_writes = "enabled"` under `[petals.runtime.tolly.values]`), not a
   TOLLY-side switch. Also check `pad_matches_constants` and `network`: the
   optional runtime setting `tolly_network` selects the API host; only
   `stage` is enabled in this build (`prod` answers `-2` until its own
   manifest release).
2. `tokens/<address>.json` — look at `provenance` and each venue's
   `execution`. Day-1 executes Uniswap V3 pools (pad tokens through
   SwapRouter02, external tokens through the TOLLY multi router) and V2 pairs
   with a solved fee (external tokens, TOLLY multi router). V4 pools and custom
   curves are QUOTED for honesty but `execution: "unsupported"`
   (`v4-follow-up`, `custom-curve`).
3. `quote/<address>/buy/<usdc>.json` — read `best`, `best_executable`,
   `worse_than_best_pct`, `impact_pct` and `warnings`. If `best` is not
   executable, a write needs `allow_worse_venue: true` and will execute on
   `best_executable`.

## Bodies (max 4 KiB, unknown fields rejected)

BuyRequest → `wallets/<wallet>/buy.json`

```json
{ "operationId": "buy-moss-001", "token": "0x…", "amount_usdc": "25",
  "slippage_bps": 500, "venue": null, "min_out_raw": null, "allow_worse_venue": false,
  "acknowledge_unrecorded_stage": false }
```

- `amount_usdc` ≤ 250 (`max_op_usdc`). `slippage_bps` 50–5000, default 500.
- `acknowledge_unrecorded_stage` (all three bodies) is only ever needed after
  an "Unrecorded stage" (below); leave it out otherwise.
- External-token buys pay TOLLY's 0.2% interface fee, banked on-chain by the
  router in the same transaction (`interface_fee.raw` in the quote and
  `plan.interface_fee_raw` in the record). Pad tokens and all sells pay 0.

SellRequest → `wallets/<wallet>/sell.json`

```json
{ "operationId": "sell-moss-001", "token": "0x…", "amount": "1234.5",
  "slippage_bps": 500, "venue": null, "min_out_raw": null, "allow_worse_venue": false }
```

- `amount` is a decimal token amount or `"all"` (the balance is frozen at the
  first stage). Sells are capped by the QUOTED USDC output: ≤ 250 USDC.

LaunchRequest → `wallets/<wallet>/launch.json`

```json
{ "operationId": "launch-moss", "name": "Moss Coin", "symbol": "MOSS",
  "meta": { "imageURI": "ipfs://…", "website": "", "twitter": "", "telegram": "" },
  "dev_buy_usdc": "0" }
```

- `imageURI` is required and must already be pinned (this Petal does not pin
  logos). Each meta field ≤ 512 bytes. `dev_buy_usdc` default 0, max 140.
- The token address is never predicted; it appears in `result.token` after the
  launch mines and the TOLLY index lists it.

## What a write means — and does not mean

A successful write (`Write`, no body) means: the Petal validated the body,
re-quoted, checked funding and allowances, simulated the exact calldata from
the wallet, and staged AT MOST ONE transaction in Bloom's outbox — or made no
change because the operation is already live or terminal. Then read
`operations/<operationId>.json`.

A write never means broadcast, mined, filled, or launched. Only the record
says that, and only from host and chain evidence.

`operationId` is the idempotency key. It is bound to the economic tuple
(buy: token + amount; sell: token + amount; launch: name + symbol + meta + dev
buy). Re-POSTing with a different tuple returns `-3 operationId already
bound`; use a new id. Execution parameters (`slippage_bps`, `venue`,
`min_out_raw`, `allow_worse_venue`) may change between attempts and are
recorded per attempt in `txs[].attempt_params`; `request` in the record is
the body of the attempt that last staged (a no-op re-POST leaves it alone).

## Lifecycle (`status` / `step` / `next_action`)

```
created ──stage──▶ staged ──owner confirms──▶ broadcast ──mined ok──▶ confirmed ──evidence──▶ completed
                     │                                        │
                     │ (approve step confirmed) next_action: repost ──▶ POST again, the swap/create is staged
                     ▼
                  failed{retryable} ──POST again──▶ re-quote, re-stage the same step (old attempt marked superseded)
```

| status | meaning | next_action |
|---|---|---|
| `created` | id claimed, nothing staged yet | `repost`; `inspect` when `stage_in_flight` is set (see "Unrecorded stage") |
| `staged` | one entry pending in Bloom's outbox | `confirm_in_bloom` — the owner writes to `confirm_path` (`/bloom/wallets/<wallet>/chains/arc/outbox/pending/<outbox_id>/confirm`) |
| `broadcast` | sent, no receipt yet | `wait` — read the record again |
| `confirmed` | mined successfully | step `approve`: `repost` (POST the same body to stage the swap/createToken). step `swap`/`create`: `wait` for completion evidence |
| `completed` | domain evidence recorded in `result` | `none` |
| `failed` | `error.code`, `error.message`, `error.retryable` | `retry` (re-POST) when retryable, else `none` |
| `unknown` | Bloom no longer lets the Petal inspect the outbox entry | `inspect` — look at the wallet's outbox in Bloom; nothing is re-staged |

`step` ∈ `approve | swap | create`. `txs[]` keeps every attempt (audit):
`outbox_id`, `confirm_path`, `outbox_state`, `tx_hash`, `outcome`,
`block_number`, `revert_reason`, `plan_md` (Bloom's rendered plan, truncated),
`superseded`.

Completion evidence (`result.method`):
- buy: `balance_delta` — the token's `balanceOf(wallet)` after the swap minus
  the balance frozen when it was staged (`amount_out_raw`, `amount_out_human`).
  Receipt logs are not available to Petals. A mined buy whose delta is zero
  is NOT promoted: it stays `confirmed` with a `note` (the wallet moved the
  token meanwhile, or something is wrong — look before re-reading).
- sell: `balance_delta_net_of_gas` (`net_of_gas: true`) — the same delta on
  the ERC-20 USDC view, which on Arc IS the balance that pays gas, so
  `amount_out_raw` is the USDC received minus the gas the sell paid (and
  minus anything else the wallet spent in between). The receipt carries no
  `gas_used`, so it cannot be corrected. A sell completes even at a zero delta.
- launch: `creator-index-block` — the TOLLY index row for this wallet whose
  `created_block` equals the receipt block (`result.token`, `result.pool`);
  `creator-index-identity` when the block is unavailable. Until the index
  lists it — or while the TOLLY API is unreachable — the record stays
  `confirmed` with a `note` (`completion evidence unavailable: …`); the read
  never fails because evidence is missing.

A recorded receipt is final: once `txs[].outcome` is `success`, the entry is
not inspected again and the host forgetting it (pruned outbox, `Denied`)
cannot regress the record to `unknown`.

## Retry rules

- `staged`, `broadcast`: a re-POST is a no-op refresh. Never expect a second
  entry; if the pending entry is stale (the quote is old — SwapRouter02 and the
  TOLLY routers have no deadline, only `amountOutMinimum` protects you) ask the
  owner to cancel it in Bloom (`…/outbox/pending/<id>/cancel`) and re-POST
  after it reports `failed`/`cancelled`.
- `failed` with `retryable: true` — exactly these codes: `reverted`,
  `expired-or-dropped`, `cancelled`, `stage-failed`, `quote-unavailable`,
  `fee-check-unavailable`, `venue-changed`, `venue-unsupported`,
  `better-venue-unsupported`, `below-requested-floor`, `insufficient-funds`,
  `cap-exceeded` (sell: the quoted USDC output exceeded 250 — sell less),
  `preflight-reverted`, `provenance-mismatch`. Re-POST: the Petal re-quotes
  and re-stages the same step; the old attempt stays in `txs[]` as
  `superseded`.
- `venue-changed`: the executable winner is no longer the planned venue.
  Re-POST with `venue` set to the new winner (or keep the old one with
  `allow_worse_venue: true`).
- `venue-unsupported`: the pinned venue is not executable day-1; pin another.
- `better-venue-unsupported`: the best venue is V4/custom (not executable
  day-1). Re-POST with `allow_worse_venue: true` to accept the executable venue
  at `worse_than_best_pct`.
- `failed` with `retryable: false` — exactly these codes: `policy-denied`,
  `valuation-unavailable` (the owner's wallet policy blocked the stage),
  `fee-mismatch` (the router's `tollFor` disagrees with the mirrored fee
  rule), `execution-plan-invalid` (the venue data cannot be executed as
  planned). Do not retry blindly; report it.
- `-2 … another operation … still has a pending outbox entry`: one live entry
  per (wallet, kind, token) at a time. Wait for or cancel the other one.

### Unrecorded stage

A write that staged but could not persist the record returns `-4` with the
`outbox_id` in the message, and the record keeps `status: created`,
`next_action: inspect` and a `stage_in_flight` marker (`step`, `to`,
`data_sha256`, `staged_ms`). While the marker is set, every re-POST of this
operation answers `-2 unrecorded-stage`, and any other operation for the
same (wallet, kind, token) answers `-2` too. Nothing is staged again on its
own: the re-quote would produce different calldata, and two live entries for
one intent is the failure this Petal is built to prevent.

To proceed: inspect the wallet's outbox in Bloom
(`/bloom/wallets/<wallet>/chains/arc/outbox/`), confirm or cancel the entry
the marker describes, then re-POST the same body with
`acknowledge_unrecorded_stage: true`. The marker moves to
`unrecorded_stages[]` (audit) and the step is staged afresh. If the entry
was confirmed and mined, the money moved even though `txs[]` never listed it
— account for it before staging again.

### Listing freshness

`operations/` (the directory listing) is served with the host's 30 s cache:
a new operation may take up to 30 s to appear in `ls`, though its file is
readable immediately. `positions.json` is cached for 5 s. Listings load at
most 1000 records per wallet (`scan_truncated: true` in `recent` /
`bounds.scan` when more exist).

## Errors

`-1` not found (unknown token / operation), `-2` denied (writes disabled,
prod not enabled, host/policy denial, live-entry conflict, unrecorded stage),
`-3` invalid input (bad body, cap, bound id, venue rules, funds), `-4` backend
(API/chain failure, pre-flight revert, fee mismatch, a stage whose record
could not be written). Messages are sanitized; they never carry RPC
URLs or keys.

## Safety

- Never call anything else to "speed up" an operation: no `confirm`, no
  broadcast, no raw calldata. The owner confirms in Bloom.
- Keep sizes small on young pools; use `impact_pct` and `liquidity_usdc` from
  the quote before lowering `slippage_bps` below the default.
- Fee-on-transfer tokens (`supports_fot: true`) are refused on V2.
- Bloom's wallet policy can deny or hard-fail a stage (MEV guard, USD caps
  without a price for Arc tokens). Test a 1-USDC buy on a throwaway wallet
  under the owner's real policy before anything larger.
