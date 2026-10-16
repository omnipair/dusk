# EMA-priced internal emergency settlement

Date: 2026-10-16. Consolidated PR #45.

## Implemented scope

V2 liquidation health remains linear symmetric-EMA collateral value, including
its existing transfer-fee treatment and stored maintenance terms. Current AMM
price impact and LP withdrawals do not enter that health calculation.

Emergency settlement reconstructs the configured concentrated tail/band curve
using the collateral side's symmetric price EMA and `curve_depth_ema_nad`.
The reverse side uses its own EMA: independently averaged reciprocal prices
are not assumed to be exact inverses. Reference depth is the existing smoothed
active curve depth, not live `k`, live reserves, or `min(live, EMA)`.

Partial recovery and full-close certification use that identical geometry and
fee calculation. Ordinary flash payments and voluntary external/Dusk routing
keep their existing rules. The emergency health boundary, partial-first rules,
rewards, insurance limits and principal-first loss waterfall are unchanged.

## Fees and real accounting

- Full configured fees remain. Launch-price and divergence inputs use the
  reference curve; volatility uses the existing accumulator and decay.
  Changed fee history can therefore change net proceeds with unchanged EMAs.
- The quote credits debt repayment in exchange for real seized collateral.
  Virtual reserves, virtual hLP debts and the virtual compounded endpoint are
  never committed as actual inventory.
- Real cash funds caller/owner payouts, accrued hLP funding and distributed
  charges. Principal nets internally. Insufficient real cash still rejects.
- hLP shares/debt are reconstructed from actual post-settlement balances.
  Reference inventory losses consume protection; an apparent reference gain
  does not manufacture spendable recenter protection.
- Post-settlement risk/volatility observations use actual pool prices.

## SDK and events

`EmergencyLiquidationQuote` adds `reference_price_nad` (debt per collateral,
1e9 scale) and `reference_depth_nad` (normalized curve depth). Both preview and
`EmergencyLiquidationSettled` expose them. Account layouts are unchanged.

`DuskLiquidations.previewEmergency()` decodes the new fields.
`buildEmergency()` returns bigint `reference.priceNad` and `reference.depthNad`,
continues resolving hLP accounts and uses the preview's minimum output/reward
by default. The reference is not locked across transactions; simulate again
when state changes. `SwapExecuted` describes the reference-priced debt offset
and actual final reserve/spot snapshots, not a live execution quote.

## Verification

Required local checks passed:

- Rust formatting, repository hygiene, code-shape and Clippy gates.
- 501 Dusk tests in each normal/production profile, production library check,
  16 delegate tests and 1 faucet test.
- Anchor/SBF builds for all programs and fixtures; build-identity check;
  regenerated Dusk/delegate interfaces; TypeScript and 64 SDK tests.
- Strict LiteSVM: 135 passing, no pending tests, 71/71 instructions exercised
  with the complete existing compute baseline and no raised ceilings.

Five new native regressions cover 144 combinations of 1x/4x/10x concentration,
both debt directions, hLP on/off, fee collection modes, fee compounding,
transfer fees and partial/full settlement; additional cases cover actual
settlement following spot moves, directional EMAs, gradual depth updates,
LP withdrawals, one-use prepared plans and accrued hLP/borrow interest.

The concentrated-hLP instruction test compares SDK previews before/after a
same-slot spot move with unchanged fee state, executes the returned plan and
matches event price/depth/output to preview. The fallback test now verifies
that a poor live quote cannot override a reference-priced partial, while
sufficiently shallow smoothed depth can still authorize the approved full close.

Logs: `.audit/findings/ema-settlement-implementation/` (local evidence).

## Finding #287652

The direct live-price/depth dependency is removed. The subsequent
[current implementation closeout](EMA_SETTLEMENT_CLOSEOUT.md) uses admitted
positions and actual settlement, with both fees and final LP/hLP value. It confirms
unchanged reference invoices and settlement resource deltas in the tested paths,
but additional trading profit around real inventory remains. #287652 stays open
for that residual economic risk. The older counterfactual report is historical
evidence, not the latest validation. No historical PoC, merge or deployment.
