# hLP accounting regressions and joint-shock replay

**Scope correction:** the joint-shock insurance figures below come from the
benchmark-only historical auction floor. They are **not** current emergency or
flash liquidation capital estimates. The historical floor can insure unpaid
interest and 100% of a principal gap subject to draw capacity; current full
liquidation cancels unpaid interest and covers at most the configured 75% of a
principal gap. The companion `policy-contrast.csv` reports the difference at
each funded historical floor's observed recovery. Because the live liquidation
can have a different recovery and execution path, the contrast is a policy
bound, not a live-path loss or capital projection.

## Scope and implementation already present in #45

The original hLP regressions targeted `0b2c53f`, the head of #45 when this
report began. The refreshed joint-shock replay and policy contrast use the
later `c32575c8` program tree with this replay correction; the generated
manifest records exact source hashes. The earlier branch already contained
`3162901` (`fix: reconcile hLP quotes with recorded debt`)
with mixed-decimal/ownership rounding corrections. Both bugs found in
the earlier `22114ad` analysis snapshot are already corrected here:

- The quote preserves actual indexed hLP debt instead of synthesizing a new
  starting debt from rounded NAV and reserve ratios.
- Funding interest is reserved before quoting and paid once; settlement does
  not subtract it from the endpoint again. Proportional-claim certification
  accounts for the actual rounded ownership.

The older analysis candidate instead retained stored funding reserves and
adjusted the endpoint after quoting. Applying that patch over #45 would replace
its newer working implementation. This change therefore adds permanent
regressions and reproducible economic validation, with **no runtime, interface,
account-layout or risk-policy change**. The three-atom reserve guard remains.

## Regression coverage

`programs/dusk/src/tests/transitions/hlp_joint_shock.rs` uses native market
initialization and liquidity entry for the $1m, SOL $200, 9/6-decimal, 5x fixture.
It checks $25k/$100k hLP equity per vault, both swap directions, and 30/900/86,400
seconds of actual funding accrual. It verifies exact starting reserves, one cash
payment of interest, bounded opposite-asset claims and a second same-slot swap
that must not pay cleared interest again.

Both new native tests pass. Disposable engine copies reintroduce each defect
independently: synthetic starting debt fails the exact reserve assertion, and an
extra interest deduction fails the unchanged `BrokenInvariant` guard. The
mutation results are in `joint-shock-mutation-checks.json`.

Two LiteSVM tests use real 9/6-decimal token accounts, both active hLP vaults and
both directions, at entry and after one day of slot-based accrual. They compare
preview and execution, reconcile physical owner/reserve/interest balances,
verify the exact indexed funding payment, check yLP custody, and bound the
opposite-asset claim. They do not edit debt indexes or token balances.

## SVM compute regression ceilings

A complete successful 125-test LiteSVM run measured these new paths. Each
ceiling is exactly `ceil(measured maximum * 1.05)`; existing ceilings are
unchanged. Both named scenarios are mandatory in the complete-baseline check.

| New scenario | Maximum CU | Strict ceiling CU |
|---|---:|---:|
| SOL/USDC hLP, same slot | 449,203 | 471,664 |
| SOL/USDC hLP, one day of funding | 558,895 | 586,840 |

The complete local CI sequence passed: formatting, hygiene, code shape,
Clippy, 477 Dusk tests in each default/production profile, production checking,
16 delegate tests, one faucet test, TypeScript, SBF/fixture builds, unchanged
program identities, SDK interface checks and 56 SDK tests. The final LiteSVM
run passed 125 tests with `DUSK_REQUIRE_COMPLETE_CU_BASELINE=1` and
`--forbid-pending`, exercising 67/67 instructions. It measured 2,939 successful
transactions, with a 1,293,630-CU maximum below the 1,355,000-CU repository limit.

## Completed native losses

The main book has $1m opening LP equity: $800k ordinary, $100k base hLP and $100k
quote hLP; hLP leverage is 2x, separately from 5x curve concentration. Ten public
loans total $75k USDC. These rows request no withdrawals or extra keeper delay.
All ten loans liquidate at the 30%, 50% and 70% gaps. At 10%, no credit shortfall
is realized during the one-hour horizon.

| SOL gap | Historical uninsured credit loss, USDC | Historical funded payout, USDC | Historical auction opening USDC | Change from prior historical estimate |
|---|---:|---:|---:|---:|
| -10% | 0.00 | 0.00 | 0.00 | 0.00 |
| -30% | 3,861.80 | 3,774.07 | 7,548.13 | -1,629.18 |
| -50% | 26,200.98 | 25,071.69 | 50,143.38 | +13.23 |
| -70% | 45,018.55 | 43,726.89 | 87,453.78 | +19.77 |

The maximum historical-auction USDC requirement in these rows is
**87,453.777248 USDC**: round up to 87,454 whole USDC. The earlier 87,435-USDC
historical planning balance no longer covers this run. None of these numbers
establish a live-policy capital requirement. The changes from the older analysis
reflect the merged engine, including elapsed-time accounting; they are not
bit-for-bit equivalence claims. Across the funded corpus, the largest absolute
USDC capital change is $1,629.183687 and the largest SOL change is
1.022368566 SOL.

Insurance restores reserves during liquidation and changes later execution.
Historical-auction capital is sized on the funded path and verified with a
native replay, rather than inferred by multiplying the uninsured loss. The
historical floor uses the configured 50% daily draw limit; no new insurance
credits arrive during the modeled liquidation window. The 30%, 50% and 70%
main funded cases are verified at that daily-cap boundary.

At -50%, the historical uninsured credit loss is allocated to ordinary LPs
(16,731.100808 USDC), base hLP (2,588.345432) and quote hLP (6,881.530969),
using ownership at each loss event. Collateral pays first, then insurance;
uncovered losses reduce the shared reserve. hLP is not a first-loss protection
tranche.

Those credit losses are already included in holder P&L. With sufficient
insurance, ordinary LP dollar P&L at -50% is still -243,514.98, base hLP is
-49,944.10 and quote hLP is +117.97. Credit insurance does not remove SOL exposure
or AMM inventory repricing.

For SOL public debt, the historical +100% price shock can require hundreds of
SOL in this corpus. The largest historical SOL requirement across hLP loads is
265.085429680 SOL. These are different borrowing books, not a combined
simultaneous two-sided capital requirement.

## Current-policy coverage bound at historical recovery

The runner now records principal and configured coverage for every historical
floor, then compares its payout with the current policy's 75%-of-principal-gap
ceiling at that floor's observed swap recovery. Across 670 funded-floor events,
597 historical insurance draws exceed this same-recovery ceiling. This does not
predict the actual live liquidation's recovery or writeoff.

The severe-shock control makes the discrepancy explicit. Its first borrower has
7,500,000,000 principal atoms and 7,500,002,205 debt atoms. The historical floor
recovers 568,776,768 atoms and draws 6,931,225,437 insurance atoms. At that
same recovery, the current 75% principal-gap ceiling is 5,198,417,424 atoms:
the historical payout exceeds it by 1,732,808,013 atoms (1,732.808013 USDC).
Even with zero recovery, live insurance could not exceed 5,625,000,000 atoms
for that principal. A separate replay through current emergency or flash
liquidation is needed for live-policy capital estimates.

## Replay coverage and reproduction

The input JSON retains all 104 previous scenarios. **102 complete, zero hit the
settlement blocker, and two attempted borrowing configurations fail admission.**
The failed $150k/$200k setups do not establish universal capacity ceilings.
All 66 positive-shortfall paths were funded and rerun at the calculated minimum
and one raw debt-token atom less. **All 132 boundaries pass:** the exact minimum
leaves no socialization, while one atom less leaves a positive shortfall.

Across 327 native replays, 2,850 liquidation events and 8,078 custody/preview
checks reconcile. All 32 focused probes pass; the starting gap is zero and the
largest final opposite-asset residual is one token atom.

Run `python3 scripts/joint-shock-replay/run.py`. Its README records assumptions
and outputs. The adapter invokes the benchmark-only atomic backstop transition and
existing hLP token-settlement helpers in a copied benchmark engine. Raw results,
source hashes and logs remain under `target/joint-shock-results`; summary CSVs,
including the same-recovery policy contrast, and validation results are checked
in alongside this report.

The auction replay is a deterministic native historical model, while the SVM
tests verify actual program/token execution on the focused fixtures. Neither
establishes scenario probabilities, historical tail frequencies, insurance
premiums, repeated-shock
capital, multi-market contagion, or coverage for transfer-fee assets.
