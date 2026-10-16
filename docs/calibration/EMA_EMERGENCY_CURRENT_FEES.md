# EMA-priced emergency settlement with current fees and hLP

**Current disposition, 2026-10-17:** #287652 is closed after the EMA-price/
smoothed-depth implementation and explicit user acceptance of residual inventory/
arbitrage risk. See the [current evidence and acceptance record](EMA_SETTLEMENT_CLOSEOUT.md).
This document preserves earlier proposals/results; its open-finding statements
refer to that historical stage and do not override the current disposition.

Date: 2026-10-16 (Asia/Makassar). PR #45 / `6da3f7785b2e4523a74843b21980a1b3f9b128fb`; GitHub head verified during this experiment.

## Verdict

**The tested EMA-pricing substitution does not resolve #287652.** It stabilizes the quoted recovery against an immediate spot move, but a full internal liquidation still transfers collateral into the pool and extinguishes the loan. The attacker can profit from trading around that inventory change. Lower recorded write-offs do not establish higher final LP value.

No runtime code, fees, liquidation permissions, instruction interfaces or deployment were changed. This is a native economic counterfactual, not a deployed implementation or an instruction-level exploit demonstration.

## Scope and controls

The experiment replaces pricing only in the existing emergency route. Ordinary flash settlement is not replaced or added to its permission rules. It retains V2 linear symmetric-EMA health, the 70%-of-maintenance emergency gate, current stored size-margin terms, improving partials, the integer first-recovery/full-close certificate, time/health rewards, and the principal-first loss waterfall. The certificate uses the same candidate curve as the corresponding settlement; a failed sampled partial is never itself full-close permission.

Current native fee code is used: the fixture's 0.3% base fee, configured divergence coefficient 10 and existing surcharge caps/retention, volatility state, 20% protocol share of claimable swap fees, current 1% solvent-liquidation charge and its split/taper, and the progressive emergency reward. These are explicitly configured scenario values, not a claim that every market has identical settings. Insurance starts at 1,000 debt tokens, with current 75% principal coverage and 50% daily draw budget (500 initially).

Six reference candidates combine:

| Depth choice | Construction |
| --- | --- |
| Live depth | Preserve the concentrated curve's current depth, reconstruct at symmetric EMA |
| Frozen depth | Use pre-shock depth as an idealized slow/frozen depth EMA |
| Share-adjusted frozen depth | Scale that reference by current ordinary yLP shares / reference ordinary shares |

Each is tested with (a) the complete native fee formula evaluated on the reference path, retaining real launch/base/volatility inputs, and (b) a sensitivity retaining at least the full live-curve fee amount. No base-only fee exemption is introduced. The frozen-depth experiment is a deliberately stable reference; it does not claim to implement or calibrate a new on-chain depth oracle. In particular, it is not a direct use of `min(observed_depth, ema_depth)`.

The reference geometry preserves the native CPMM/concentrated shape. It is not a constant-product replacement for concentrated markets. Virtual reserves are used only to quote. Actual accounting receives collateral, nets principal, accounts for real payouts/fees/insurance, and uses the native current-state hLP reconciliation kernel. No virtual cash, virtual pool balances or virtual hLP debts are committed. This inventory allocation is the tested counterfactual; other possible hLP loss allocations require separate evaluation.

## Coverage

- **384 starting states × 3 front-trade sizes × 8 choices = 9,216 rows.** Choices are current emergency, six reference candidates and no-liquidation control.
- Concentration 1×/4×/10×; hLP absent/present; both debt directions; collateral 1,000/15,000; gross EMA equity +4%/-2%; external price equal to EMA or 20% below; transfer fees 0/50 bps on both assets; ordinary LP withdrawal 0/25%.
- Same-slot front sales of 0/1,000/5,000 collateral tokens. EMA is unchanged during these trades. Withdrawals and prior price discovery are shared initial conditions, not free attacker actions.
- **48 native accounting parity checks passed:** copied policy quotes equal native quotes, and full-close inventory reconciliation matches native LP/hLP values within 0.01 debt-token units. No application/accounting errors occurred in the matrix. The saved matrix harness was rerun and reproduced all 9,216 result rows exactly.
- **Two additional admitted-position paths** use actual leverage admission, native price-moving swaps and elapsed-slot EMA updates, with and without hLP. Their 42 liquidation comparisons also retain profitable front-trade cases under reference pricing.

The 120-second distress age is an experiment input; keeper inactivity is assumed, not predicted. The matrix begins from accounting-consistent distressed snapshots. The additional paths address reachability for their particular examples; they do not establish reachability of every matrix state. Borrowing shares use the shared settlement kernel, but this experiment instantiates leverage positions: it is not a separate borrowing-instruction or CPI audit.

## Costs and final value

The actor obtains collateral from a finite external CPMM with 100,000 debt-token depth and 30 bps fee, pays applicable token transfer fees, sells into Dusk, triggers the permitted liquidation, and takes the best sampled reverse/arbitrage trade. The external pool carries its reserves across the sequence. Both arbitrage directions and no trade are considered. The search is finite, not a global profit optimum.

The honest comparator also receives its best sampled permitted liquidation and arbitrage. Thus the reported additional profit is **manipulated sequence minus honest sequence**, not total post-liquidation arbitrage. A matched no-liquidation control measures the cost of trading out and back without the liquidation.

Ordinary yLP and both hLP cohorts are marked at the same external price after the reverse trade, including earned fees once. Protocol fees, protected reserves and remaining insurance are separate. A remaining loan's mark-based deficit is also recorded; it must be deducted when comparing risk-adjusted pool resources. The headline full-close cases have no remaining loan or residual deficit.

Accrued interest/funding over time, priority fees, financing charges and competition are not simulated in the matrix. Trading costs and transfer fees are simulated. Additional financing/transaction costs are shown as sensitivities rather than invented estimates.

## Matched example: positive EMA equity, 4× concentration and hLP

Shared starting state: 15,000 collateral, 14,400 principal, EMA and external mark both 1; approximately 14.63% MM; +4% EMA equity. Emergency eligibility already holds before the front trade. Ordinary LPs have withdrawn 25% of their shares. There is no token transfer fee in this example. Both methods permit a full close under their recovery certificate.

The EMA column uses frozen depth **and retains at least the full live fee charge**. Amounts below are debt-token units. All rows include the final reverse trade.

| Metric | Current, honest | Current, front sale 5,000 | EMA, honest | EMA, front sale 5,000 |
| --- | ---: | ---: | ---: | ---: |
| Credited debt recovery | 8,975.221 | 7,121.139 | 9,690.784 | 9,690.784 |
| Insurance used | 500.000 | 500.000 | 500.000 | 500.000 |
| Recorded principal write-off | 4,924.779 | 6,778.861 | 4,209.216 | 4,209.216 |
| Caller reward | 90.658 | 71.930 | 97.886 | 97.886 |
| Actor net profit | 3,755.539 | 6,649.545 | 3,765.687 | 6,692.083 |
| Final ordinary yLP value | 56,958.428 | 54,679.990 | 56,952.750 | 54,655.364 |
| Final base hLP value | 8,603.382 | 8,002.411 | 8,601.658 | 7,995.460 |
| Final quote hLP value | 7,585.540 | 6,432.325 | 7,582.086 | 6,416.844 |
| Combined LP/hLP value | 73,147.350 | 69,114.726 | 73,136.494 | 69,067.668 |

**Additional actor profit from manipulation: 2,894.006 current; 2,926.396 EMA.** The EMA candidate's stable recovery/write-off number does not stop the economic loss.

For the EMA manipulated sequence: external collateral acquisition costs 5,278.995; front sale returns 3,873.572; reverse arbitrage earns 7,999.620; reward is 97.886. Net: **6,692.083**. Against the honest EMA sequence, the extra profit is 2,926.396. Charging an additional 100 units specifically to the manipulated sequence still leaves 2,826.396 additional profit. This is a sensitivity, not an estimate of actual financing/transaction fees.

The matched front-and-reverse sequence **without liquidation loses approximately 895.65**. The LP/hLP loss versus honest EMA execution is 4,068.826. An actor owning a pro-rata share of all LP/hLP cohorts would also bear that share of this loss: extra profit at 0%/10%/50% ownership is approximately 2,926/2,520/892. The model does not assume the actor owns the LP liquidity.

Protocol/protected/insurance totals are also in the CSV. They do not explain away the LP loss: EMA honest values are 12.900 / 3,000.000 / 500.000; manipulated values are 17.100 / 3,834.924 / 500.000.

## Why the write-off improvement can be cosmetic

For a full principal-only close, the entire principal receivable disappears regardless of the quoted split between repayment and write-off. The pool receives the same collateral. If insurance and actual payouts stay the same, relabeling more of that extinguished loan as internally repaid creates no additional external debt tokens.

In simplified debt-asset accounting:

`final cash = initial cash + insurance - actual external payouts`

`remaining position principal = 0`

The reference quote can change rewards, insurance draws or owner surplus. Those are real effects and are included here. It cannot, by valuation alone, undo the price effect of receiving collateral or finance a restoration of the real AMM inventory. In the matched example the higher reference-derived caller reward slightly worsens final LP value.

## Overall results and progress versus cleanup

| Method | Successful honest executions / 384 | Honest positions still eligible afterward | Largest sampled extra manipulation profit |
| --- | ---: | ---: | ---: |
| Current emergency | 384 | 34 | 3,105.638 |
| EMA, live depth | 384 | 68 | 3,137.207 |
| EMA, frozen depth | 384 | 72 | 3,138.972 |
| EMA, share-adjusted frozen depth | 384 | 68 | 3,137.207 |
| EMA, live depth + live fee floor | 350 | 68 | 3,137.207 |
| EMA, frozen depth + live fee floor | 348 | 72 | 3,138.972 |
| EMA, share-adjusted depth + live fee floor | 350 | 68 | 3,137.207 |

Reference pricing sometimes enables a smaller improving partial and avoids an immediate full close. Conversely, retaining live fee charges can make a reference-priced partial infeasible while the reference recovery certificate prevents full closure. These are single-step results; remaining eligibility does not by itself prove permanent stuckness. The finite sampler may miss an executable first-recovery size. None of the candidates establishes guaranteed cleanup or manipulation resistance. These stress counts are not probabilities.

## Check using actual admitted positions

Two 15,000-token gross-spend leverage openings with 50% wallet contribution pass native admission. Both borrow 7,500. Native swaps subsequently bring collateral spot to about 0.60; a native EMA update at slot 1,000 brings the EMA to about 0.604. The positions are then EMA-insolvent. No custom health exemption or hand-written EMA is used. Interest accrual is held fixed to isolate settlement.

With hLP, the position holds 9,788.670 collateral. Using the reference-depth candidate with the full live-fee floor:

| Metric | Honest | Front sale 5,000 |
| --- | ---: | ---: |
| Credited recovery | 4,308.104 | 4,308.104 |
| Recorded write-off | 2,691.896 | 2,691.896 |
| Actor net profit | 731.208 | 1,023.670 |
| Ordinary yLP value | 62,936.609 | 62,379.408 |
| Base hLP value | 6,193.097 | 5,924.755 |
| Quote hLP value | 10,248.482 | 10,156.722 |

Manipulation adds **292.462** profit after modeled trading costs. Current emergency execution adds **285.716** on the same admitted path. Without hLP, the reference candidate also retains positive incremental profit. The previous market decline is exogenous shared history; this does not assume the attacker moves that entire market for free.

## Decision supported by the evidence

The experiment does not establish complete remediation of #287652. Stabilizing repayment does not demonstrate protection of final LP/hLP resources. The subsequent user-approved scope is to remove direct live-price/depth dependence while retaining V2 health and explicitly acknowledging this remaining economic risk. Implementation and regression results belong to the later settlement change, not this counterfactual experiment. This experiment does not rule out a more complete EMA-based design that also changes inventory execution/allocation; such a change would require an explicit economic policy.

## Evidence and reproduction

Local evidence: `.audit/findings/ema-emergency-current/`.

- `results.csv`, `summary.json`, `matrix.log`: complete 9,216-row matrix, parity results and zero application errors.
- `matrix-experiment.rs.txt`: test-only matrix source, including copies of current fee allocation and integer sizing proof.
- `admitted-experiment.rs.txt`, `admitted.log`: clock/general-price extension and admitted-position results.
- `reproduce.py`: temporarily includes the selected harness in the native benchmark test module and restores the source in `finally`.

From this worktree: `python3 .audit/findings/ema-emergency-current/reproduce.py matrix` or replace `matrix` with `admitted`. Temporary native build output is `/private/tmp/dusk-ema-emergency-build`; it is not a program artifact or deployment.
