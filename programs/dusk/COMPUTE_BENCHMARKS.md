# Dusk Compute Benchmarks

This document is the durable acceptance record for Dusk compute cost. The
authoritative swap guards come from deterministic LiteSVM scenarios in
`tests/v2-final-smoke.test.ts`. The full-instruction table records broader
transaction telemetry for every public instruction; it is diagnostic and
cannot substitute for a named-path measurement.

## O(1) concentrated+hLP acceptance (2026-08-15)

The representative complete swap uses the former one-band concentrated curve,
gross-path toxicity and volatility fees, two active hLP vaults, algebraic
zero-opposite-exposure reconstruction, retained-principal handling, and the
normal token/account commit path. On LiteSVM's default 32 KiB heap it consumed
**97,457 CU** and completed successfully, below the **100,000 CU** product
ceiling. The measured SBF artifact SHA-256 is
`7839cac7aa799552369782e04b770df6e56877b66069f4c0178e78645e9858a2`.

This measurement predates the nested core-and-shoulder curve. It remains a
historical baseline and must not be cited as the current nested-curve cost
until the final LiteSVM acceptance pass is rerun.

Retained toxicity surcharge is now credited to a custody-backed,
non-quoteable reserve bucket. Ordinary swaps only credit that bucket; they do
not rebuild the curve. A separately measured funded-recenter transaction
consumed **191,305 CU** while atomically deploying the previously protected
reserve, reconstructing the curve at the new center, and executing the
triggering swap. The complete suite's largest swap, an active-hLP funded
recenter, consumed **205,296 CU**. The triggering swap may seed a new protected
bucket for a later recenter.

Reproduction command:

```sh
npm run test-litesvm:no-build -- --grep "measures O\\(1\\) concentrated hLP swap"
```

The production swap path contains no finite-difference, Jacobian, Broyden, or
iterative invariant solve. Legacy solver entry points are test-only
differential references with no production caller or state authority.

The final no-build validation passed **51/51 LiteSVM tests**, exercised
**53/53 public instructions**, and recorded 1,027 successful transactions or
simulations. Its largest observed transaction was 269,775 CU. Named swap-path
maxima were 55,419 CU for same-slot CPMM, 64,401 CU for centered
concentration, 97,457 CU for active concentrated hLP, and 191,305 CU for a
funded recenter.

## Acceptance contract

The ordinary path is a legacy-SPL, no-debt, inactive-hLP, same-slot CPMM swap.
It must consume **strictly less than 100,000 CU**.

hLP entry is live. The concentrated path reconstructs hLP ownership and indexed
debt algebraically so each active vault ends with zero opposite-asset exposure
at canonical atom precision. Passive funding-debt insolvency and its terminal
recovery waterfall remain a separate economic design risk.

The benchmark keeps LiteSVM's default 32 KiB transaction heap. A larger heap
may be requested only by the specific scenario that proves it is necessary;
that request remains part of the measured transaction and its CU total.

Every named path has a checked-in measured maximum and a CI ceiling equal to:

\[
\operatorname{ceiling}_{\mathrm{CI}}
=
\left\lceil 1.05 \times \operatorname{maximum}_{\mathrm{measured}} \right\rceil
\]

A baseline is accepted only from fully successful runs of one unchanged,
finished SBF binary. A failed or partially executed suite cannot update a
ceiling. The test harness checks both that all required scenarios ran and that
every checked-in ceiling is exactly 5% above its measured maximum.

Generate candidate baselines with:

```sh
yarn build:litesvm
yarn test-litesvm:no-build
```

Only if the complete suite passes, copy the emitted candidate values into
`COMPUTE_SCENARIO_BASELINES`. Then run the release guard:

```sh
yarn test-litesvm:release
```

The release command requires every named baseline and the separate external
Token-2022 transfer-hook transaction measurement; the ordinary development
command remains usable while a finished-binary baseline is being generated.

The required named scenarios are:

| Group | Deterministic scenario |
|---|---|
| CPMM | same slot; advanced slot; active debt after elapsed-slot accrual |
| Concentration | centered; finite transition; a trade wholly inside a shifted-CPMM tail |
| Dynamic fees | divergence stress; volatility stress; retained surcharge |
| Lazy controller | due funded recenter |
| hLP | active integrated hedge; funding-interest settlement |
| Token behavior | Token-2022 asset swap |

External transfer-hook overhead is recorded as a separate direct Token-2022
transfer transaction because hook implementation and extra-account count are
external inputs. That row is the whole transaction cost (Token-2022 plus its
hook), not an isolated hook-program measurement. The `token_2022_swap`
scenario measures Dusk's Token-2022 transfer path without pretending that
every third-party hook has the same cost.

## Historical pre-nested-curve finished-binary measurements

The following deterministic maxima were captured on 2026-08-10 across five
fully successful no-build runs of one finished SBF binary. The artifact was
built once before the five measurement runs:

```sh
yarn build:litesvm
yarn test-litesvm:no-build
```

This is the default development/audit binary. hLP entry is enabled in every
build profile; its settlement paths are therefore part of the measured
instruction surface.

Each run passed 49/49 LiteSVM tests, exercised and measured 53/53 Dusk
instructions, and measured every required scenario. Each CI ceiling below is exactly
`ceil(measured maximum * 1.05)`. It used Node 24.9.0 and LiteSVM 0.8.0 with
LiteSVM's 219 default feature accounts plus only Solana's stricter ABI/runtime
constraints feature; the harness asserts that feature set before executing.
The measurement and CI execution environments are ARM64, where LiteSVM uses
the deterministic SBF interpreter. The SBF artifacts are still built and
validated on Ubuntu x86-64 before CI transfers them to the isolated macOS ARM64
execution job. This avoids the upstream LiteSVM x86-64 Node/JIT memory-corruption
bug without changing program math or sanitizing malformed decoded values.

| Scenario | Measured maximum | CI ceiling |
|---|---:|---:|
| CPMM, same slot | 57,267 CU | 60,131 CU |
| CPMM, advanced slot | 90,356 CU | 94,874 CU |
| CPMM, active debt | 99,034 CU | 103,986 CU |
| Concentrated, centered | 189,935 CU | 199,432 CU |
| Concentrated, finite transition | 275,804 CU | 289,595 CU |
| Concentrated, exact-CPMM tail | 109,028 CU | 114,480 CU |
| Divergence-fee stress | 358,002 CU | 375,903 CU |
| Volatility-fee stress | 106,459 CU | 111,782 CU |
| Retained surcharge | 475,662 CU | 499,446 CU |
| Due parameter ramp | 490,651 CU | 515,184 CU |
| Due funded recenter | 404,326 CU | 424,543 CU |
| Active hLP | 106,768 CU | 112,107 CU |
| hLP residual correction | 168,751 CU | 177,189 CU |
| Token-2022 asset swap | 66,500 CU | 69,825 CU |

The separately measured direct Token-2022 transfer-hook transaction consumed
77,031–93,531 CU across the five runs. This is the full Token-2022 transaction cost,
not a hook-program-exclusive measurement, and is reported separately because
hook implementation, extra accounts, and address-dependent canonical PDA bump
searches are external inputs rather than deterministic Dusk swap guards.

Every swap row increased by 6,522–6,528 CU when the compact swap receipt moved
from a raw log to Anchor's reliable event self-CPI. This is the measured
end-to-end cost of making the receipt recoverable from inner instructions; it
is not curve or fee math. The ordinary path remains 42,733 CU below its 100k
acceptance limit.

## Full instruction snapshot

The five finished-binary runs recorded 4,730 successful transaction or
simulation samples: 46,946 CU weighted average and 634,554 CU observed maximum.
The SBF SHA-256 was
`6c191ccea46caa2e0da54e71974af16505aa328d65fec90a68c5d2ad48be4ac7`.
The subsequent `yarn test-litesvm:release` gate passed 49/49 tests, measured
53/53 instructions, enforced every checked-in ceiling, and rebuilt the same
SBF SHA-256.

Each row attributes the complete transaction cost to every top-level Dusk
instruction in that transaction. It therefore includes token/system CPIs,
transfer-hook work, and event self-CPIs and is a conservative integration cost,
not an instruction-exclusive profiler result. Preview rows are successful
LiteSVM simulations. “Observed maximum” is the largest exercised fixture, not
a proof of a global maximum; the named swap scenarios above are the stricter
worst-path regression guards. Headroom is against Dusk's 1,350,000-CU test
limit, which itself stays 50,000 CU below Solana's 1,400,000-CU transaction
ceiling.

| Instruction | Samples | Average CU | Observed maximum CU | Headroom |
|---|---:|---:|---:|---:|
| `add_liquidity` | 235 | 227,185 | 634,554 | 53.00% |
| `swap` | 170 | 207,403 | 490,651 | 63.66% |
| `preview_swap` | 65 | 220,523 | 441,642 | 67.29% |
| `backstop_liquidation_auction` | 5 | 260,654 | 260,654 | 80.70% |
| `repay` | 30 | 184,347 | 253,241 | 81.25% |
| `borrow` | 40 | 184,414 | 227,263 | 83.17% |
| `preview_borrow_capacity` | 10 | 219,631 | 220,324 | 83.68% |
| `fill_liquidation_auction` | 5 | 218,061 | 218,061 | 83.85% |
| `delegated_close_leverage` | 5 | 201,823 | 203,923 | 84.90% |
| `preview_borrow_position` | 10 | 161,657 | 187,215 | 86.14% |
| `open_leverage` | 25 | 132,970 | 169,232 | 87.47% |
| `deposit_single_sided` | 70 | 116,406 | 167,659 | 87.59% |
| `initialize_yield_accounts` | 80 | 46,469 | 167,659 | 87.59% |
| `add_leverage_margin` | 10 | 121,301 | 164,263 | 87.84% |
| `initialize_market` | 250 | 130,568 | 163,700 | 87.88% |
| `start_liquidation_auction` | 10 | 128,662 | 128,662 | 90.47% |
| `decrease_leverage` | 5 | 120,579 | 124,479 | 90.78% |
| `increase_leverage` | 10 | 113,244 | 117,595 | 91.29% |
| `close_leverage` | 5 | 113,082 | 115,182 | 91.47% |
| `withdraw_single_sided` | 15 | 104,516 | 111,623 | 91.74% |
| `liquidate_leverage_position` | 5 | 97,333 | 101,233 | 92.51% |
| `create_parameter_proposal` | 10 | 85,620 | 97,913 | 92.75% |
| `remove_liquidity` | 10 | 82,896 | 88,893 | 93.42% |
| `remove_leverage_margin` | 5 | 83,487 | 83,487 | 93.82% |
| `support_parameter_proposal` | 5 | 64,055 | 68,555 | 94.93% |
| `claim_referral_interest` | 15 | 47,558 | 66,132 | 95.11% |
| `preview_market` | 5 | 61,143 | 61,143 | 95.48% |
| `initialize_lp_metadata` | 750 | 24,862 | 57,872 | 95.72% |
| `deposit_collateral` | 40 | 51,297 | 57,860 | 95.72% |
| `withdraw_collateral` | 5 | 55,567 | 55,567 | 95.89% |
| `withdraw_parameter_support` | 5 | 52,141 | 54,241 | 95.99% |
| `execute_parameter_proposal` | 5 | 54,093 | 54,093 | 96.00% |
| `harvest` | 5 | 52,000 | 53,200 | 96.06% |
| `settle_protocol_auction` | 10 | 51,125 | 51,127 | 96.22% |
| `preview_add_liquidity` | 15 | 31,067 | 34,372 | 97.46% |
| `initialize_lp_transfer_hook` | 15 | 22,546 | 30,165 | 97.77% |
| `update_protocol_auction_route` | 5 | 29,429 | 29,429 | 97.83% |
| `create_leverage_delegation` | 10 | 25,593 | 29,193 | 97.84% |
| `initialize_referral_accrual` | 30 | 17,613 | 27,468 | 97.97% |
| `configure_referral_partner` | 40 | 17,864 | 26,329 | 98.05% |
| `set_market_reduce_only` | 5 | 25,810 | 25,810 | 98.09% |
| `queue_parameter_proposal` | 5 | 24,159 | 24,159 | 98.22% |
| `update_leverage_delegation` | 5 | 20,966 | 20,966 | 98.45% |
| `set_yield_recipient` | 5 | 20,103 | 20,103 | 98.52% |
| `init_futarchy_authority` | 5 | 13,933 | 13,933 | 98.97% |
| `update_protocol_auction_recipients` | 5 | 12,183 | 12,183 | 99.10% |
| `update_protocol_auction_config` | 10 | 11,977 | 11,977 | 99.12% |
| `update_protocol_revenue` | 35 | 9,718 | 11,522 | 99.15% |
| `set_referral_recipient` | 10 | 9,924 | 9,924 | 99.27% |
| `set_global_reduce_only` | 5 | 5,210 | 5,210 | 99.62% |
| `update_revenue_recipients` | 5 | 5,058 | 5,058 | 99.63% |
| `update_futarchy_authority` | 5 | 4,928 | 4,928 | 99.64% |
| `close_leverage_delegation` | 5 | 3,095 | 3,095 | 99.78% |

The release-mode test now rejects a run missing a successful CU sample for any
of the 53 public Dusk instructions, in addition to enforcing every named swap
scenario baseline.

## Rewrite deltas

Only the same-slot and advanced-slot CPMM observations use directly
comparable named fixtures. The historical concentrated observations were a
mixed-path range, so their deltas are context rather than a controlled
before/after claim.

| Path | Before | Finished rewrite | Delta |
|---|---:|---:|---:|
| CPMM, same slot | 111,593 CU | 57,267 CU | -54,326 CU (-48.68%) |
| CPMM, advanced slot | 164,529 CU | 90,356 CU | -74,173 CU (-45.08%) |
| Concentrated, centered versus prior mixed range | 677,000–1,323,000 CU | 189,935 CU | -71.94% to -85.64% |
| Highest named lazy-controller path versus prior mixed high-water mark | 1,323,000 CU | 490,651 CU | -832,349 CU (-62.91%) |

The ordinary same-slot path is also 27,005–30,191 CU below the observed
Omnipair V1 range of 84,272–87,458 CU. Controller and hLP rows
already include the lazy work performed by the user operation; there is no
separate maintenance transaction to add.

## Historical comparison context

These figures are prior local observations retained for architectural context.
They were not all captured with the same pool state, transaction composition,
runtime version, or account extensions, so they are **not** regression guards
and should not be read as an apples-to-apples ranking.

| Implementation / path | Observed CU | Measurement status |
|---|---:|---|
| Omnipair V1 swap | 84,272–87,458; 86,104 median | Mainnet program-invocation sample, 11 successful Jupiter-routed swaps |
| Pre-rewrite Dusk V2 CPMM, same slot | 111,593 | Prior deterministic LiteSVM observation |
| Pre-rewrite Dusk V2 CPMM, advanced slot | 164,529 | Prior deterministic LiteSVM observation |
| Pre-rewrite Dusk concentrated paths | 677,000–1,323,000 | Prior mixed-path LiteSVM range |
| Raydium CPMM | approximately 23,000 | Mainnet program-invocation sample |
| Raydium AMM v4 | approximately 25,700 | Mainnet program-invocation sample |
| Raydium CLMM | approximately 41,900 median | Mainnet program-invocation sample |
| Meteora DAMM v2 | approximately 14,300 median | Mainnet program-invocation sample |
| Meteora DAMM v1 | approximately 78,100 median | Mainnet program-invocation sample |
| Meteora DLMM | approximately 37,700 median | Mainnet program-invocation sample |
| Orca Whirlpool | approximately 32,900 median | Mainnet program-invocation sample |

The competitor observations are live transaction samples, not controlled
benchmarks: router composition, token program, tick/bin crossings, account
extensions, and runtime version can materially change cost. They establish an
order-of-magnitude reference only.
