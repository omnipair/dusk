# Dusk Compute Benchmarks

This document is the durable acceptance record for Dusk compute cost. The
authoritative swap guards come from deterministic LiteSVM scenarios in
`tests/v2-final-smoke.test.ts`. The full-instruction table records broader
transaction telemetry for every public instruction; it is diagnostic and
cannot substitute for a named-path measurement.

## Acceptance contract

The ordinary path is a legacy-SPL, no-debt, inactive-hLP, same-slot CPMM swap.
It must consume **strictly less than 100,000 CU**.

The benchmark keeps LiteSVM's default 32 KiB transaction heap. A larger heap
may be requested only by the specific scenario that proves it is necessary;
that request remains part of the measured transaction and its CU total.

Every named path has a checked-in measured maximum and a CI ceiling equal to:

\[
\operatorname{ceiling}_{\mathrm{CI}}
=
\left\lceil 1.05 \times \operatorname{maximum}_{\mathrm{measured}} \right\rceil
\]

A baseline is accepted only from one fully successful run of the finished SBF
binary. A failed or partially executed suite cannot update a ceiling. The test
harness checks both that all required scenarios ran and that every checked-in
ceiling is exactly 5% above its measured maximum.

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
| Concentration | centered; finite transition; a trade wholly inside an exact-CPMM tail |
| Dynamic fees | divergence stress; volatility stress; retained surcharge |
| Lazy controller | due parameter ramp; due funded recenter |
| hLP | active correction; stored residual correction |
| Token behavior | Token-2022 asset swap |

External transfer-hook overhead is recorded as a separate direct Token-2022
transfer transaction because hook implementation and extra-account count are
external inputs. That row is the whole transaction cost (Token-2022 plus its
hook), not an isolated hook-program measurement. The `token_2022_swap`
scenario measures Dusk's Token-2022 transfer path without pretending that
every third-party hook has the same cost.

## Finished-binary measurements

The following deterministic maxima were captured on 2026-08-07 across five
fully successful runs of the finished SBF binary:

```sh
yarn build:litesvm
yarn test-litesvm:no-build
```

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
| CPMM, same slot | 56,215 CU | 59,026 CU |
| CPMM, advanced slot | 89,304 CU | 93,770 CU |
| CPMM, active debt | 97,982 CU | 102,882 CU |
| Concentrated, centered | 188,883 CU | 198,328 CU |
| Concentrated, finite transition | 274,752 CU | 288,490 CU |
| Concentrated, exact-CPMM tail | 107,976 CU | 113,375 CU |
| Divergence-fee stress | 356,950 CU | 374,798 CU |
| Volatility-fee stress | 105,407 CU | 110,678 CU |
| Retained surcharge | 474,610 CU | 498,341 CU |
| Due parameter ramp | 489,599 CU | 514,079 CU |
| Due funded recenter | 403,274 CU | 423,438 CU |
| Active hLP | 105,701 CU | 110,987 CU |
| hLP residual correction | 164,258 CU | 172,471 CU |
| Token-2022 asset swap | 65,448 CU | 68,721 CU |

The separately measured direct Token-2022 transfer-hook transaction consumed
70,859–118,859 CU across the five runs. This is the full Token-2022 transaction cost,
not a hook-program-exclusive measurement, and is reported separately because
hook implementation, extra accounts, and address-dependent canonical PDA bump
searches are external inputs rather than deterministic Dusk swap guards.

Every swap row increased by 6,522–6,528 CU when the compact swap receipt moved
from a raw log to Anchor's reliable event self-CPI. This is the measured
end-to-end cost of making the receipt recoverable from inner instructions; it
is not curve or fee math. The ordinary path remains 43,785 CU below its 100k
acceptance limit.

## Full instruction snapshot

The five finished-binary runs recorded 4,740 successful transaction or
simulation samples: 48,089 CU weighted average and 672,876 CU observed maximum.
The SBF SHA-256 was
`eb60c252a66342d154d0e6cee36512e6f267b2976061ef666106c7615f71c531`.

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
| `swap` | 180 | 226,961 | 672,876 | 50.16% |
| `add_liquidity` | 235 | 225,715 | 630,507 | 53.30% |
| `preview_swap` | 60 | 231,184 | 439,865 | 67.42% |
| `settle_liquidation_auction_floor` | 5 | 260,031 | 260,031 | 80.74% |
| `repay` | 30 | 181,824 | 234,618 | 82.62% |
| `borrow` | 40 | 182,966 | 223,640 | 83.43% |
| `preview_borrow_capacity` | 10 | 219,425 | 220,118 | 83.69% |
| `bid_liquidation_auction` | 5 | 217,438 | 217,438 | 83.89% |
| `delegated_close_leverage` | 5 | 200,991 | 209,991 | 84.45% |
| `deposit_single_sided` | 75 | 120,374 | 199,459 | 85.23% |
| `preview_borrow_position` | 10 | 161,451 | 187,009 | 86.15% |
| `open_leverage` | 25 | 132,640 | 165,182 | 87.76% |
| `initialize_market` | 250 | 131,568 | 164,994 | 87.78% |
| `initialize_yield_accounts` | 80 | 46,131 | 162,640 | 87.95% |
| `add_leverage_margin` | 10 | 121,578 | 160,640 | 88.10% |
| `trigger_liquidation_auction` | 10 | 128,456 | 128,456 | 90.48% |
| `decrease_leverage` | 5 | 118,029 | 121,929 | 90.97% |
| `withdraw_single_sided` | 15 | 104,051 | 118,039 | 91.26% |
| `close_leverage` | 5 | 112,032 | 115,632 | 91.43% |
| `increase_leverage` | 10 | 111,144 | 115,045 | 91.48% |
| `create_parameter_proposal` | 10 | 83,164 | 97,707 | 92.76% |
| `liquidate_leverage` | 5 | 94,853 | 96,953 | 92.82% |
| `remove_liquidity` | 10 | 80,654 | 85,157 | 93.69% |
| `remove_leverage_margin` | 5 | 82,866 | 82,866 | 93.86% |
| `deposit_collateral` | 40 | 51,578 | 71,585 | 94.70% |
| `support_parameter_proposal` | 5 | 61,149 | 68,349 | 94.94% |
| `preview_market` | 5 | 66,392 | 66,392 | 95.08% |
| `claim_referral_interest` | 15 | 45,752 | 56,926 | 95.78% |
| `claim_yield` | 5 | 52,877 | 55,577 | 95.88% |
| `withdraw_parameter_support` | 5 | 51,035 | 55,535 | 95.89% |
| `withdraw_collateral` | 5 | 55,361 | 55,361 | 95.90% |
| `execute_parameter_proposal` | 5 | 53,875 | 53,875 | 96.01% |
| `settle_protocol_auction` | 10 | 50,889 | 50,891 | 96.23% |
| `initialize_lp_metadata` | 750 | 25,032 | 48,923 | 96.38% |
| `initialize_lp_transfer_hook` | 15 | 23,316 | 39,135 | 97.10% |
| `preview_add_liquidity` | 15 | 30,861 | 34,166 | 97.47% |
| `create_leverage_delegation` | 10 | 27,063 | 32,163 | 97.62% |
| `update_protocol_auction_route` | 5 | 29,223 | 29,223 | 97.84% |
| `configure_referral_partner` | 40 | 18,426 | 26,329 | 98.05% |
| `set_market_reduce_only` | 5 | 25,604 | 25,604 | 98.10% |
| `queue_parameter_proposal` | 5 | 24,129 | 24,129 | 98.21% |
| `initialize_referral_accrual` | 30 | 17,533 | 22,839 | 98.31% |
| `update_leverage_delegation` | 5 | 20,936 | 20,936 | 98.45% |
| `set_yield_recipient` | 5 | 20,073 | 20,073 | 98.51% |
| `init_futarchy_authority` | 5 | 13,933 | 13,933 | 98.97% |
| `update_protocol_auction_recipients` | 5 | 12,183 | 12,183 | 99.10% |
| `update_protocol_auction_config` | 10 | 11,977 | 11,977 | 99.11% |
| `update_protocol_revenue` | 35 | 9,718 | 11,522 | 99.15% |
| `set_referral_recipient` | 10 | 9,924 | 9,924 | 99.26% |
| `set_global_reduce_only` | 5 | 5,210 | 5,210 | 99.61% |
| `update_revenue_recipients` | 5 | 5,058 | 5,058 | 99.63% |
| `update_futarchy_authority` | 5 | 4,928 | 4,928 | 99.63% |
| `close_leverage_delegation` | 5 | 3,095 | 3,095 | 99.77% |

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
| CPMM, same slot | 111,593 CU | 56,215 CU | -55,378 CU (-49.62%) |
| CPMM, advanced slot | 164,529 CU | 89,304 CU | -75,225 CU (-45.72%) |
| Concentrated, centered versus prior mixed range | 677,000–1,323,000 CU | 188,883 CU | -72.10% to -85.72% |
| Highest named lazy-controller path versus prior mixed high-water mark | 1,323,000 CU | 489,599 CU | -833,401 CU (-62.99%) |

The ordinary same-slot path is also 28,057–31,243 CU below the observed
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
