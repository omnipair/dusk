# Liquidation feasibility remediation

Date: 2026-10-11. PR: [#45](https://github.com/omnipair/dusk/pull/45).
Comparison baseline: `5688f38162a95d0e65bd7a13a6946923c36593a4`.

## Follow-up: remove the minimum remaining debt

After the `97889a92` remedy, the user rejected a minimum-debt setting at market
creation. The follow-up removes both fields from `MarketConfig`, launch arguments,
SDK helpers and scripts, together with both liquidation paths' residual-debt
restrictions. This supersedes the pending minimum-debt choice described in the
frozen comparison below. Any useful partial may retain positive debt; existing
health, first-recovery and full-close conditions still apply. This changes the
preproduction account/creation interface, so program and SDK artifacts must be
updated together; no production account migration is asserted or implemented.

The checked V1 source (`omnipair-rs` at `84a8ef7`) has no configurable minimum
remaining debt. It rounds its 50% close-factor share burn upward. The follow-up
does not import V1's impact-based health calculation or liquidation sizing.

Follow-up verification passed the full mandatory local CI sequence, including
493 Dusk tests in each default/production profile, 16 delegate tests, 1 faucet
test, both regenerated program interfaces, SDK/TypeScript checks and 134 strict
LiteSVM tests covering 66/66 instructions. The benchmark-feature suite passed
525 tests. All 576 economic CSV rows exactly match the prior one-atom-default
matrix. A replacement native regression verifies useful flash and internal AMM
partials in both debt directions without the former residual floor, and confirms
that a useful partial still prevents full closure. Historical fixture adapters
were updated for removal of the 16-byte config; their original captures are
unchanged.

## Outcome

The two reproduced liquidation-feasibility defects are addressed together:
partial flash quotes can reduce the buyer discount to preserve health, and
emergency sizing accounts for actual AMM fees while considering those newly
available flash prices. This restores valid execution paths without treating a
failed keeper quote or an exhausted proof budget as permission for a full sale.
It does **not** establish universal liquidation liveness or lower losses on every
price path. All required local CI gates passed before this change was committed.

## What changed

| Finding | Remedy | Verification |
| --- | --- | --- |
| NM-001: the incentive ramp can make a previously useful partial fail | Treat the ramp as a ceiling. Search for the highest whole-basis-point discount between 0.5% and that ceiling which improves the remaining position after collateral debits and the 1% charge. Reject if none works. | Both debt directions and both borrowing/leverage ledgers; actual begin/payment/settle transactions at 5.5% equity and near the minimum discount. |
| NM-002: an optimistic fee-free partial can block real emergency cleanup | Include base AMM fees in interval bounds; evaluate surviving AMM inputs with actual fee math and the same repayment/share allocation as execution. Flash bounds include the minimum permitted discount. | Exhaustive small integer comparisons, all three swap-fee collection modes, capped token fees, raw token-scale proof, first recovery crossing and actual positive-equity full liquidation. |
| NM-003 / original #287245: an oversized repayment cap is checked against wallet balance | Remove the early balance-versus-maximum check. Retain checks against the exact computed gross transfer and the caller's maximum. | Both public repayment aliases with Token-2022 fees, a wallet smaller than the cap but sufficient for repayment, insufficient actual funds, and third-party repayment. |
| NM-004 / original #287621: documentation promises a nonexistent concentration ramp | Describe concentration updates as immediate on validated proposal execution. | Program and SDK documentation agree with existing runtime; no new governance ramp. |
| NM-005: captured VOB fixture cannot deserialize | Adapt the historical test fixture to the current layout with neutral values for new fields; preserve captured economic data. | All three captured VOB regressions and the complete benchmark-feature suite pass. |

### Discount example

With collateral worth 1,000 debt tokens, debt of 945, and a requested repayment of
100, the charge is 1. A 5% discount would consume about 106.316 of collateral and
worsen the remaining equity ratio. At the tested raw-token precision the quote
instead selects **4.55%**, consumes about 105.815, and leaves 845 debt with a
slightly improved equity ratio. The emergency gate remains closed in this 7% MM
example. The quote restores a permitted partial; it does not promise that a buyer
will profit or that a single fill reaches the MM + 2-point recovery target.

The original NM-002 example (1,000 collateral value, 970 debt, 3% AMM fee) now has
a useful cheaper flash partial. Therefore that example correctly continues to
refuse a full close. Separate tests verify full access when no useful partial can
be certified and the positive-equity emergency conditions otherwise permit it.

### Bounds, rounding and compute

- The discount search prunes intervals using their best possible remaining value
  and lowest maintenance rate. It does not assume exact health is monotonic
  across integer rounding or tier boundaries.
- The emergency proof uses separate flash and AMM share-rounding bounds. It
  retains optimistic treatment of constraints not included in the proof, such
  as physical cash. Uncertainty can refuse a full sale; it cannot authorize one.
- Flat-maintenance, untaxed valuation cases use an algebraic bound to avoid
  enumerating raw token atoms. Remaining exact AMM checks reuse prepared curve
  state and pure repayment calculations rather than allocating account clones
  on each iteration.
- A bounded proof can still run out of work or transaction compute. Rejection
  remains the safe outcome. The instruction regressions cover the reproduced
  cases, not every possible account value or route.
- The existing 70%-of-MM emergency boundary, loss waterfall, insurance limits,
  first-recovery caps and 0.5% minimum discount remain unchanged.
- At `97889a92`, the configured minimum alone did not grant full-close permission.
  The follow-up above removes the economic minimum itself; it adds no new
  full-close permission.

## SDK and integration

`liquidationIncentives()` now exposes `maximumBuyerDiscountBps`. Its old
`buyerDiscountBps` property remains as a deprecated alias for the ceiling. The
executable discount is `preview.rates.buyerDiscountBps`; route builders receive
that quote. A client must use the complete preview payment, including transfer
fees and rounding allowances, rather than reconstructing payment from the ramp.

There are no new serialized instruction accounts, arguments, events or account
layout changes in this remedy. Both program IDLs were regenerated and the
vendored interfaces remained unchanged. Frontend, keeper, indexer and deployment
follow-ups are not represented as shipped by this repository change.

## Native economic replay

The permanent benchmark test reruns the same **576 deterministic combinations**
as the baseline audit. It varies concentration (1x/4x/10x), hLP presence, debt
direction, transfer fees (0/50 bps), requested opening equity (20%/50%), one or
three positions, three declining/gap paths, and Dusk-only versus hypothetical
external execution. One path withdraws 25% of yLP supply. Native interest accrual
and actual native settlement/accounting transitions are included.

The external venue is an explicit model: a debt-side depth of 100,000 units,
constant-product execution, a 30 bps venue fee and token transfer fees. It is not
a real Jupiter route. Keeper selection samples a finite fill-size grid, requires
50 raw atoms of modeled profit, and limits repeated fills per step. Failure to
find a fill is therefore not proof that every possible amount is infeasible.

Of the 576 combinations, 288 admit no requested entries. Another 16 admitted
paths stop at the shock-input search cap. The comparison below uses the remaining
**272 completed admitted paths**, 136 for each venue assumption. These deliberately
stressed cases are not estimates of real-world probabilities.

| Completed-path measure | Dusk only: before → after | With modeled external venue: before → after |
| --- | ---: | ---: |
| Cases ending with an eligible position | 32 → 32 | 6 → 4 |
| Cases realizing principal write-off | 104 → 104 | 104 → 106 |
| Matched cases with less / more / equal write-off | 0 / 7 / 129 | 8 / 11 / 117 |
| Accepted settlement or market-invariant failures | 0 → 0 | 0 → 0 |

The maximum principal write-off remains **3,585.678 debt-token units** across the
matrix. The two external cases newly realizing write-off previously ended with
eligible positions; they now close with 14.114 units written off each. More cleanup
can realize a loss that had remained outstanding in the baseline.

Other changed outcomes are also material. For example:

| Scenario (all use 50% requested opening equity) | Before write-off | After write-off |
| --- | ---: | ---: |
| External; 1x concentration; hLP; base debt; no transfer fee; one position; 20% gap + withdrawal | 2,399.140 | 1,321.478 |
| External; 10x concentration; hLP; quote debt; no transfer fee; three requested positions; 45% gap | 903.908 | 1,769.365 |
| Dusk only; 10x concentration; hLP; quote debt; 50 bps transfer fee; three requested positions; gradual decline | 1,446.349 | 1,606.110 |

Amounts are in each scenario's debt token, not a common USD numeraire. The tested
policy alters which partials and full closes are permitted and therefore the
subsequent sequence of swaps and recovery. These examples prevent interpreting
the remedy as a uniformly superior economic policy. In particular, a theoretically
useful flash partial can still lack a profitable actual buyer and continue to
block a full close. The remedy does not establish external liquidity availability.

Data: [baseline matrix](liquidation-feasibility-before.csv) ·
[remediated matrix](liquidation-feasibility-after.csv).

### Reproduce

```sh
cargo test -p dusk --lib --features benchmark final_economic_policy_paths -- --nocapture
```

Rows prefixed `FINAL_ECON,` contain the CSV header and data. Monetary columns use
1,000 raw atoms per modeled token. Scenario keys are `amp,hlp,side,fee_bps,
wallet_bps,pieces,path,external,withdrawal`; side 0 is base debt, side 1 quote debt.
Paths 0/1/2 are gradual decline / 45% gap / 20% gap with LP withdrawal. Count only
`opened > 0` and empty `path_error` as completed admitted scenarios. The baseline
matrix is frozen evidence from the prior audit; the current harness reproduces
the after matrix. New assertions require zero accepted-settlement failures and
no recorded market-invariant failure.

## Validation and review scope

All mandatory local CI steps passed: formatting, hygiene, code shape, clippy,
default and production Dusk suites (**493 tests each**), production build check,
leverage delegate (**16**), faucet (**1**), TypeScript, full LiteSVM program builds,
build identity, IDL preparation, SDK checks, and the complete strict LiteSVM suite
(**134 tests; 66/66 instructions exercised; no pending tests**). The additional
benchmark-feature suite passed **525 tests**, including the captured VOB replay
and economic matrix. Five focused LiteSVM regressions also passed separately.

The final source review follows the prior Nemesis findings through invoice
construction, share rounding, recovery/full permission, session-bound terms,
repayment validation, SDK consumers and the frozen original-audit ledger. This is
verification of the remedies, not a new exhaustive audit of the entire PR, a
formal proof, or a replay of the original supplied exploit PoC.

## Original audit status and remaining decisions

The prior review traced all 35 originally unreviewed subjects from the 169-report
export. With #287245 and #287621 now fixed, that ledger becomes **31 root causes
remediated, 2 superseded by removal of the old auction path, and 2 retained risk
items**. The other 134 reports' original Invalid classifications were inventoried;
this work does not claim to have independently revalidated each one.

- **#287652: emergency execution-price risk remains open.** The caller can choose
  zero minimum output. EMA eligibility, partial-first permission and capped
  insurance do not provide a protocol execution-price floor. No profitable
  manipulation exploit is established here. This needs explicit risk disposition
  or a separately approved mitigation; atomic settlement alone does not close it.
- **#287160: mutable transfer-fee collateral risk remains.** Net-credit accounting
  and adverse scheduled-fee checks improve handling, but a hostile issuer can
  destroy recoverable collateral value. Principal-only insurance and write-off
  handle the resulting accounting loss; they do not prevent it.
- **Minimum remaining debt — resolved by removal:** the follow-up above removes
  the fields and restrictions. No market creator chooses a number, and no
  minimum-based exception to full-close permission is needed.

PR #45 remains draft. No deployment or merge is performed by this remediation.
