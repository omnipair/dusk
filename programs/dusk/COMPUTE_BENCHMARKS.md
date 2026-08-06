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

The following deterministic maxima were captured on 2026-08-06 from one
fully successful run of the finished SBF binary:

```sh
yarn build:litesvm
yarn test-litesvm:no-build
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
| CPMM, same slot | 58,286 CU | 61,201 CU |
| CPMM, advanced slot | 91,378 CU | 95,947 CU |
| CPMM, active debt | 100,032 CU | 105,034 CU |
| Concentrated, centered | 190,954 CU | 200,502 CU |
| Concentrated, finite transition | 276,597 CU | 290,427 CU |
| Concentrated, exact-CPMM tail | 109,821 CU | 115,313 CU |
| Divergence-fee stress | 362,583 CU | 380,713 CU |
| Volatility-fee stress | 107,001 CU | 112,352 CU |
| Retained surcharge | 474,615 CU | 498,346 CU |
| Due parameter ramp | 491,675 CU | 516,259 CU |
| Due funded recenter | 486,935 CU | 511,282 CU |
| Active hLP | 107,758 CU | 113,146 CU |
| hLP residual correction | 166,313 CU | 174,629 CU |
| Token-2022 asset swap | 67,366 CU | 70,735 CU |

The separately measured direct Token-2022 transfer-hook transaction consumed
71,329–86,329 CU across the final production- and development-feature
verification runs. This is the full Token-2022 transaction cost,
not a hook-program-exclusive measurement, and is reported separately because
hook implementation, extra accounts, and address-dependent canonical PDA bump
searches are external inputs rather than deterministic Dusk swap guards.

Every swap row increased by 6,522–6,528 CU when the compact swap receipt moved
from a raw log to Anchor's reliable event self-CPI. This is the measured
end-to-end cost of making the receipt recoverable from inner instructions; it
is not curve or fee math. The ordinary path remains 41,714 CU below its 100k
acceptance limit.

## Rewrite deltas

Only the same-slot and advanced-slot CPMM observations use directly
comparable named fixtures. The historical concentrated observations were a
mixed-path range, so their deltas are context rather than a controlled
before/after claim.

| Path | Before | Finished rewrite | Delta |
|---|---:|---:|---:|
| CPMM, same slot | 111,593 CU | 58,286 CU | -53,307 CU (-47.77%) |
| CPMM, advanced slot | 164,529 CU | 91,378 CU | -73,151 CU (-44.46%) |
| Concentrated, centered versus prior mixed range | 677,000–1,323,000 CU | 190,954 CU | -71.79% to -85.57% |
| Highest named lazy-controller path versus prior mixed high-water mark | 1,323,000 CU | 491,675 CU | -831,325 CU (-62.84%) |

The ordinary same-slot path is also 25,986–29,172 CU below the observed
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
