# V1 controller reuse and market configuration

31 October 2026. This note supersedes the numerical recommendation in the earlier
calibration study. The user selected per-market configuration with governance
updates, using V1's rate-controller mechanism. Further searches for one fixed
configuration for every market are not required.

## Current defaults

| Parameter | Default | Meaning |
| --- | --- | --- |
| Signal lower / upper bound | 5% / 6.25% | Fixed bounds until governance changes them |
| Rate half-life | 14 days | Configurable per market |
| Initial APR | 4% | Configurable new-market rate; not a reset on model replacement |
| Minimum APR | 2% | Configurable floor |
| Maximum APR | 0 / uncapped | Configurable; zero means no economic ceiling |

Both band bounds, half-life, initial APR, minimum APR and maximum APR can be
supplied at market creation. Omitted inputs use the defaults above.
Governance can replace a market’s configuration later; changing initial APR does
not reset its existing rate. The defaults do not override supplied valid values.

The user's 20–25% target means junior's share of total LP capital. For the simple
one-asset balance sheet without unpaid interest or other claims, translate it once
at an 80% reference utilization:

```
R = (J / D) * (1 - U) = (J / total LP capital) * (1 - U) / U
R_lower = 20% * 20% / 80% = 5%
R_upper = 25% * 20% / 80% = 6.25%
```

The 80% value is a reference used to express the defaults, not a live conversion
factor for the target band. Recomputing these bounds from live utilization would
cancel the liquidity weighting in the controller. At other utilizations, the
equivalent junior share changes. In a two-asset market, use consolidated debt
value and each asset's utilization; the simple 20–25% equivalence also requires
both assets to be at the reference utilization.

The separate default borrowing limit remains 80% per asset. The seed, income
split, loss waterfall and exit rules remain as specified in [the plan](../PLAN.md).
All of these are market settings rather than an assertion that one calibration
is optimal for every pair. Record changes and market configuration versions so
LPs and borrowers can inspect their actual terms.

## What V1 actually implements

V1's RateModel stores both band bounds, half-life, initial rate, minimum and maximum.
Market initialization accepts per-pool overrides. Governance creates another
RateModel and assigns it to a pair with SetPairRateModel; it does not need to edit
the shared original model in place. The authorized signer is checked against the
FutarchyAuthority. Sharing a model account must not let one market silently change
another market's settings.

The inspected source defaults are **2% initial, 1% minimum, no maximum, three-day
half-life, 30–50% utilization band**. They are not evidence of current deployed
market settings. The user's 4% initial and 2% minimum are intentional new defaults.

Reuse the V1 exponential increase/hold/decrease mechanism and elapsed-rate integral.
The input is now coverage/liquidity rather than utilization, so its direction
reverses: below the coverage band increases the rate, inside holds, above decreases.
Both boundaries are included in the hold range. There is no extra instantaneous
Morpho-style utilization curve. Preserve current per-asset rates across model
replacement; only new markets take the model's initial rate.

Before replacing a model or changing the signal-producing state, accrue both debt
books through the current timestamp under the old model and previous state. New
parameters take effect prospectively. A raised floor affects subsequent time, not
the already accrued interval. Zero debt holds the current rate under the reference
policy; no interest is earned on nonexistent debt.

## Two porting defects found in the V1 source

1. V1 stores `exp_rate = 693147180 / half_life_ms` as an integer. The allowed
   fourteen- and thirty-day settings produce zero, and the out-of-band integration
   divides by that coefficient. An isolated compilation of the actual V1 functions
   reproduces the panic. A declared three-day half-life produces 6.717354% from a
   4% starting rate after three days, instead of the mathematical 8%. Compute with
   sufficient intermediate precision; do not pre-truncate the coefficient. Keep
   numerical overflow handling distinct from an economic APR cap.
2. The inspected `set_pair_rate_model` replaces the pointer without accruing the
   elapsed interval under the old model. The port must settle that interval first.
   This is a source-level integration finding, not a deployed exploit claim.

The [isolated reproduction](v1-reproduction.json) preserves outputs and source
fingerprints. Its temporary Rust source and executable are deleted after execution.
The [reference controller](controller.py) checks intended economic behavior in
floating-point mathematics; it does not validate native fixed-point or Solana costs.
Eleven targeted tests cover band boundaries, longer half-lives, no configured cap,
governance authorization, independent market settings, optional creation overrides and prospective updates.

From this directory:

```sh
python3 -m unittest test_controller.py -v
python3 reproduce_v1.py /path/to/omnipair-rs
```

Sources: [RateModel](https://github.com/omnipair/omnipair-rs/blob/84a8ef7a2a3a81e750435b86d7191bf43413d6a4/programs/omnipair/src/state/rate_model.rs),
[constants](https://github.com/omnipair/omnipair-rs/blob/84a8ef7a2a3a81e750435b86d7191bf43413d6a4/programs/omnipair/src/constants.rs),
[create model](https://github.com/omnipair/omnipair-rs/blob/84a8ef7a2a3a81e750435b86d7191bf43413d6a4/programs/omnipair/src/instructions/futarchy/create_rate_model.rs),
[assign model](https://github.com/omnipair/omnipair-rs/blob/84a8ef7a2a3a81e750435b86d7191bf43413d6a4/programs/omnipair/src/instructions/futarchy/set_pair_rate_model.rs).
The relevant files were also fetched from the GitHub default branch; see
[source-refs.json](source-refs.json) for their blob identities.
