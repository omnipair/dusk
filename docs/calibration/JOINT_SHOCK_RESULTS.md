# hLP accounting regressions and joint-shock replay

## Scope and implementation already present in #45

This replay targets `0b2c53f`, the head of #45 at the start of this work. That
branch already contains `3162901` (`fix: reconcile hLP quotes with recorded debt`)
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

| SOL gap | Uninsured credit loss, USDC | Funded payout, USDC | Required opening USDC | Change from prior capital estimate |
|---|---:|---:|---:|---:|
| -10% | 0.00 | 0.00 | 0.00 | 0.00 |
| -30% | 3,860.34 | 3,771.63 | 9,177.15 | -0.17 |
| -50% | 26,202.66 | 25,064.75 | 50,129.51 | -0.65 |
| -70% | 45,014.11 | 43,716.75 | 87,433.49 | -0.52 |

The exact maximum USDC requirement is **87,433.490576 USDC**: round up to 87,434
whole USDC. The earlier 87,435-USDC planning balance still covers this replay.
The three main credit-loss changes are below $0.25, and capital changes below
$0.65. Across all other load/control variants, the largest absolute USDC capital
change is $6.110666; the largest SOL capital change is 0.030942406 SOL. These are
explicit comparisons between engine versions, not bit-for-bit equivalence.

Insurance restores reserves during liquidation and changes later execution.
Required capital is therefore sized on the funded path and verified with a new
native replay, rather than inferred by multiplying the uninsured loss. The
current caps are 20% of remaining insurance per event and 50% per day; no new
insurance credits arrive during the modeled liquidation window. The 30% main
case binds an individual-event cap; the 50%/70% cases bind the daily cap.

At -50%, the uninsured credit loss is allocated to ordinary LPs (16,736.905249
USDC), base hLP (2,582.781005) and quote hLP (6,882.970667), using ownership at each
loss event. Collateral pays first, then insurance; uncovered losses reduce the
shared reserve. hLP is not a first-loss protection tranche.

Those credit losses are already included in holder P&L. With sufficient
insurance, ordinary LP dollar P&L at -50% is still -243,494.55, base hLP is
-49,940.58 and quote hLP is +116.72. Credit insurance does not remove SOL exposure
or AMM inventory repricing.

The continuing decline to -51%, with a 900-second keeper delay and a blocked
50% withdrawal request, realizes 24,188.755525 USDC of uninsured credit loss.
For SOL public debt, the +100% price shock needs 250.648196306 SOL insurance in
the main hLP configuration. The largest SOL requirement across hLP loads is
264.852096822 SOL. These are different borrowing books, not a combined
simultaneous two-sided capital requirement.

## Replay coverage and reproduction

The input JSON retains all 104 previous scenarios. **102 complete, zero hit the
settlement blocker, and two attempted borrowing configurations fail admission.**
The failed $150k/$200k setups do not establish universal capacity ceilings.
All 66 positive-shortfall paths were funded and rerun at the calculated minimum
and one raw debt-token atom less. **All 132 boundaries pass:** the exact minimum
leaves no socialization, while one atom less leaves a positive shortfall.

Across 326 native replays, 2,840 liquidation events and 8,091 custody/preview
checks reconcile. All 32 focused probes pass; the starting gap is zero and the
largest final opposite-asset residual is one token atom.

Run `python3 scripts/joint-shock-replay/run.py`. Its README records assumptions
and outputs. The adapter invokes the current atomic backstop transition and
existing hLP token-settlement helpers in a copied benchmark engine. Raw results,
source hashes and logs remain under `target/joint-shock-results`; summary CSVs
and validations are checked in alongside this report.

The replay is a deterministic native model, while the SVM tests verify actual
program/token execution on the focused fixtures. Neither establishes scenario
probabilities, historical tail frequencies, insurance premiums, repeated-shock
capital, multi-market contagion, or coverage for transfer-fee assets. No program
has been deployed or merged by this work.
