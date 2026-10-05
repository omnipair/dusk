# Flash purchase candidates — preliminary

2026-10-05. Exploratory CPMM results; no runtime settings selected.

## Selected-policy assumptions

EMA-only ordinary eligibility, stored-depth marginal MM, IM >= MM + 3 points plus crowding; partial fills may improve health without fully restoring the target. Fixed-payment buyers keep execution upside. Critical EMA equity permits internal emergency execution without a timer.

Candidate inputs: 0.5–3% health-based discount; 0.2% solvent insurance contribution; internal caller reward up to 1% of proceeds (mechanism approved, numeric cap unselected). Compare old 20%/60% depth bands with steeper rates above 5%/15% of stored collateral-side depth. These are experiments, not defaults. All candidates retain a 7% first-band maintenance rate.

## Matched request: $100k pool, $15k total entry spending, 50% wallet equity

Rows use critical equity = half maintenance and no outside venue. A fast sample is a 3% spot decline every ten seconds with a 60-second EMA; slow is 0.5% per minute. These are stress assumptions, not forecasts. Different admission counts require care.

| MM rates (depth bands) | Positions requested/opened | Path | Admitted debt | Partials | Emergency sales | Principal loss after insurance | Insurance drawn | Unresolved eligible |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 0.07/0.08/0.1 (0.2/0.6) | 1/1 | slow | 7500.00 | 2 | 1 | 862.36 (11.50%) | 200.06 | 0 |
| 0.07/0.08/0.1 (0.2/0.6) | 1/1 | fast | 7500.00 | 0 | 1 | 2281.47 (30.42%) | 200.00 | 0 |
| 0.07/0.08/0.1 (0.2/0.6) | 10/10 | slow | 7500.00 | 4 | 10 | 0.00 (0.00%) | 61.59 | 0 |
| 0.07/0.08/0.1 (0.2/0.6) | 10/10 | fast | 7500.00 | 0 | 10 | 1847.72 (24.64%) | 500.00 | 0 |
| 0.07/0.12/0.3 (0.05/0.15) | 1/1 | slow | 7500.00 | 2 | 1 | 431.13 (5.75%) | 200.07 | 0 |
| 0.07/0.12/0.3 (0.05/0.15) | 1/1 | fast | 7500.00 | 0 | 1 | 1992.97 (26.57%) | 200.00 | 0 |
| 0.07/0.12/0.3 (0.05/0.15) | 10/10 | slow | 7500.00 | 4 | 10 | 0.00 (0.00%) | 61.59 | 0 |
| 0.07/0.12/0.3 (0.05/0.15) | 10/10 | fast | 7500.00 | 0 | 10 | 1847.72 (24.64%) | 500.00 | 0 |
| 0.07/0.2/0.4 (0.05/0.15) | 1/1 | slow | 7500.00 | 3 | 1 | 172.32 (2.30%) | 200.10 | 0 |
| 0.07/0.2/0.4 (0.05/0.15) | 1/1 | fast | 7500.00 | 0 | 1 | 1682.12 (22.43%) | 200.00 | 0 |
| 0.07/0.2/0.4 (0.05/0.15) | 10/10 | slow | 7500.00 | 4 | 10 | 0.00 (0.00%) | 61.59 | 0 |
| 0.07/0.2/0.4 (0.05/0.15) | 10/10 | fast | 7500.00 | 0 | 10 | 1847.72 (24.64%) | 500.00 | 0 |
| 0.07/0.25/0.45 (0.05/0.15) | 1/0 | slow | 0.00 | 0 | 0 | 0.00 (0.00%) | 0.00 | 0 |
| 0.07/0.25/0.45 (0.05/0.15) | 1/0 | fast | 0.00 | 0 | 0 | 0.00 (0.00%) | 0.00 | 0 |
| 0.07/0.25/0.45 (0.05/0.15) | 10/10 | slow | 7500.00 | 4 | 10 | 0.00 (0.00%) | 61.59 | 0 |
| 0.07/0.25/0.45 (0.05/0.15) | 10/10 | fast | 7500.00 | 0 | 10 | 1847.72 (24.64%) | 500.00 | 0 |

## Limits before selecting defaults

This model shares the previous independent CPMM formulas, not a new native implementation. It omits token transfer fees, accrued interest in the sweep, concentrated curves, hLP, public borrowing, adversarial price/depth additions, instruction locks and cross-position composition. Insurance starts at 1% of pool value, with 20% event / 50% window caps. The debt side includes principal receivables; cash constraints are checked. A partial search uses a 1% grid; keepers may execute repeated useful fills at each observation. No claim of best execution, exact minimal seizure, or guaranteed cleanup follows from these results. Fees and numerical thresholds require further native coverage.

The larger-MM candidates deliberately show the leverage cost of earlier intervention. Neither their loss rates nor the gentle schedule have an approved loss budget. Do not remove the existing admission cap based only on this preliminary comparison.
