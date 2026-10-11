# Why the economic replay still has unfilled liquidations

Date: 2026-10-11. Source: PR #45 at `4dd17d5e` plus the diagnostic harness in this change.
The runtime change reduces computation in the existing full-close proof. No fee,
eligibility, full-close permission, accounting formula or SDK interface is changed.

## Findings

The original 576-case matrix is unchanged. Its 272 completed admitted paths end
with 32 eligible Dusk-only positions and four eligible positions under the
modeled external-liquidity assumption. All 36 are on the 20% price-drop plus 25%
yLP-withdrawal path; terminal debt ranges from 2,487.500 to 7,500.003 tokens.
These are not economic dust balances.

1. **Two external cases hit the end of the test, not a permanent execution
   block.** In the 10x-concentration/hLP/no-transfer-fee/single-position cases,
   eligibility starts at the last sample, t=540 seconds. At t=570, the larger
   incentive admits an actual external partial settlement: approximately 750
   debt tokens are repaid. The position becomes ineligible and remains so through
   the extended replay, with no write-off. Both debt directions behave alike.
2. **The original six-size keeper search missed a useful small fill.** In the
   two 1x/no-hLP/50-bps-transfer-fee external cases, adding a 0.25%-of-debt request
   executes another partial. Debt falls from 4,651.346 to 4,639.719 tokens at the
   original terminal time. That does not restore health; both remain eligible
   after the extension. A successful quote alone was not counted: the test
   applies recovery, fee accounting and invariant checks.
3. **Internal execution faces a large configured divergence surcharge.** The
   stress initializer sets `divergence_fee_coefficient_nad = 10 * NAD`. At the
   displaced prices, representative quotes hit the 20% divergence-fee share cap,
   in addition to the 0.3% base swap fee. This is separate from the 1% liquidation
   charge and keeper reward. It is a property of this stress configuration, not
   a claim that all Dusk markets always charge 20.3%. The surcharge follows
   distance from the AMM concentration center; waiting for the liquidation
   incentive ramp does not remove it. In CPMM mode the controller does not move
   the center. Concentrated quotes attempt the normal funded controller step.
4. **Current permission rules can preserve economically unfillable positions.**
   Of the 32 Dusk-only terminal states, 15 are above the 70%-of-MM emergency gate;
   17 are inside it. Sampled ordinary internal routes do not earn the modeled
   keeper cost. Sampled emergency partials mostly fail health improvement; two
   states admit tiny slices whose sampled reward is at most 10 atoms, below the
   50-atom modeled cost. Full closes inside the gate are rejected because the program
   cannot certify that no useful partial exists. A hypothetical flash purchaser
   willing to pay the floor counts toward that protection even if no actual
   route makes the purchase profitable. The 120-second clock caps the incentive;
   it does not override eligibility, partial health or full-close protection.

The native economic paths reproduced no accepted-settlement or accounting failure.
The transaction suite separately exposed a compute-limit failure, fixed below.
The economic stalls above remain search/horizon limitations and policy constraints;
a finite sampled search is not an impossibility proof.

## Continuation results

Each eligible terminal position is continued separately from a clone of its
original market state. Other positions retain their balances. The native engine
continues interest, risk observations, actual swap/settlement transitions and
market-invariant checks. This is a diagnostic continuation, not a fresh full
market or adversarial simulation.

| Eligible positions remaining | Dusk only | Modeled external venue |
| --- | ---: | ---: |
| Original endpoint | 32 | 4 |
| Wider size grid at the same time | 32 | 4 |
| Wider grid plus 20 minutes | 32 | 2 |
| Same extension, divergence coefficient set to zero in the scratch market | 6 | 0 |

The fee counterfactual closes 26 formerly unfilled Dusk-only positions, with
principal write-offs; it is **not** evidence that waiving the fee is safe or that
recovery becomes lossless. It changes the coefficient for all continuation
swaps, including ordinary internal flash routing and any stress trade, rather
than implementing an emergency-only exemption. The test has no new production
fee exception. Removing that surcharge alone still leaves six Dusk-only cases.

### One execution-cost example

For 1x concentration, no hLP, no token transfer fees, one position, base debt:

| Metric at t=540 | Debt-token units |
| --- | ---: |
| EMA collateral value | 7,728.013 |
| Debt | 7,500.002 |
| Full AMM output with configured divergence fee | 5,198.254 |
| Full AMM output with only base swap fee | 6,259.277 |

Both outputs precede the keeper reward and loss waterfall. They are quotes, not
permitted or completed full liquidations: the existing full-close gate refuses
this positive-equity position while a useful partial remains possible in the
proof. A lower sale fee cannot create missing executable liquidity.

Principal recovery is not a complete LP-wealth measure: collected swap fees
remain protocol/LP assets, including protected retained collateral. Internal
principal repayment is netted against the swap rather than arriving as an
external cash transfer. Comparing quoted repayment or write-off alone therefore
cannot establish the net LP benefit of an exemption; retained assets, insurance,
caller payments and the final inventory must also be valued.

## Program defect found and fixed during validation

The strict LiteSVM suite reproduced a valid positive-equity full emergency
liquidation exhausting its compute meter. The previous passing evidence already
recorded 1,354,587 CU, only about 0.04% headroom against that run's reported guard;
address-dependent validation costs made this fragile. The new failing run
exhausted its 1,354,700-CU instruction allowance before completion.

The proof repeatedly recalculated mint decimal scales and passed full
concentrated geometry through CPMM-only interval quotes. It now:

- Reuses the already validated collateral/debt decimal scales for exact checked
  multiplication and floor division.
- Uses the canonical `cpmm_amount_out_nad` formula directly for CPMM geometry;
  concentrated curves retain their existing quote path.

This is equivalent integer arithmetic, with the same output rounding, base-fee
bounds, partial-health checks and conservative full-close certificate. It does
not waive costs, relax a proof bound or treat compute exhaustion as permission.
The focused transaction completed at approximately 1.176 million CU. Its
regression now requires at most 1.25 million CU and checks the specific
`LiquidationRepayTooLarge` rejection when a useful partial still exists, so a
compute failure cannot masquerade as successful protection.

The matrix continuation and this instruction-level defect are separate findings:
fixing compute does not supply missing route profitability in the 32/2 remaining
modeled continuations.

## Product choices still required

- Whether emergency AMM sales should pay the full divergence surcharge, have a
  separate cap, or pay only the base fee. An exemption changes LP protection and
  caller incentives and needs manipulation/self-liquidation analysis before
  adoption. Ordinary external venues retain their own fees.
- Whether a theoretical flash partial should continue to block a full emergency
  sale when no profitable route exists. On-chain code cannot prove the absence
  of all outside liquidity from a keeper's failed quote. Relaxing that rule can
  force full closure of positive-equity positions and realize losses earlier.

The user was asked about the fee treatment first. No decision is inferred from
these experiments. The emergency price-execution risk in original finding
#287652 is not closed by this investigation.

## Reproduce and interpret

```sh
DUSK_LIQUIDATION_DIAGNOSTICS=1 cargo test -p dusk --lib --features benchmark final_economic_policy_paths -- --nocapture
```

The original `FINAL_ECON` rows remain identical to the previous 576-row matrix.
Diagnostics print `UNFILLED_STATE`, `UNFILLED_COST`, `UNFILLED_GATE` and
`UNFILLED_CONTINUE` records.
The finite-horizon cases also assert successful settlement, restored eligibility
health and zero write-off in the extended modeled-external replay.

- [Terminal states](unfilled-liquidations-states.csv)
- [Execution costs](unfilled-liquidations-costs.csv)
- [Full-close gates and sampled partials](unfilled-liquidations-gates.csv)
- [Continuation results](unfilled-liquidations-continuations.csv)

Scenario IDs are `amp-hlp-debt_side-transfer_fee_bps-wallet_bps-pieces-path-external-position_index`.
Money uses 1,000 raw atoms per modeled token; it is not a common USD measure.
NAD rates use 1,000,000,000 = 100%. The wider repayment/collateral fractions are
50%, 20%, 10%, 5%, 2.5%, 1%, 0.5%, 0.25%, 0.1%, 0.05% and 0.01%, with at most
100 fills per sample. The extension uses 40 additional 30-second samples.

The external venue remains a hypothetical 100,000-token debt-side CPMM at the
path mark, charging 30 bps plus both token transfer fees. Profit must cover 50
raw atoms. External depth is reset for each quote, not depleted across fills.
The stress helper pushes an overpriced Dusk pool down to the path mark; it does
not simulate arbitrage restoring a pool pushed below that mark. No outside
keeper competition, real routing liquidity or guaranteed cleanup is assumed.

## Validation

All mandatory local CI gates passed after the compute fix: formatting, hygiene,
code shape, Clippy, 493 Dusk tests in both default and production profiles,
production check, 16 delegate tests, one faucet test, TypeScript, all program
builds, build identity, SDK checks and the strict LiteSVM suite. LiteSVM passed
134 tests with 66/66 instructions covered; the repaired full emergency used
1,177,160 CU with approximately 13% headroom against the reported guard.
The additional benchmark suite passed 525 tests. The opt-in diagnostic replay
passed and exactly matched all 576 original economic rows, 36 terminal states,
144 cost quotes, 36 gate classifications and 108 continuation rows. The program
optimization therefore changes none of the captured economic outcomes.
