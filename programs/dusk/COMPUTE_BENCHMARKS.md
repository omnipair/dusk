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
yarn test-litesvm:release --reporter dot
```

That run passed 50/50 LiteSVM tests, exercised 52/52 Dusk instructions, and
measured every required scenario. Each CI ceiling below is exactly
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
| CPMM, same slot | 51,758 CU | 54,346 CU |
| CPMM, advanced slot | 84,850 CU | 89,093 CU |
| CPMM, active debt | 93,504 CU | 98,180 CU |
| Concentrated, centered | 184,426 CU | 193,648 CU |
| Concentrated, finite transition | 270,069 CU | 283,573 CU |
| Concentrated, exact-CPMM tail | 103,293 CU | 108,458 CU |
| Divergence-fee stress | 356,055 CU | 373,858 CU |
| Volatility-fee stress | 100,473 CU | 105,497 CU |
| Retained surcharge | 370,975 CU | 389,524 CU |
| Due parameter ramp | 485,147 CU | 509,405 CU |
| Due funded recenter | 480,413 CU | 504,434 CU |
| Active hLP | 101,231 CU | 106,293 CU |
| hLP residual correction | 159,786 CU | 167,776 CU |
| Token-2022 asset swap | 60,838 CU | 63,880 CU |

The separately measured direct Token-2022 transfer-hook transaction consumed
77,049–125,049 CU across the final runtime-validation runs; the strict
fresh-build release run measured 95,049 CU. This is the full Token-2022 transaction cost,
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
| CPMM, same slot | 111,593 CU | 51,758 CU | -59,835 CU (-53.62%) |
| CPMM, advanced slot | 164,529 CU | 84,850 CU | -79,679 CU (-48.43%) |
| Concentrated, centered versus prior mixed range | 677,000–1,323,000 CU | 184,426 CU | -72.76% to -86.06% |
| Highest named lazy-controller path versus prior mixed high-water mark | 1,323,000 CU | 485,147 CU | -837,853 CU (-63.33%) |

The ordinary same-slot path is also 32,514–35,700 CU below the observed
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
