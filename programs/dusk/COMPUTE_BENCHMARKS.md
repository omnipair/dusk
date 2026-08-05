# Dusk Swap Compute Benchmarks

This document is the durable acceptance record for Dusk swap compute cost. The
authoritative measurements come from the deterministic LiteSVM scenarios in
`tests/v2-final-smoke.test.ts`; aggregate instruction telemetry is diagnostic
only and cannot substitute for a named-path measurement.

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

The following deterministic maxima were captured on 2026-08-05 from one
fully successful run of the finished SBF binary:

```sh
yarn test-litesvm:no-build --reporter dot
```

That run passed 50/50 LiteSVM tests, exercised 52/52 Dusk instructions, and
measured every required scenario. Each CI ceiling below is exactly
`ceil(measured maximum * 1.05)`.

| Scenario | Measured maximum | CI ceiling |
|---|---:|---:|
| CPMM, same slot | 63,924 CU | 67,121 CU |
| CPMM, advanced slot | 97,012 CU | 101,863 CU |
| CPMM, active debt | 106,098 CU | 111,403 CU |
| Concentrated, centered | 196,592 CU | 206,422 CU |
| Concentrated, finite transition | 282,235 CU | 296,347 CU |
| Concentrated, exact-CPMM tail | 115,459 CU | 121,232 CU |
| Divergence-fee stress | 368,109 CU | 386,515 CU |
| Volatility-fee stress | 112,635 CU | 118,267 CU |
| Retained surcharge | 383,233 CU | 402,395 CU |
| Due parameter ramp | 497,311 CU | 522,177 CU |
| Due funded recenter | 492,667 CU | 517,301 CU |
| Active hLP | 113,817 CU | 119,508 CU |
| hLP residual correction | 172,825 CU | 181,467 CU |
| Token-2022 asset swap | 60,606 CU | 63,637 CU |

The separately measured direct Token-2022 transfer-hook transaction consumed
79,358–97,358 CU across the final full-suite runs; the strict fresh-build
release run measured 88,358 CU. This is the full Token-2022 transaction cost,
not a hook-program-exclusive measurement, and is reported separately because
hook implementation, extra accounts, and address-dependent canonical PDA bump
searches are external inputs rather than deterministic Dusk swap guards.

## Rewrite deltas

Only the same-slot and advanced-slot CPMM observations use directly
comparable named fixtures. The historical concentrated observations were a
mixed-path range, so their deltas are context rather than a controlled
before/after claim.

| Path | Before | Finished rewrite | Delta |
|---|---:|---:|---:|
| CPMM, same slot | 111,593 CU | 63,924 CU | -47,669 CU (-42.72%) |
| CPMM, advanced slot | 164,529 CU | 97,012 CU | -67,517 CU (-41.04%) |
| Concentrated, centered versus prior mixed range | 677,000–1,323,000 CU | 196,592 CU | -70.96% to -85.14% |
| Highest named lazy-controller path versus prior mixed high-water mark | 1,323,000 CU | 497,311 CU | -825,689 CU (-62.41%) |

The ordinary same-slot path is also 20,348–23,534 CU below the observed
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
