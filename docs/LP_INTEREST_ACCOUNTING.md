# LP interest accounting

Implementation target: a separate PR into `fix/benchmark-replay-owner` (PR #45),
starting at `5c0aca7fcb582418f1c5b99061d4ff7605b6f185`.

## Selected behavior (2026-10-18)

- Keep V1-style ownership: accrued public borrower interest is included in LP
  share value. Entrants pay for their share of the existing receivable.
- Apply the same economic backing to yLP mint/burn and hLP share valuation.
- An exiting holder receives the permitted cash withdrawal and retains no
  separate unpaid-interest entitlement. Existing funded yield is distinct.
- Repayment converts a receivable into cash; it must not count the same income
  twice. Cancellation removes the receivable from current share value.
- Cash availability and backing checks remain binding. Notional reserves do
  not authorize spending tokens the reserve vault does not hold.
- Preserve the existing swap-fee split: compounded fees enter share principal;
  separate swap fees retain their funded per-holder index.
- Keep the current AMM pricing model: public unpaid interest remains excluded
  from `curve_reserve`. The later V1-style swap-pricing proposal was declined.

The previously proposed historical claim ledger and proportional funding
policy are superseded. They are not part of this implementation.

## Full-value controller accounting (2026-10-19)

The accounting implementation uses live-reserve claims consistently for hLP deposit
funding, entry admission, NAV, hedge reconstruction and recovery quotes. The
opposite-token funding leg matches the ratio used to mint the underlying yLP.
Existing funded holder claims remain separate from this NAV.

Trading still uses the ordinary curve coordinates, with public unpaid interest
excluded. The transient integrated state carries separate book-value offsets:

```text
book_base  = ordinary_curve_base  + base_book_offset
book_quote = ordinary_curve_quote + quote_book_offset

live_base  = book_base  + base_hLP_equity  + quote_hLP_debt
live_quote = book_quote + quote_hLP_equity + base_hLP_debt
curve[i]   = live[i] - unpaid_public_interest[i]
```

The book offsets provide cash-flow coordinates during one quoted trade.
Trader cash changes the curve and book ledgers by the same amount. Ownership
and opposite-asset debt are reconstructed from full book backing; public
receivables remain excluded from pricing principal.

The price calculation must include that reconstructed ownership. Freezing an
old curve point and repricing only after settlement was insufficient: a
fee-free 10,000-token round trip could gain 0.116057 tokens initially and
0.001803 on a repeated cycle in one test. The corrected quote starts from the
canonical hedged point and certifies the completed endpoint against the same
curve. A few residual-based candidates are followed, when needed, by bounded
bisection; every accepted candidate independently satisfies the invariant.
When no active hLP owns unpaid interest, the quote retains its analytic path.

The regression runs three successive round trips across both directions,
both interest assets, 1x/4x concentration, 20k/200k hLP deposits, 25k/250k
receivables and 1k/10k/100k trades. It requires no positive trader return,
including with zero swap fees. Separate tests verify full NAV, cash
conservation, repayment/writeoff accounting and the quoted final price after
compounding and recovery incentives.

The reserve/debt identity retains its existing atom-sized reconciliation
bound. Final cache, price and volatility observations come from the
atom-rounded settlement plan. Owner leverage closes use the measured-input
settlement's existing debt-coverage and payout checks; only delegated closes
need the additional pre-callback payout quote.

Opposite-asset residuals are valued at the actual concentrated curve mark,
not the raw reserve ratio. The extreme funding regression verifies economic
NAV before and after rehedging, in addition to checking that the operation
executes. Cash must cover the actual funding payment.

Native coverage includes both interest assets and both hLP targets at 1x and
4x concentration; swaps, cash withdrawals, public debt repayment, principal
writeoff, canceled interest, current-holder yield and recovery from negative
curve-only equity with positive full NAV. Interest accrual alone leaves the
trading price unchanged. Repayment removes the receivable and funds yield
once; writeoff reduces current LP value rather than creating yield.

The SDK `get.previewHlpDepositCapacity` helper uses the native full-value
admission and funding ceiling. `Ready` is a current funding/hedge status, not a
promise that every smaller deposit passes amount-specific rounding checks.

## Validation and compute cost

The complete local CI sequence passes for this accounting correction: 513
Dusk tests in each default/production profile, the EMA settlement invariant,
delegate/faucet tests, formatting, hygiene, code shape, clippy, TypeScript,
SBF builds, build identity, IDL freshness, and 70 SDK checks. The strict LiteSVM
suite passes 136 tests, exercises 71/71 instructions and checks all 18 required
compute scenarios. Generated SDK interfaces are unchanged by this correction.

The new instruction regression opens real public debt in each asset, applies
12.5% index accrual (including hLP funding), settles funding once, then checks
previews and reverse trades with both hLPs at 1x/4x concentration. This isolates
repeatable pricing credit from the existing one-time funding-recovery reward.
The native tests separately check the first trade after public accrual with
no funding premium, plus repeated cycles and larger receivables.

Full-value settlement has a compute cost. The measured concentrated hLP
funding case increases from 355,721 to 835,599 CU; the new concentrated public
interest case peaks at 881,839 CU. An ordinary concentrated swap falls from
266,593 to 188,344 CU after reusing prepared curve boundaries. The plain
same-slot swap is 97,627 CU, below the unchanged 100,000-CU architectural
limit. The 1,355,000-CU transaction guard is also unchanged. Per-scenario
ceilings are the successful finished-binary measurements plus exactly 5%.
These fixture measurements do not establish that every composition fits;
external hLP flash execution needs its own transaction budget tests.

## Approved follow-up scope, not yet implemented

1. Cash-funded harvesting of accrued public interest for yLP and hLP, bounded
   by available reserve cash. Advances must be tracked through repayment and
   writeoff, including protocol/referral shares and transfer fees. Existing
   harvest currently pays only already-funded liabilities.
2. Before releasing new hLP yield, settle its funding cost and restore its
   hedge. Already-funded historical holder claims cannot be spent on this.
3. External atomic hLP flash liquidation, in addition to native recovery.
   It must preserve the remaining holders' value, bind measured net repayment
   to the flashed backing, lock the affected session, and account for shared
   pool mutations without restoring an obsolete market snapshot.
4. Shared native/flash recovery valuation and loss accounting, corresponding
   SDK builders/previews, instruction-level adversarial coverage, and fresh
   full-PR review before merging or deploying.

The recovery objective is to return the vault to its intended hedge. In a
balanced constant-product pool this is 2x: gross LP value is twice equity and
the opposite-token claim equals funding debt. Under concentration, fixed 2x
and an opposite-token-neutral hedge can differ. The confirmed target for
external recovery is neutral opposite-token exposure, with variable gross
leverage under concentration; no literal 2x constraint is being introduced.

This candidate does not close the complete hLP audit item or implement the
new harvest/flash interfaces. See [the remediation ledger](audit/MODEL_REVIEW_CLOSEOUT.md).
