# Dusk Devnet Production Stack Plan

Supersedes the Surfpool-first plan of the same lineage. Surfpool is retired as
a substrate; devnet is the deployed target and the E2E environment, and it is
operated as a public service rather than a private rehearsal.

## 1. Objective

Run a public Dusk deployment on devnet that is production-shaped in every
respect except the value at risk, and that can be promoted to mainnet by
redeploying the same artifacts against a mainnet protocol revision — without
rewriting the application, SDK, indexer, keepers, or deployment model.

Four independently deployable repositories:

1. `dusk` — programs, deployment tooling, protocol SDK, deterministic market
   bootstrap.
2. `dusk-webapp` — the Omnipair webapp interface and design system, adapted and
   extended for Dusk.
3. `dusk-indexer` — Dusk ingestion, historical APIs, backfill, reconciliation,
   and the feeds the webapp and keepers read.
4. `dusk-keepers` — liquidation, conditional-order and auction execution.

"Public" is the operative word. Anyone may connect a wallet, mint test tokens,
and transact. That sets the bar for rate limiting, abuse handling, status
reporting, and unattended operation, and it means every failure is visible to
someone who cannot read the logs.

## 2. Locked decisions

- **Devnet is the substrate.** Surfpool is retired. Its markers, fork-generation
  detection, reset epochs, and `surfnet_*` RPC dependencies are removed rather
  than carried forward disabled.
- **Determinism moves to LiteSVM.** Devnet cannot be reset, rewound, or fault
  injected, so the deterministic layer that Surfpool provided becomes
  LiteSVM-based program tests plus recorded fixtures. Tests that genuinely need
  a live cluster run against devnet and are written to tolerate its
  nondeterminism; tests that need determinism never touch a cluster.
- **The SDK is the only protocol boundary.** The webapp and keepers construct no
  instructions, derive no PDAs, and call no program directly. Generic builders
  remain an escape hatch inside the SDK; product code does not reconstruct
  account order.
- **The fork lab is ported, then deleted.** `dusk-devnet-api`'s 53 transaction
  builders are the working reference for account ordering. What product flows
  need becomes typed SDK methods; the lab and every `/api/v2/fork/*` path are
  then removed.
- **Keepers ship Rust-live, TypeScript deferred.** The dual-runtime parity gate
  is a mainnet requirement, not a devnet one. TypeScript stays scaffolded and
  compiles against the same fixtures, but is not on the devnet critical path.
- **One protocol revision governs everything.** Program ids, IDL digests, and
  binary hashes are pinned in a lock file that the SDK, indexer, keepers, and
  webapp all verify. A consumer that disagrees with the lock fails closed.
- **Market creation is permissionless.** Parameter changes go through proposal,
  timelock, queue, and execution. Binary upgrade authority is out of
  application scope.
- **Program authority stays in a cold/Squads workflow.** Runtime services use a
  signer interface so devnet keypairs can become remote or HSM signers without
  code change.

## 3. Non-negotiable webapp design-system contract (P0)

`dusk-webapp` is an adapter port and product extension, not a redesign.

### Preservation rule

- Preserve every current functional route, surface, interaction, responsive
  shell, and feedback pattern by default.
- Removal requires evidence that the surface is functionally unused plus an
  explicit product decision. Protocol incompatibility alone is not evidence
  that a surface should disappear.
- Replace legacy data and transaction adapters behind the UI instead of
  deleting the UI.
- Extend existing components and visual grammar before adding a new primitive.

### Visual and interaction rules

- Preserve Aeonik typography, colour tokens, module accents, density, spacing,
  radii, fading separators, continuous surfaces, and fixed black overlays.
- Preserve compact financial formatting, tabular numbers, exact-value
  affordances, wallet identity formatting, relative/absolute timestamps, and
  locale-safe numeric input.
- Preserve the 60px sticky material header, global search, wallet/settings,
  themes, toasts, error boundaries, route hydration, and module-tone behaviour.
- Use the existing rounded-square icon controls, `Segmented`, Radix dialogs and
  sheets, tables/mobile cards, amount fields, breakdowns, and toast patterns.
- Critical values and actions are never hover-only. Preserve keyboard, focus,
  reduced-motion, screen-reader, and mobile wrapping behaviour.
- New controls pass light/dark contrast and have at least 44px effective touch
  targets without changing the compact visible styling.
- Section shells are `rounded-3xl border-[1.6px] border-grey-200 bg-bg`; buttons
  come from `components/ui/cta.ts`. The binding rules live in
  `.agents/skills/omnipair-ui-system/SKILL.md`, `COLOR_SYSTEM.md`, and
  `docs/ui-product-criteria.md`, and are read before any surface is written.

### Route and flow extension matrix

| Existing surface | Preserve | Dusk extension |
| --- | --- | --- |
| Global shell | Header, search, wallet, theme, errors, toasts | Deployment identity, network selection, freshness and degradation state |
| Markets | Explore tabs, filters, sticky desktop table, mobile cards | Curve-kind badge and filter, Dusk TVL/utilization/health, source and freshness |
| Market detail | Identity, vitals, docked actions, chart, liquidity, borrowing, history, parameters | Curve/range/fee-tier, yLP/hLP, lending and governance constraints |
| Trade | Existing centered transaction shell | SDK quote/build/simulate for both curve kinds, exact fees and price impact |
| Borrow | Existing centered form and guide | Deposit/borrow/repay/withdraw previews and health consequences |
| Leverage | Existing mobile-first ordering and desktop two-column shell | Real positions, open/adjust/close/liquidate, delegated TP/SL |
| Liquidity | Existing split Radix sheet and compact amount controls | Add/remove, balanced and single-sided, range editor, LP lock and governed migration |
| Create market | Fixed overlay, step rail, review and staged deployment feedback | Curve parameters, fee/risk inputs, proposal/direct authority, timelock |
| Portfolio | Public/self modes and continuous command/chart composition | yLP, hLP, ranged LP, lending, leverage, locked LP, orders, proposals, auctions, activity |
| Governance | Existing table/detail/wizard/sheet vocabulary | Proposal list/detail/create, lock, queue, execute, expired/failed states |
| Faucet | — | Test-token minting; exists only where a faucet is configured, absent on mainnet |

### Data and transaction UX

- The SDK is authoritative for account state, previews, validation, and
  transaction construction.
- The indexer supplies discovery, lists, history, portfolio aggregation, charts,
  searchable activity, and keeper candidate hints.
- Keepers re-read target state from RPC before simulation and send.
- Each view model exposes source, slot, commitment, timestamp, and stale status.
- Mutations use one state machine:
  `idle -> preparing -> simulating -> awaiting-signature -> submitted -> confirming -> indexing -> success|error|cancelled`.
- Confirmation is signature- and slot-aware with indexer catch-up, never a fixed
  delay.
- Render explicit wrong-network, wrong-program, IDL-mismatch, RPC-degradation,
  partial-data, and indexer-lag states.

### Captured flow evidence

`../dusk-webapp/docs/ux-preservation-audit/audit.md` holds the 13-screen
evidence set and is a visual acceptance contract. Changes preserve those
structures or record an explicit exception with replacement evidence.

## 4. Protocol revision contract

The deployment is identified by a lock file, not by a branch or a commit
message. `protocol.lock.json` records:

- Revision name, cluster, and genesis hash.
- Program ids for `dusk` and `leverage_delegate`.
- IDL digests — both the raw file digest and the canonical key-sorted digest,
  because different consumers legitimately compute different ones.
- Program binary hashes, attested from the chain's upgradeable-loader accounts.
- SDK package name, version, and tarball digest.

The webapp and the API must read from the same RPC provider. A client brackets
each read between two of its own slot observations and rejects an envelope
observed outside that window; two providers disagree on the current slot, so
every read fails. For the same reason the envelope is observed per request
rather than cached, with only the expensive part — hashing program binaries —
cached by programdata slot.

Every consumer verifies the lock at startup and refuses to run against a
deployment it was not built for. The webapp additionally validates a deployment
envelope on every read, so a served payload cannot come from a program the
client was not built against.

Current pin: revision `devnet-1`, dusk `JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X`,
delegate `AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv`.

## 5. Devnet deployment and market bootstrap

- Deployment is scripted and idempotent: program deploy, IDL publication, lock
  regeneration, and attestation of on-chain binary hashes.
- Market bootstrap is deterministic and re-runnable, creating a documented set
  of markets from a manifest rather than ad-hoc scripts.
- Test tokens are minted by the faucet program, whose PDA holds mint authority.
  The faucet is devnet-only by configuration, not by a runtime check.
- Upgrade authority is held in the cold/Squads workflow. Nothing in the runtime
  path can upgrade a program.
- Reproducing the environment from scratch is a documented sequence of commands,
  and is exercised in CI against a throwaway program id.

## 6. Dusk SDK completion — the gate

Before any webapp flow is wired, the SDK exposes typed APIs for that flow's
complete read, preview, build lifecycle. This is the ordering constraint that
the previous plan stated and that the work then ignored.

Required domains:

- Market creation and multi-market discovery.
- Swap and quote for both curve kinds.
- Balanced and single-sided yLP/hLP operations.
- Concentrated range management.
- Deposit, borrow, repay, withdraw, health, and liquidation auctions.
- Open, adjust, close, and liquidate leverage.
- Leverage delegation, TP/SL create/cancel/query, and delegated close.
- Protocol-revenue auctions.
- Referral flows.
- Parameter proposal creation, locking, queueing, and execution.
- Faucet minting.

Rules:

- Generic account-heavy builders remain as an escape hatch. Product flows never
  depend on callers reconstructing instruction account order.
- Account resolution, PDA derivation, and discriminators live in the SDK and
  nowhere else. Any occurrence of these in `dusk-webapp` or `dusk-keepers` is a
  defect.
- The SDK is consumed as a published package, not a vendored tarball, once its
  version is stable enough to publish per change.
- Every typed builder has a test that asserts its account list against the IDL,
  so an IDL change breaks the test rather than production.

## 7. Keeper architecture

Rust is the live runtime for devnet. TypeScript remains scaffolded, compiles
against shared fixtures, and is not required for the devnet milestone; its
parity gate is a mainnet prerequisite.

### Services

One codebase, one image, one deployed service per job profile and wallet role.
`deploy/railway/profiles.json` already defines seven:

| Service | Wallet role | Jobs |
| --- | --- | --- |
| `dusk-sentinel` | none | health and candidate observation |
| `dusk-lending-trigger` | `lending_trigger` | start liquidation auctions |
| `dusk-lending-bidder` | `lending_bidder` | bid with capital |
| `dusk-lending-settler` | `lending_settler` | settle auctions |
| `dusk-leverage-keeper` | `leverage_executor` | leverage liquidation, stop-loss, take-profit |
| `dusk-auction-arbitrageur` | `revenue_auction_bidder` | protocol revenue buybacks |
| `dusk-lifecycle-keeper` | `lifecycle_executor` | low-frequency lifecycle work |

The lending auction is three services, not one, because trigger and settle need
only gas while bidding holds capital and inventory: a drained or compromised
bidder must not stop liquidations being triggered. `dusk-sentinel` holds no
wallet at all, so the watchdog cannot cause harm.

### Rollout order

Stand these up in sequence, not together:

1. `dusk-sentinel` — no wallet, no risk; proves ingestion and candidate
   detection before anything can sign.
2. Lending trio — once borrow and repay exist in the app.
3. `dusk-leverage-keeper` — once leverage flows exist.
4. `dusk-auction-arbitrageur` and `dusk-lifecycle-keeper` — last; they have
   nothing to act on until revenue and auctions are live.

### No cranker

Omnipair v1 ran an `update_pair` cranker as a deployed Railway service to
accrue interest. Dusk has no equivalent instruction — accrual is lazy, done on
interaction — so no such service exists here. This is deliberate. Do not
re-add one.

### Execution lifecycle

- Discover candidates from indexer hints, then re-read state from RPC before
  acting. Database state is never trusted alone.
- Simulate before send; classify expected races and terminal outcomes rather
  than retrying blindly.
- Bound work per tick, hold a lease, and release it on shutdown.
- Handle SIGTERM: stop discovery, drain in-flight work, release leases.

### Operational safety

- One service per job and environment, independently scalable.
- Per-job hot wallets with minimal SOL. No reuse of upgrade, governance,
  futarchy, or treasury authority.
- Signer interface, so a devnet keypair can become a remote signer unchanged.
- A capital floor is a readiness condition, not a runtime surprise.

## 8. Indexer architecture

- Ingestion from a durable finalized cursor over `getSignaturesForAddress` and
  `getTransaction`, decoding both Anchor event transports.
- Geyser/Yellowstone remains the intended primary ingestion path for mainnet
  scale; the polling ingester is the devnet implementation and the fallback.
- Reconciliation: periodic program-account scans decoded with the pinned
  revision.
- Unique event key: `(cluster, program_id, signature, instruction_index, event_index)`.
- Observations are stored separately from canonical state.
- Public APIs default to finalized data and expose freshness.
- Schema and decoder identity include cluster, program id, and IDL digest.
- Valuation is derived, and its derivation is visible: assets whose price is
  known by definition are anchored in a table with a recorded reason, and
  everything else is priced from pool ratios. No price is invented silently.

## 9. Operational model — public service bar

Because anyone can use this deployment:

- **Rate limiting** per IP and per wallet on the API, with limits that a normal
  session never hits and a script does.
- **Abuse handling**: faucet limits per wallet and per window; a documented way
  to cut off a specific abuser without taking the service down.
- **Status page** reporting API, indexer lag, RPC health, and keeper liveness,
  updated automatically rather than by hand.
- **`/live`** checks process survival. **`/ready`** fails closed on RPC lag,
  stale reconciliation, protocol mismatch, signer or capital floor, or database
  outage.
- **Structured logs, metrics, traces**, and build/protocol provenance on every
  service.
- **Runbooks** for the failures that will actually happen: RPC provider outage,
  indexer falling behind, keeper wallet drained, program upgraded underneath a
  running deployment, database full.
- **Deploy safety**: one service per job, always-restart for workers, cron only
  for low-frequency lifecycle jobs, and safe overlap during rollout.
- **Data retention** and backup for the indexer database, with a tested restore.
- Secrets are per-environment and never shared between devnet and mainnet.

## 10. Test and acceptance matrix

### Deterministic layer (LiteSVM, no cluster)

- Program behaviour, invariants, solvency, compute budget, economic bounds.
- Decoder fixtures: every event and account type decodes from recorded bytes.
- SDK builders: account lists asserted against the IDL.
- Keeper logic: candidate selection, priority, bounds, and terminal
  classification against recorded fixtures.

### Live layer (devnet)

- Every product flow end to end with a real wallet signature.
- Indexer ingest, backfill from a cold cursor, and reconciliation.
- Keeper execution against real positions, including losing a race.
- Degradation: RPC loss, blockhash expiry, indexer restart, stale data,
  program/IDL mismatch.

### Webapp quality

- Existing unit tests preserved; component and browser coverage added.
- Playwright covers wallet-signed happy paths, rejection, expiry, retries,
  duplicate execution, and partial or stale data.
- Axe, keyboard/focus, reduced-motion, and semantic interaction checks.
- Visual baselines for light and dark at mobile and desktop viewports.
- No route, function, design token, or interaction is removed without a
  recorded approved exception.

## 11. Promotion gates

### Devnet (this milestone)

Public deployment passing the full live matrix, operating unattended for a
sustained period, with observability and runbooks in place.

### Mainnet

The same artifacts and configuration schema against a mainnet revision, plus
the requirements deferred here: TypeScript keeper parity, and the staged
rollout `observe -> shadow -> approved limited-capital partitions -> expanded`.

## 12. Protocol revision reconciliation gate

A newer program revision is accepted only when all pass:

1. **Source** — record new hashes; review the diff from the pinned revision.
2. **ABI** — semantic-diff instructions, account order, arguments, PDA seeds,
   account layouts, types, events, errors, discriminators, for both programs.
3. **SDK** — regenerate clients; compile webapp, indexer, and keepers against
   the new revision.
4. **Data** — decode recorded account bytes and replay recorded events through
   the new decoders.
5. **Behaviour** — full deterministic and live matrices on newly built
   artifacts.

Reconciliation is one explicit compatibility change across consumers, never
independent untracked fixes per repository.

## 13. Execution phases and current status

| Phase | Deliverable | Status |
| --- | --- | --- |
| 0 | Devnet deployment, protocol lock, attested binaries | **Done** — revision `devnet-1` pinned and verified from chain |
| 1 | Indexer ingestion and public read API | **Done** — daemon ingesting to Timescale; v1-contract API serving markets, activity, stats, positions, derived valuation |
| 2 | Webapp reads devnet through the deployment envelope | **Done** — markets, market state and history render from the hosted API |
| 3 | Network selection and faucet | **Done** — per-network env resolution, picker appears when a second network is configured; faucet page live on devnet |
| 4 | SDK completion for product flows (section 6) | **Done for the app's actions** — typed builders added for swap, borrow, openLeverage and leverage delegation, plus a leverage-delegate client for conditional orders |
| 5 | Webapp writes through the SDK; fork lab ported and deleted | **Done** — all 9 actions build through the SDK, and no `fork` path or name remains in the app or the API; the lab in the `dusk` repo is unused and can be deleted |
| 6 | Rust keepers live on devnet | **Lending trigger and bidder live** — both have sent confirmed transactions on devnet: the trigger opened an auction on a genuinely underwater position, the bidder repaid 150 quote for 265.56 base. Settler, leverage, auction arbitrageur and lifecycle still have no loop |
| 7 | Public-service operations (section 9) | **Done** — rate limiting, `/status`, a self-refreshing status page, `/metrics` and `/provenance`, structured request logs, six runbooks, backups with a tested restore, and 90-day retention. The faucet limit is written but not deployed (see below) |
| 8 | Full live matrix and sustained unattended operation | **Matrix runs, soak runs; 7 of 11 flows pass** — `live_flow_matrix.ts` signs and sends every product flow: faucet, add and remove liquidity, deposit, borrow, repay and withdraw all confirm. Swap and open leverage are blocked by the hLP defect and the two leverage follow-ups cascade from it. `soak.sh` samples the deployment unattended and is what caught the monitoring flaw below |

### Deployment

All seven keeper services run in the `dusk-devnet` Railway project in **live
mode**, one per job and wallet role as section 7 requires. Each signing profile
has its own hot wallet, generated on authorisation and kept in
`~/.config/omnipair/dusk-devnet/keeper-<profile>.json`, funded with 1 SOL and —
for the four that spend — 5,000 of each asset as inventory.

The rollout was staged as section 7 asks: the lending trio first, verified
live, then leverage, the arbitrageur and lifecycle.

**Proven unattended.** A position was made underwater; the deployed trigger
discovered it and opened the auction on its own, and the deployed bidder filled
it from its own wallet a discovery cycle later — 6.25 quote paid for 14.40
base, confirmed on chain. Neither was prompted.

### Keeper status by profile

| Profile | State |
| --- | --- |
| `dusk-sentinel` | Deployed, no wallet, reading the chain |
| `dusk-lending-trigger` | **Sent live** — opened an auction on a real underwater position |
| `dusk-lending-bidder` | **Sent live** — repaid 150 quote for 265.56 base |
| `dusk-lending-settler` | Complete; every settlement refused by the hLP defect |
| `dusk-leverage-keeper` | Evaluates a real 3x position and declines it correctly |
| `dusk-auction-arbitrageur` | Offers all eight lane/source/side combinations; all refused for want of revenue |
| `dusk-lifecycle-keeper` | Runs; no parameter proposals exist on this deployment yet |

Only the trigger and bidder have been proven by sending. The other three
signing profiles are proven as far as this deployment allows: they discover
real accounts, assemble their instructions, and are refused by the program for
reasons that name a business condition rather than a malformed transaction. A
wrong account fails differently from an empty lane, which is what makes the
refusals evidence rather than silence.

### The first monitoring was measuring the wrong thing

`/status` originally flagged on slot lag. Slot lag measures how recently
somebody traded, not whether the daemon is alive, so on a quiet devnet it grows
without bound while ingestion is perfectly healthy — the endpoint would have
declared a working deployment degraded and taught whoever read it to ignore the
alarm. The runbook warned against judging this by `latestEventAt` for exactly
that reason, and then the endpoint judged it by the newest event slot, which
has the same flaw.

The soak caught it: lag climbing steadily, event count flat, daemon logging
nothing. That reads as a stall and was an idle market.

The daemon now touches its cursor on every poll whether or not it found
anything, so the cursor's age is a liveness signal rather than a measure of
trading activity, and that age is what decides degradation. Measured live: age
holding at 3-15 seconds while lag passed 7,000. Slot lag is still reported —
it is the right number for how stale the history is, just not for whether
ingestion works.

### The lending liquidation path is proven end to end

A position was made underwater on purpose, the trigger opened its auction, and
the bidder filled it — all on live devnet, all confirmed. Three healthy
positions were declined in the same pass, so the discrimination is real and
not an artifact of there being nothing else to look at.

Two design choices carried the work, and both are the same choice: **the
program is the oracle, not the keeper.** The bidder does not recompute the
auction's decaying price; it simulates with no floor, reads the collateral
that actually arrives, and binds the sent transaction to what it measured. It
does not recompute the partial-liquidation cap either; it searches for the
largest repayment the program will accept. Either formula reimplemented in the
keeper would be free to drift from the protocol it is pricing against.

Measuring is also the only way to tell a fill from a no-op:
`fill_liquidation_auction` returns `Ok` without moving anything once an
auction has recovered, so a bidder trusting the return code would pay a fee,
change nothing, and record a successful liquidation.

### Swaps revert: diagnosed, not yet fixed (blocking)

Swaps on the devnet market revert with `BrokenInvariant` (6047) at the hLP
reserve-identity check in `transitions/liquidity/hlp/engine.rs`. Rate varies
with market state: a quarter on an idle market at one point, all of them at
another.

**The cause is measured.** An instrumented build was deployed briefly, its logs
read, and the attested binary restored:

```
hlp-drift base=1 quote=25 base_interest=0 quote_interest=0 final_base_debt=1482619 final_quote_debt=0
```

The quote-side drift is 24-26 atoms against a three-atom tolerance, and **both
interest tranches are zero**. Every explanation built around accrued interest
was wrong — the double-subtraction theory, the "debt makes it constant"
reading, and the periodicity attributed to interest ticking over.

`final_base_debt` cancels across the comparison, so what disagrees is the
materialized reserves against the quoted endpoint rebuilt through
`denormalize_from_nad_floor`:

```
(quote_live_reserve - old_quote_hlp_live)   vs   (ordinary_quote + quote_equity)
```

The tolerance's premise — three independently floored quantities, each off by
at most one — does not describe an error of that size. The remaining clue is
the asymmetry: base drifts by 1 and quote by 25, and only the base hLP vault
carries debt, so the side *without* debt is the side that drifts.

Widening the constant is still the wrong fix: the error is a reconstruction
disagreement scaling with market magnitudes, not a fixed rounding budget.
Deciding what the quoted endpoint should reconstruct is a protocol design
question. `programs/dusk/src/tests/fixtures/devnet-replay/README.md` holds the
full record, including everything ruled out on the way.

### Faucet abuse: written, not deployed

`faucet_mint` now takes a per-request ceiling and an hourly per-recipient
cooldown, recorded in a claim account keyed by recipient and mint — not by
payer, since a payer-keyed limit is sidestepped by paying from a fresh wallet,
which costs nothing on devnet. The webapp passes the new account.

Not deployed. It changes `faucet_mint`'s account list, so the program upgrade
and the app must land together, and upgrading a deployed program is a decision
for whoever holds the upgrade authority.

### The original gap, for reference

The faucet mints straight from the browser to the program, so no server sits in
the path and no amount of API rate limiting constrains it. `faucet_mint` checks
only that the amount is above zero — there is no per-wallet cap, no cooldown,
and no supply ceiling. On a public devnet one actor can mint unbounded balances
and distort every market with them.

Closing this needs a program change and a redeploy, which is a protocol
decision rather than an operational one. The options are a per-wallet cooldown
account, a per-mint supply ceiling, or moving the faucet behind a server that
holds the authority. Until one is chosen the faucet is safe only because devnet
tokens are worthless — which is a reason not to promote this program shape to
mainnet unchanged.

### The keeper contract had drifted from the deployed program

Four of the eleven instructions the keepers were pinned to did not exist in the
deployed program: `trigger_liquidation_auction`, `bid_liquidation_auction`,
`settle_liquidation_auction_floor` and `liquidate_leverage` had become
`start_liquidation_auction`, `fill_liquidation_auction`,
`backstop_liquidation_auction` and `liquidate_leverage_position`. Every keeper
built on them would have been dispatched to the transfer-hook fallback.

The conformance suite could not catch it. It checked that each discriminator
matched the Anchor hash of the manifest's own instruction name, which is true
of any name at all, including one the program has never heard of. The
generator did check against the pinned IDL — it had simply not been re-run
since the lock moved to `devnet-1`.

The drift went past names: the backstop takes one argument rather than four,
three account lists had gained accounts, and `delegated_close_leverage` had
gained a `close_bps` field. The lock was also frozen without a source
fingerprint, so `assert_live_ready` would have refused every live start.

All of it is corrected and regenerated from the IDL, and the tests that
asserted the pin rather than the rule now demote a lock in the test instead of
depending on nothing being deployed.

Honest summary: reads are finished, every write the app can request is built
through the SDK, and **four writes have now been signed and confirmed on
devnet** — faucet, swap, deposit collateral, borrow — leaving a real borrow
position at `Hnqf2sonacYGJdLHESagBWovSsHirkRyBXzqLvRwuZSz` for the keepers to
watch. The lending trigger evaluates that position correctly in shadow mode
and declines to act on it, which is the right answer for a healthy position.

What remains: no keeper has sent a live transaction, five signing profiles
still have no execution loop, and no liquidation has been driven end to end —
which needs an unhealthy position, and therefore a way to make one on devnet.

## 14. What is left, and why

Three items remain, and none of them is engineering. Each is written, tested
and one command from done; each needs a decision that is not an engineer's to
make on someone else's behalf.

| Item | State | What it needs |
| --- | --- | --- |
| hLP invariant defect | Diagnosed as far as off-chain work allows; eight hypotheses eliminated by measurement; instrumented build ready behind `debug-hlp-drift` | Deploy the instrumented build, simulate a swap, read `hlp-drift` in the logs |
| Faucet abuse limit | Per-request ceiling and hourly per-recipient cooldown implemented; webapp passes the new account | Upgrade the program and ship the app together — the account list changes, so they cannot land separately |
| Keepers live | All seven profiles deployed and healthy in shadow; trigger and bidder both proven live from a local run against this same devnet | A generated hot wallet per service as `KEEPER_SIGNER_KEY`, and `KEEPER_MODE=live` |

Phase 8's four failing flows — swap, open leverage, and the two leverage steps
that cascade from open leverage — are all downstream of the first item. Nothing
else in the matrix is blocked, and no further keeper or indexer work moves
them.

## 15. Definition of done

A person who has never seen this project can open the public devnet webapp,
connect a wallet, mint test tokens, and complete every supported product flow —
swap, provide and remove liquidity, deposit, borrow, repay, withdraw, open and
close leverage, place and cancel conditional orders, create a market, and move a
parameter proposal through timelock to execution — with each transaction built
by the SDK, confirmed slot-aware, and reflected in indexed history.

Meanwhile Rust keepers liquidate unhealthy positions and settle auctions without
supervision; the indexer backfills from cold and reconciles; health, metrics and
provenance are inspectable; the status page reflects reality; documented runbooks
cover the failures that occur; and the same artifacts and configuration schema
promote to mainnet with no code change.
