# Implementation sources for the whitepapers

Reviewed source: `263c1635ad87ee36ffd9bf0b970457e4be965326`, September 10, 2026. This is a code snapshot, not a deployment claim. Paths below are relative to the repository root. Active callers were checked; historical comments and test-only helpers were not treated as current mechanism specifications.

| Paper claim | Implementation source and semantic anchor |
| --- | --- |
| Stored live = cash + indexed public and isolated debt + hLP live | `programs/dusk/src/transitions/ledger.rs`: `assert_virtual_reserve_invariant`; `programs/dusk/src/state/market.rs`: `Debt` |
| Unpaid public and isolated interest excluded from executable curve | `programs/dusk/src/transitions/amm/mod.rs`: `unrealized_interest`, `curve_reserve`, `integrated_curve_state_nad` |
| Reserve custody claims, donations excluded | `programs/dusk/src/instructions/accounts.rs`: `require_reserve_custody` |
| Nested tail/core/shoulder geometry, exact CPMM configuration | `programs/dusk/src/transitions/amm/curve.rs`: `ConcentratedCurveParameters`, `ConcentratedCurveCache::geometry`, `prepare_concentrated_cache_at_point` |
| Deferred controller and protected recenter funding | `programs/dusk/src/transitions/amm/mod.rs`: `credit_protected_recenter_reserve`, deferred controller and retention logic; `programs/dusk/src/instructions/prepare_swap.rs` |
| Active gross-path divergence, bounded fee basis, input/output collection | `programs/dusk/src/transitions/amm/mod.rs`; `programs/dusk/src/transitions/amm/fees.rs`: `gross_path_divergence_fee_raw`, `hard_total_fee_budget_floor` |
| Fee compounding and frozen yLP eligibility | `programs/dusk/src/transitions/liquidity/hlp/integrated.rs`: `apply_compounded_ylp_fee`; `programs/dusk/src/transitions/ledger.rs`; `programs/dusk/src/state/market.rs`: `FeeProfile` |
| Algebraic ordinary-tranche quote and endpoint ownership | `programs/dusk/src/transitions/liquidity/hlp/integrated.rs`: `IntegratedCurveState`, `reconstruct_hlp_endpoint`, `reconstruct_hlp_ownership` |
| Deposit funding headroom, cash floors, endpoint tolerance | `programs/dusk/src/transitions/liquidity/hlp/engine.rs`: `require_hlp_borrow_headroom`, `ConcentratedHlpTransition::consume`, `interest_cash_floors`; `programs/dusk/src/instructions/prepare_swap.rs` |
| hLP funding recovery, activation, discount, critical ratio, bonus | `programs/dusk/src/transitions/liquidity/hlp/recovery.rs`: `quote_hlp_recovery`; `programs/dusk/src/transitions/liquidity/hlp/engine.rs` |
| Interest-only terminal hLP waterfall, senior insurance, caller bounds, surviving yield | `programs/dusk/src/transitions/liquidity/hlp/engine.rs`: terminal waterfall preparation/consume; `programs/dusk/src/instructions/spot/close_insolvent_hlp.rs` |
| Public admission, stored factor, shadow risk curve, daily bucket | `programs/dusk/src/transitions/lending/mod.rs`; `programs/dusk/src/math/risk.rs`; `programs/dusk/src/state/market.rs`; lending borrow/withdraw handlers |
| Index accrual before anchor update; all debt domains in utilization | `programs/dusk/src/transitions/lending/mod.rs`: `accrue_side`; `programs/dusk/src/math/risk.rs`: `utilization_bps`, `instantaneous_rate_apr_nad`, `adapt_rate_at_target_nad` |
| Defaults and bounds | `programs/dusk/src/constants.rs`; `programs/dusk/src/state/market.rs`: `IrmConfig`, `DEFAULT_IRM_*`, `MAX_DAILY_BORROW_BPS` |
| Public liquidation trigger and auction floor/fill/expired backstop | `programs/dusk/src/transitions/lending/mod.rs`; `programs/dusk/src/transitions/lending/liquidation.rs`; `programs/dusk/src/instructions/lending/liquidation/start_liquidation_auction.rs`, `programs/dusk/src/instructions/lending/liquidation/fill_liquidation_auction.rs`, `programs/dusk/src/instructions/lending/liquidation/backstop_liquidation_auction.rs`, `programs/dusk/src/instructions/lending/liquidation/settlement.rs` |
| Isolated leverage, entry limits, cash settlement and closeout health | `programs/dusk/src/transitions/leverage.rs`; `programs/dusk/src/instructions/leverage/` |
| Conditional entries, leverage exits, hLP stops, escrow yield/bounty | `programs/leverage_delegate/src/lib.rs` and its modules |
| Harvest owner/recipient/authority, canonical destination | `programs/dusk/src/state/yield_account.rs`; `programs/dusk/src/instructions/liquidity/harvest.rs`, `set_yield_recipient.rs`, `set_harvest_authority.rs` |
| Seven governance families | `programs/dusk/src/state/parameter_proposal.rs`: `ParameterFamily`, `MarketParameterUpdate` |

## Corrections from the recovered July drafts

1. Replaced universal CPMM pricing with the implemented nested concentrated path and distinguished marginal price from inventory ratio.
2. Separated raw live reserves, unpaid-interest-adjusted reserves, ordinary trader inventory, and physical custody.
3. Included isolated debt in cash-backed debt without counting it twice in utilization.
4. Replaced synthetic-depth and iterative pre-rebalance claims with ordinary-tranche execution and algebraic hLP reconstruction; restricted the two-times statement to the balanced CPMM case.
5. Scoped hLP funding headroom to deposit admission and documented cash rejection, recovery, and terminal interest-only insolvency.
6. Added fee denomination, optional LP compounding, retained recenter funding, launch controls, and the current harvest authority.
7. Corrected the utilization target to 70%, anchor-versus-APR bounds, update order, and cadence-dependent numerical examples.
8. Distinguished admission, liquidation eligibility, auction floor, and realized backstop recovery; removed generic close-factor and bounded-LP-loss claims.
9. Replaced obsolete simulations, repeated prototype appendices, unsupported volume statistics, stale API descriptions, and deployment recommendations with current derivations and explicit evidence limits.

## External references

The bibliography retains primary sources for GAMM lineage, Curve's StableSwap and dynamic-peg work, Morpho's adaptive rate-at-target mechanism, and Egorov/Yield Basis leveraged liquidity. Online references were checked on September 10, 2026. The supplied June 12, 2025 Egorov manuscript was read as theoretical context. None is used as authority for current Dusk runtime behavior or as a performance benchmark.

## Verification scope

- `cargo test -p dusk --lib`: 362 passed, zero failures on the reviewed source.
- `simulations/generate.py`: disclosed illustration checks pass; these are not full Python/Rust differential tests.
- LaTeX compilation, bibliography resolution, page rendering, and final visual checks are recorded in `verification.json` after PDF QA.
- No code or interface changes; no commit or push. The complete pre-commit CI sequence was not required or represented as run.
