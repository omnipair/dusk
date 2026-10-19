# Implementation sources for the whitepapers

Reviewed source: PR #45 at `5c0aca7fcb582418f1c5b99061d4ff7605b6f185`, October 19, 2026. This is a code snapshot, not a deployment claim. Paths below are relative to the repository root. Active callers were checked; historical comments and test-only helpers were not treated as current mechanism specifications.

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
| Depth without spot motion: fixed-shape ordinary scaling and distinct radial hLP ownership | `programs/dusk/src/transitions/amm/curve.rs`: `prepare_concentrated_cache_at_point` and homothetic branch selection; `programs/dusk/src/transitions/liquidity/hlp/integrated.rs`: `quote_integrated_exact_in`, `reconstruct_hlp_endpoint`; existing concentrated-liquidity scaling test in `programs/dusk/src/tests/transitions/amm.rs` |
| Deposit funding headroom, cash floors, endpoint tolerance | `programs/dusk/src/transitions/liquidity/hlp/engine.rs`: `require_hlp_borrow_headroom`, `ConcentratedHlpTransition::consume`, `interest_cash_floors`; `programs/dusk/src/instructions/prepare_swap.rs` |
| hLP funding recovery, activation, discount, critical ratio, bonus | `programs/dusk/src/transitions/liquidity/hlp/recovery.rs`: `quote_hlp_recovery`; `programs/dusk/src/transitions/liquidity/hlp/engine.rs` |
| Interest-only terminal hLP waterfall, senior insurance, caller bounds, surviving yield | `programs/dusk/src/transitions/liquidity/hlp/engine.rs`: terminal waterfall preparation/consume; `programs/dusk/src/instructions/spot/close_insolvent_hlp.rs` |
| Public admission, stored factor, shadow risk curve, daily bucket | `programs/dusk/src/transitions/lending/mod.rs`; `programs/dusk/src/math/risk.rs`; `programs/dusk/src/state/market.rs`; lending borrow/withdraw handlers |
| Index accrual before anchor update; all debt domains in utilization | `programs/dusk/src/transitions/lending/mod.rs`: `accrue_side`; `programs/dusk/src/math/risk.rs`: `utilization_bps`, `instantaneous_rate_apr_nad`, `adapt_rate_at_target_nad` |
| Defaults and bounds | `programs/dusk/src/constants.rs`; `programs/dusk/src/state/market.rs`: `IrmConfig`, `DEFAULT_IRM_*`, `MAX_DAILY_BORROW_BPS` |
| Flash liquidation, affordable discount, fees and emergency gating | `programs/dusk/src/instructions/flash_liquidation.rs`, `emergency_liquidation.rs`; `programs/dusk/src/transitions/liquidation_policy.rs` |
| EMA-priced internal execution and smoothed depth | `programs/dusk/src/transitions/amm/liquidation.rs`; `docs/calibration/EMA_SETTLEMENT_CLOSEOUT.md` |
| Isolated margin, entry limits, cash settlement and closeout health | `programs/dusk/src/transitions/leverage.rs`; `programs/dusk/src/instructions/leverage/` |
| Conditional entries, margin exits, hLP stops, escrow yield/bounty | `programs/leverage_delegate/src/lib.rs` and its modules |
| Harvest owner/recipient/authority, canonical destination | `programs/dusk/src/state/yield_account.rs`; `programs/dusk/src/instructions/liquidity/harvest.rs`, `set_yield_recipient.rs`, `set_harvest_authority.rs` |
| Reusable governance balance and revocable delegation | `programs/dusk/src/state/governance_balance.rs`; `programs/dusk/src/instructions/governance/balance.rs`, `delegated.rs` |
| Atomic LP metadata creation | `programs/dusk/src/instructions/market/initialization.rs` |
| Stored margin terms, aggregate entry charge and position actions | `programs/dusk/src/transitions/leverage_margins.rs`; `programs/dusk/src/state/leverage_position.rs`; `docs/STORED_LEVERAGE_MARGINS.md`, `docs/LEVERAGE_POSITION_ACTIONS.md` |
| Seven governance families | `programs/dusk/src/state/parameter_proposal.rs`: `ParameterFamily`, `MarketParameterUpdate` |


## Derivations and numerical evidence

[HLP_DERIVATION.md](HLP_DERIVATION.md) is the GitHub-readable walkthrough. The same derivation appears in `main.tex`, under **From Yield Basis's square-root intuition to Dusk's hedge**. The expanded appendix contains the complete native trade and reconstruction trace.

The chain is `xy=k` → `W=2√(kp)` → static versus continuously maintained 2× leverage → `dW=x dp` → inventory weight → target gross leverage `1/w`. The continuously rebalanced benchmark assumes a smooth price path, self-financing adjustments and no costs. Fixed debt does not deliver the same payoff. The concentration formula describes a local hedge; it does not promise fee-free finite-move tracking.

`simulations/generate.py` produces mathematical illustrations. `simulations/native-hlp-flow.json` separately records eight runs of the native benchmark API at the pinned #45 source. The runner is `simulations/native_hlp_flow.rs`; reproduction instructions are in `simulations/README.md`. These runs include 1×/4× concentration, both hLPs, no-fee controls and 0.3% fees with/without compounding. They do not exercise token CPIs, elapsed funding, transfer fees or external prices. The final marginal pool price is the valuation mark, not an independently observed market price.

## Incoming #52 and approved work

Incoming committed snapshot: `2bf53910aef2277b044e83bece8a13c555582039`, PR #52 targeting #45. It contains retired-hLP cleanup and SDK flash-simulation fixes. It does **not** establish that the full accounting redesign is complete.

The local full-value accounting candidate separates book ownership from executable reserves and prices fees at the actual concentrated marginal price. Native and SDK checks exist, but its full instruction suite still has compute-limit failures. Cash-funded harvesting of unpaid interest, advance settlement/writeoff accounting, funding-first treatment of newly accrued hLP yield and external hLP flash recovery remain unfinished. The paper's **Incoming Accounting Work and Merge Boundary** section records these explicitly. Refresh this status and the source hashes when that implementation lands.

The selected accounting policy keeps unpaid claims in share value, excludes them from swap pricing, and leaves no detached unpaid claim after exit. Paying an accrued claim from available cash still requires balanced advance/revenue accounting; ordinary cash availability alone is not proof of a safe payout. The selected hLP recovery target is the opposite-token hedge, which is 2× only at balanced value weights.

## External references

Primary sources include Omnipair's GAMM documentation, Curve's StableSwap/CryptoSwap papers, Morpho's adaptive rate model and Egorov/Yield Basis leveraged liquidity. The Yield Basis paper and official compounding/leverage documentation were checked on October 19, 2026. They explain the theoretical lineage; Dusk runtime statements are grounded in the pinned code. Yield Basis already uses concentrated Curve pools, so this paper does not claim that concentrated leveraged liquidity is new or that its constant-product idealization applies to every curve unchanged.

## Verification

`source-manifest.json` hashes the mapped source files. `verification.json` records current illustration checks, PDF compilation, page counts, hashes and visual review. `precommit-verification.json` records the complete required local CI for this documentation update. None of these documents authorizes deployment or asserts that pending #52 functionality is implemented.
