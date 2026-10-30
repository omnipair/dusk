# Bounded OP-GAMM implementation

30 October 2026. Implemented on merged main `d3d1cfadfcd36d34ada911dd31f7d588df8c78e6`, incorporating PR #45 and its #52 changes. This is the integrated bounded design, not the paper’s unbounded blend. Historical research documents retain their original assumptions and dates.

## Market contract

A market discloses immutable maximum oracle weight and maximum adjustment at creation. A zero maximum creates a permanently oracleless market. Otherwise the creator can configure a source before first liquidity, and the existing proposal/support/timelock mechanism can change the source and weight prospectively. No provider allowlist or Horus privilege is introduced.

- **λ = 0:** native Dusk pricing, no feed-account dependency. Lending, leverage and hLP use that same native pricing path.
- **Positive λ:** 1–9,999 basis points. For native price N, external reference O and cap M, the marginal price is `N × exp(clip(λ × ln(O/N), −ln(M), ln(M)))`. Clipping preserves full-range native tails; it limits adjustment rather than guaranteeing exact oracle tracking.
- **One consumer path:** read authenticated on-chain accounts. Upstream verification/update can precede Dusk in the same transaction. Dusk does not fetch off-chain data or verify a separate raw signed-report format.
- **Source formats:** a versioned normalized pair account and fully verified Pyth `PriceUpdateV2` accounts for both assets. The latter computes a ratio, including quote-asset uncertainty, rather than assuming a dollar peg.
- **Invalid/stale source:** price-sensitive actions reject. Debt-token repayment and collateral additions can proceed without granting price authority. There is no automatic switch to native pricing, time-decay liquidation, or guaranteed external execution during an outage.

The provider, accounts, feed identities, age in seconds and slots, confidence limits, and Pyth publication skew are selected by the market. Account owner, format, instrument identity, status, positivity, future timestamps and replay/equivocation are checked on-chain. A stored observation is history, not an execution capability: authentication is repeated after every account deserialization.

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

- **574 native Dusk tests**, including fixed-observation repeated cycles, accrued carry, both frozen cohorts, compounded fees, λ = 0, both-direction and split exit loans, and governance transitions with outstanding debt/claims.
- **149 LiteSVM tests**, exercising all **76 instructions** and the complete existing compute baseline. OP route cases span λ = 0, 25%, 50%, 99.99%; amplification 1, 4, 32, 2,000; both hLP vaults; changing reports and funding accrual; and full fee compounding at amplification 4. Delegated full/partial closes, Token-2022 fees, invalid-source rollback, price-free principal/interest repayment, Pyth activation, stale-provider replacement and return to native mode are included.
- **79 SDK tests**, including wire encoding, feed resolution, transaction budget ordering, governance and virtual-book identity.
- Independent high-precision invariant/price references, wide division/root differential tests, interval enclosure tests and a transfer-hook resolver comparison against SPL. See [numerical evidence](../analysis/op-gamm-implementation-2026-10-30/README.md).

The [machine-readable validation record](../analysis/op-gamm-implementation-2026-10-30/validation.json) includes every required command, source hashes and binary hashes. Formatting, hygiene, helper shape, Clippy, normal/production checks, the benchmark EMA invariant, delegate/faucet tests, TypeScript, both Anchor interfaces, clean-build identity, SDK checks and the complete runtime baseline pass. The native same-slot CPMM swap measures 99,650 CU against its unchanged 100,000-CU guard. The final suite executes 5,102 successful transactions, with a maximum of 1,275,890 CU. OP routes use the existing transaction limit; no failed transaction or enlarged diagnostic limit is counted as successful execution.

These are finite tests, not a security proof or an independent audit. Arbitrary trade size, concentration, fee policy, callback composition and integer precision still have transaction/work limits: clients must simulate the complete route. Accepted but wrong prices can still cause LP losses. Governance changes can create trading opportunities even when claims and balances are preserved. Asset-specific λ/cap/freshness calibration remains a deployment decision.

## Separate work

Horus aggregation and optional depth-sensitive **borrow-interest** pricing remain separate from this PR’s OP-GAMM integration. The depth design uses fast deterioration/slow recovery; absent depth data means the ordinary base APR. This implementation does not claim to produce aggregate external liquidity or price outage liquidation execution. No deployed accounts or production-market migrations are assumed.
