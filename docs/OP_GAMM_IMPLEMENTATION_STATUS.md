# Bounded OP-GAMM implementation

Updated 31 October 2026. Implemented on merged main `d3d1cfadfcd36d34ada911dd31f7d588df8c78e6`, incorporating PR #45 and its #52 changes. This is the integrated bounded design, not the paper’s unbounded blend. Historical research documents retain their original assumptions and dates.

## Market contract

Market creation accepts an optional oracle policy alongside the other parameters. Omission selects an empty policy and λ = 0; a supplied policy launches the market with its chosen source and weight in the same instruction. There is no separate initial-configuration instruction or permanent native-only option. Creation validates the policy and emits it in `MarketCreated`; first liquidity authenticates a current report for positive weight. After creation, the existing proposal/support/timelock mechanism can activate, change or disable the oracle prospectively. Every proposed policy obeys the same mathematical limits: λ below 100%, and a nonempty policy's adjustment multiplier above one. Its source, weight and adjustment cap can change without moving LP inventory. No provider allowlist or Horus privilege is introduced.

- **λ = 0:** native Dusk pricing, no feed-account dependency. Lending, leverage and hLP use that same native pricing path.
- **Positive λ:** 1–9,999 basis points. For native price N, external reference O and cap M, the marginal price is `N × exp(clip(λ × ln(O/N), −ln(M), ln(M)))`. Clipping preserves full-range native tails; it limits adjustment rather than guaranteeing exact oracle tracking.
- **One consumer path:** read authenticated on-chain accounts. Upstream verification/update can precede Dusk in the same transaction. Dusk does not fetch off-chain data or verify a separate raw signed-report format.
- **Source formats:** a versioned normalized pair account and fully verified Pyth `PriceUpdateV2` accounts for both assets. The latter computes a ratio, including quote-asset uncertainty, rather than assuming a dollar peg.
- **Invalid/stale source:** price-sensitive actions reject. Debt-token repayment and collateral additions can proceed without granting price authority. There is no automatic switch to native pricing, time-decay liquidation, or guaranteed external execution during an outage.

The provider, accounts, feed identities, age in seconds and slots, confidence limits, and Pyth publication skew are selected by the market. Account owner, format, instrument identity, status, positivity, future timestamps and replay/equivocation are checked on-chain. A stored observation is history, not an execution capability: authentication is repeated after every account deserialization.

A proposal containing exactly one oracle-policy update can execute despite market distress or utilization at/above 80%. This lets governance replace a dead source or explicitly disable the oracle before liquidation can resume. Positive weight still requires a fresh authenticated proposed source; support, timelock, execution window and revision checks still apply. Adding any other parameter update restores the ordinary utilization and market-health guards for the whole proposal. This exception grants no new borrowing authority and performs no automatic inventory fallback.

Liquidation follows main's health-only policy from PR #59: there is no stored distress episode or incentive clock. `previewLiquidationHealth` is read-only and authenticates the same selected oracle; begin/emergency execution independently checks current health. See [health-only liquidation](HEALTH_ONLY_LIQUIDATION.md).

## Coupled execution and accounting

Swaps, lending marks, leverage closeout and hLP valuation use the bounded market price. Lending does not substitute the raw external price for the executable market price. Existing cash, daily-rate, health, stored-margin and transfer-fee constraints remain active.

The positive-weight output solver uses outward-rounded Q56 intervals. Native primitives and available special cases are analytic; other regions use bounded Taylor integration. The rounded ownership endpoint must have a nonnegative lower conditional-invariant bound. Baseline preparation gains stay with LPs. Uncertainty, overflow, nonconvergence or insufficient cash rejects the transaction. Neither floating point nor an approximate root alone authorizes output.

hLP net NAV is separated from cash/backing and carried interest. The endpoint accounts for indexed debt, raw-atom share rounding, frozen cohorts and compounded fees. A bounded fixed-point iteration solves carry cover at the materialized mark; failure rejects rather than settling an inconsistent endpoint. The native frozen-hLP double-counting/denominator regression is fixed as a prerequisite.

Oracle-only changes update price/volatility history once without moving balances or silently rehedging hLP. hLP still needs an executing transaction and available cash to rebalance. Existing funded maintenance/recovery paths remain necessary in idle markets.

A recognized-depth ceiling prevents movements between native invariants under OP trades from manufacturing lending capacity. Real ordinary LP share additions scale that ceiling; losses reduce it. **Current limitation:** fee compounding without new ordinary shares does not automatically raise the ceiling. Fees remain LP-owned capital, but the credit ceiling can lag that capital. This is disclosed conservative admission, not an external depth oracle or a new interest surcharge.

## Delegated close and SDK

Dusk prepares the selected close once and exposes a temporary quote in its own position account during the read-only approval callback. Partial closes also include the full-position trigger value. The delegate checks the selected slice and payout against that authenticated context rather than rerunning the OP solver. Dusk protects market, position, mints and both source accounts across approval, checks actual token credit, and clears the context before settlement. The account cannot retain a successful transaction’s approval quote. Standalone delegate calls cannot supply it.

SDK builders resolve the selected feeds and append them after existing hLP/hook account prefixes. Positive-weight transaction builders request compute/heap budgets; raw instruction builders require the composer to add budgets before binding flash instruction indices. Governance execution resolves the proposed replacement feed, allowing recovery from a dead old provider. Virtual-book snapshots include feed bytes and policy identity; samples from changing observations are not combined into one depth view. All four vendored Dusk/delegate IDL/type files are regenerated with the source interface.

## Validation and practical bounds

The complete required local CI sequence passes:

- **577 native Dusk tests**, including fixed-observation repeated cycles, accrued carry, both frozen cohorts, compounded fees, λ = 0, both-direction and split exit loans, and governance transitions with outstanding debt/claims.
- **153 LiteSVM tests**, exercising all **77 IDL instructions** and the complete existing compute baseline. OP route cases span λ = 0, 25%, 50%, 99.99%; amplification 1, 4, 32, 2,000; both hLP vaults; changing reports and funding accrual; and full fee compounding at amplification 4. Delegated full/partial closes, Token-2022 fees, invalid-source rollback, price-free principal/interest repayment, Pyth activation, stale-provider replacement and return to native mode are included. Creation cases cover optional normalized/Pyth policy selection, invalid-policy rollback including the treasury charge, and missing/stale feed rejection before first liquidity.
- **82 SDK tests**, including wire encoding, automatic market-address resolution, creation packet limits, feed resolution, transaction budget ordering, governance and virtual-book identity.
- Independent high-precision invariant/price references, wide division/root differential tests, interval enclosure tests and a transfer-hook resolver comparison against SPL. See [numerical evidence](../analysis/op-gamm-implementation-2026-10-30/README.md).

The [machine-readable validation record](../analysis/op-gamm-implementation-2026-10-30/validation-main-merge-2026-10-31.json) includes every required command, source hashes and binary hashes. Formatting, hygiene, helper shape, Clippy, normal/production checks, the benchmark EMA invariant, delegate/faucet tests, TypeScript, both Anchor interfaces, clean-build identity, SDK checks and the complete runtime baseline pass. The native same-slot CPMM swap measures 99,432 CU against its unchanged 100,000-CU guard. The final suite executes 5,220 successful transactions, with a maximum of 1,275,921 CU. OP routes use the existing transaction limit; no failed transaction or enlarged diagnostic limit is counted as successful execution.

These are finite tests, not a security proof or an independent audit. Arbitrary trade size, concentration, fee policy, callback composition and integer precision still have transaction/work limits: clients must simulate the complete route. Accepted but wrong prices can still cause LP losses. Governance changes can create trading opportunities even when claims and balances are preserved. Asset-specific λ/cap/freshness calibration remains a deployment decision.

## Separate work

Horus aggregation and optional depth-sensitive **borrow-interest** pricing remain separate from this PR’s OP-GAMM integration. The depth design uses fast deterioration/slow recovery; absent depth data means the ordinary base APR. This implementation does not claim to produce aggregate external liquidity or price outage liquidation execution. No deployed accounts or production-market migrations are assumed.

## Oracle-derived first liquidity (31 October follow-up)

A positive-weight market has no independent caller-selected opening price.
`initialize_market` rejects a nonzero launch-adapter bootstrap price for guided
creation. At first liquidity, `preview_add_liquidity` uses the invocation's
validated observation to choose reserve credits within both gross-spend caps
(after transfer-fee conversion). Base rounds down and quote rounds up to token
atoms; unused amounts are never transferred. `add_liquidity` recomputes the
same credits from measured transfers before minting yLP. The resulting native
reserve ratio matches the reference up to less than one quote atom, rather
than promising exact real-number equality. The existing min-yLP floor still
protects execution. Native first liquidity and later proportional deposits
retain their prior rules. No instruction payload or account layout changes.

Validation covers missing/stale observations, mismatched caller ratios, a
reference move between instruction construction and execution, unused wallet
funds, min-yLP rollback, mixed token decimals, and rounded-preview idempotence.

The first-deposit change also exercises a different inventory state in the
existing partial delegated-close fixture. Decimal rescaling now uses an exact
compile-time table for powers 10^0 through 10^38 instead of repeated runtime
exponentiation. Its behavior is checked against the prior implementation over
all 256 × 256 decimal pairs, five boundary amounts and both rounding directions
(655,360 cases). The oracle-partial delegated close measures 1,322,073 CU under
the unchanged 1,355,000-CU repository guard and 1.4M network cap. No curve,
invariant, liquidation, interest or rounding rule was changed for this saving.
The full 155-case SVM suite passes with 77/77 instruction coverage and the
complete required compute baselines.
