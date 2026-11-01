# Native implementation status

1 November 2026. Branch: `feat/junior-risk-pricing`.
Base: `f1ddce100ae66401023dfe13c0eefe5fcd648f9c`.

The native market, token instructions, governance configuration and SDK now use
hLP, yLP and jrLP. yLP is the default senior claim; only ordinary yLP enters junior.
The verification manifest records the final checks. This work does not deploy or
merge the program.

## Implemented

- Market-wide senior/junior reserve ownership, a funded 0.1% permanent seed,
  current-generation jrLP mint, direct yLP burns on entry, and current-value
  yLP issuance on conversion. hLP does not enter junior.
- Seven-to-ten-day crowding notice, two-day exercise window, 5 bp request/renewal
  fee, escrow, cancellation, expiry and checked liquidation restrictions.
  Queued capital continues earning and taking losses until conversion.
- Per-asset exponential rates driven by `(junior / total debt) × (1 − utilization)`.
  Defaults: 5–6.25% signal band, 14-day half-life, 4% initial APR, 2% minimum,
  no configured ceiling, 80% new-borrow utilization limit. Market configuration
  and prospective governance updates remain available.
- Separate exposure-time receivable accounting for yLP/jrLP. Historical earnings
  remain with the earning holder; transfer or exit cannot give them to a newcomer.
  Public-interest advances replenish pool backing on repayment and are not paid twice.
- hLP keeps its existing full-value treatment of its own unpaid income. Collected
  income pays funding costs before holder distributions. Its share comes from
  senior's half. Partial/final exits acquire only departing hLP coupons for pool
  backing; there is no separate unpaid claim for the exiting hLP holder.
- Shared junior protection for covered public, leverage and hLP principal losses
  after insurance. Junior-only routing of the former insurance-fill liquidation
  allocation; executor and protocol allocations remain separate.
- 50/50 claimable fees and interest, plus native compounded-fee allocation.
  Compounding changes junior common ownership without creating senior wallet
  tokens. After a junior wipe, the junior fee allocation stays in seed-only
  custody until the retired generation is archived and seed capital reinvested.
- SDK builders for junior setup, conversion/queue operations, income-history
  catch-up, retired receipt burns and seed funding/reinvestment.

## Verification

The final [verification manifest](NATIVE-VERIFICATION.json) records commands,
results and source hashes. Native tests cover both asset directions, ordinary and
hLP pools, 0/20/70% junior allocations, fee compounding, first-loss conservation,
queued exits, historical income, hLP funding netting, positive total NAV with
negative funded NAV, and final exit/re-entry.

LiteSVM exercises native conversion, notices and exercise windows, seed funding,
receivable archival/catch-up, wiped-generation retirement, asset transfer fees,
leveraged positions and protection-order execution through SDK builders. The
ordinary same-slot swap still has a strict 100,000-CU acceptance limit. Other
scenario ceilings are measured against the finished binary with 5% headroom.

The account refresh and transfer-hook resolver have parity checks against their
canonical decoders/resolvers. Event data keeps its existing encoding while avoiding
large temporary allocations. All four generated SDK interface files are included.

## Practical limits

- Income history requires permissionless archival and holder checkpointing.
  Catch-up uses at most eight epochs per instruction; total history is unbounded.
  Settlement/roll churn can increase maintenance costs. The SDK supplies explicit
  batches and retries; it does not guarantee a single-transaction exit for a holder
  whose history is stale.
- Seed compounding requires a balanced asset pair. Unmatched revenue remains in
  seed custody and is excluded from productive junior coverage until reinvested.
  Because reinvestment is permissionless, an initialized market also requires
  current concentrated spot to remain within one basis point of the AMM EMA.
- Exit crowding changes request notice, not total conversion throughput. Junior
  coverage can drop when multiple windows overlap. Cash withdrawal is separate.
- Junior protection is finite, and productive junior capital shares AMM inventory
  exposure. Uncapped default APR means no governance-selected economic ceiling;
  arithmetic still uses the largest APR the public configuration can represent.
  Neither setting is proof of sustainable borrowing demand.

See [the scoped review](SCOPED-REVIEW.md) for these accounting and operating limits.
Historical economic simulations preserve their original assumptions. They are
research evidence, not execution results from the native implementation. No claim
of an independent security audit or production readiness is made here.
