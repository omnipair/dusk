# Omnipair Dusk (v2)

Omnipair Dusk (v2) is the standalone Dusk market program. It uses market terminology, floating yield LP shares, aggregate hedged LP vault accounting, isolated spot-margin leverage, and an optional oracle-less Dusk Concentrated AMM. See [`CONCENTRATION.md`](./CONCENTRATION.md) for the curve, recentering, fee, and protected-liquidity specification.

## Source Boundaries

- `instructions/`: Anchor account validation, inventory movement, slippage checks, and events.
- `transitions/`: atomic accounting mutations with small receipts for events and tests.
- `state/`: account layouts, embedded market books, and invariants.
- `tokens/`: validation for Token-2022 yLP and hLP mints.
- `math/`: fixed-point concentrated-AMM/CPMM, dynamic-fee, EMA, valuation, and interest helpers.
- `utils/`: shared accounting helpers used by transitions.

Instruction modules are split by domain: `market`, `liquidity`, `yielding`, `spot`, `lending`, `leverage`, `referral`, and `futarchy`.

## Public Instructions

Omnipair Dusk (v2) exposes the current market instruction set:

- `initialize`, `initialize_lp_metadata`, `update_config`, `set_reduce_only`
- `add_liquidity`, `remove_liquidity`
- `set_yield_recipient`, `claim_yield`
- `swap`
- `deposit_collateral`, `withdraw_collateral`, `borrow`, `repay`
- `configure_referral_partner`, `initialize_referral_accrual`, `set_referral_recipient`, `claim_referral_interest`
- `trigger_liquidation_auction`, `bid_liquidation_auction`, `settle_liquidation_auction_amm`
- `deposit_single_sided`, `withdraw_single_sided`, `crank_hlp_rebalance`, `crank_amm_maintenance`
- `open_leverage`, `close_leverage`, `delegated_close_leverage`, `increase_leverage`, `decrease_leverage`, `add_leverage_margin`, `remove_leverage_margin`, `liquidate_leverage`
- `create_leverage_delegation`, `update_leverage_delegation`, `close_leverage_delegation`
- `preview_market`, `preview_add_liquidity`, `preview_swap`, `preview_borrow_capacity`, `preview_borrow_position`
- Futarchy, operator, and revenue administration: `init_futarchy_authority`, `update_futarchy_authority`, `update_protocol_revenue`, `update_revenue_recipients`, `update_protocol_auction_config`, `update_protocol_auction_recipients`, `set_global_reduce_only`, `settle_protocol_auction`, `set_operator`, `set_manager`, `claim_manager_fees`

## Token Model

Each market records three Token-2022 LP mints:

- `yLP`: the normal two-sided LP share for balanced base/quote liquidity.
- `hLP_base`: one-sided hedged LP shares targeting base exposure.
- `hLP_quote`: one-sided hedged LP shares targeting quote exposure.

yLP and hLP mints must be fee-free Token-2022 mints with a transfer hook configured to the Dusk program, mint authority set to the market PDA, and no freeze authority. `initialize_lp_metadata` creates Metaplex metadata for each LP mint with the market PDA as update authority. Production builds additionally enforce vanity suffixes: `yLP` for yLP and `hLP` for each hLP mint. Underlying asset mints may be SPL Token or Token-2022 mints accepted by the shared mint validator.

## yLP Liquidity

`add_liquidity` is the normal LP entry. Users deposit both market assets at the current market ratio and receive one fungible `yLP` token.

yLP shares are floating two-sided principal shares:

```text
base_claim  = user_ylp_shares * base_live_reserve  / total_ylp_supply
quote_claim = user_ylp_shares * quote_live_reserve / total_ylp_supply
```

There is no fixed 1:1 protected-principal LP, no separate public fee-eligibility step, and no retained junior-capital account. `remove_liquidity` burns yLP and returns pro-rata base and quote principal reserves subject to cash availability and user slippage bounds.

Base swap fees, distributed dynamic surcharge, and borrow interest are non-compounding liabilities. They are held outside principal reserves in side-specific fee and interest vaults and distributed through side-specific growth indexes. While the AMM's protected recentering budget is under target, only the dynamic surcharge may remain in executable reserves as auto-compounding yLP principal. `YieldAccount` stores owner checkpoints, accrued revenue, and an optional external revenue recipient for treasury or protocol-owned liquidity flows.

## hLP Vaults

Each market has two aggregate hLP vault records embedded in the `Market` account:

- `hLP_base`: user deposits base, the vault borrows quote, and the vault owns yLP.
- `hLP_quote`: user deposits quote, the vault borrows base, and the vault owns yLP.

Opening hLP:

```text
user target asset
  -> hLP vault borrows opposite asset
  -> vault adds balanced liquidity
  -> vault receives yLP
  -> user receives hLP_target
```

Closing hLP burns hLP shares, burns the vault's proportional yLP, repays the borrowed-side vault debt, and returns remaining target-side inventory to the user. hLP debt is denominated in the borrowed underlying asset and tracked on the aggregate hLP vault, not as borrower margin debt.

## Isolated Leverage

Dusk includes isolated spot-margin leverage inside the market account model. A leverage position is a user-owned PDA that records margin, collateral, borrowed principal, debt shares, and the debt side for a single market-local position.

Opening leverage:

```text
user margin + isolated borrow
  -> internal GAMM swap
  -> collateral held in a leverage collateral vault
  -> debt tracked in isolated debt buckets
```

Users can increase or decrease exposure, add or remove margin, and close the position. Liquidation is permissionless once closeout value falls below the maintenance threshold. Isolated leverage debt contributes to utilization and interest accrual, but it is kept separate from normal borrower debt and aggregate hLP vault debt.

Owners can approve a position-scoped `LeverageDelegation` PDA for a delegate program. Delegated close uses a before-hook approval payload and an after-hook settlement payload, allowing keeper-style take-profit or stop-loss execution while binding the close to the expected market, owner, position, delegation, output mint, recipient, and residual amount.

## Referral Interest Sharing

Referrals are permissioned. `configure_referral_partner` lets the Futarchy authority create or update a protocol-wide `ReferralPartner` for any referrer, including its interest-share rate and active status. The referrer may use `set_referral_recipient` to rotate only the wallet that receives claims.

An active partner may be bound when a borrow debt side is first opened or when `open_leverage` creates a position. Dusk snapshots `min(partner.interest_share_bps, max_referral_interest_share_bps)` into the position at binding. The partner and share are immutable while that debt exists: later borrowing and leverage increases retain them, full repayment or closure clears them, and existing unbound debt cannot be retroactively referred. Deactivation blocks new bindings only and does not change existing referral terms, debt, liquidation terms, or claimable revenue.

Referral adds no fee or debt to the user. On each interest realization, Dusk takes a share only from the DAO's configured interest revenue:

```text
protocol_interest_revenue = floor(actual_interest_vault_credit * protocol_interest_bps / 10_000)
bound_referral_share      = min(partner.interest_share_bps, max_referral_interest_share_bps) at binding
referral_accrual          = floor(protocol_interest_revenue * bound_referral_share / 10_000)
```

The runtime cap is governed through `update_protocol_revenue` and applies when a new binding is admitted; later cap or partner changes do not reprice existing debt. `ReferralAccrual` records the claimable liability for one partner, market, and debt mint while the backing tokens remain in the market interest vault. `claim_referral_interest` pays the partner's current recipient. Realization and claims support legacy SPL Token and Token-2022 assets, including transfer fees and transfer hooks.

## Swaps And Rebalancing

`swap` is the Dusk swap entry. It transfers inventory, applies the market's exact CPMM or Dusk Concentrated AMM reserve curve, charges the configured base plus divergence/volatility fees, routes only claimable fees to the fee vault, and checkpoints both aggregate hLP vaults in O(1).

The divergence surcharge has no configured rate ceiling: its marginal toll is
unbounded. The separately configured volatility mapping is asymptotic and
stays below 100% for every finite state. The implicit gross-input solve lets
the effective surcharge share approach 100% while preserving positive
curve-executable input at token precision; a quote rejects when token
granularity cannot preserve that input. Launch markets support asset decimals
from zero through nine, and initialization rejects finer assets.
Routers and user slippage bounds decide whether the resulting market quote is
acceptable.

`peak_depth = 0, imbalance_scale = 0` is exact CPMM. Positive `peak_depth` and `imbalance_scale` activate the Dusk Concentrated AMM: `peak_depth` states the extra marginal-depth multiplier at the internally observed center, while `imbalance_scale` controls how quickly that extra depth fades toward the exact CPMM tail. These are the only invariant knobs. Fees, EMA half-life, adjustment threshold, and recenter velocity remain separate controller settings. Trades move reserves and produce price observations; a time-decayed internal EMA guides only funded, bounded center adjustments. Dusk never consults an external oracle.

hLP checkpointing computes NAV, attempts the spot-based leverage adjustment, records any unexecuted amount in `pending_rebalance`, and refreshes a cached settlement reference. The adjustment mints or burns balanced yLP, so the quoted post-swap spot is preserved within rounding and there is no hidden second price move after the user output. Leverage-up is capped by borrowed-side cash headroom; when cash is unavailable, ordinary swaps remain live and the gap is carried forward as pending rebalance. hLP open/close uses the cached reference to block settlement when spot has moved beyond `settlement_divergence_bps`.

## PDA Map

| Account | Seeds | SDK helper |
| --- | --- | --- |
| `Market` | `market_v2`, `base_mint`, `quote_mint`, `params_hash` | `deriveMarketAddress` |
| Reserve vault | `market_reserve`, `market`, `asset_mint` | `deriveMarketReserveVaultAddress` |
| Collateral vault | `market_collateral`, `market`, `asset_mint` | `deriveMarketCollateralVaultAddress` |
| Swap fee vault | `market_fee`, `market`, `asset_mint` | `deriveMarketFeeVaultAddress` |
| Interest vault | `market_interest`, `market`, `asset_mint` | `deriveMarketInterestVaultAddress` |
| Borrow position | `borrow_position_v2`, `market`, `position_id` | `deriveBorrowPositionAddress` |
| Referral partner | `referral_partner`, `referrer` | `deriveReferralPartnerAddress` |
| Referral accrual | `referral_accrual`, `referral_partner`, `market`, `asset_mint` | `deriveReferralAccrualAddress` |
| Yield account | `yield`, `market`, `owner`, `asset_mint`, `token_kind` | `deriveYieldAccountAddress` |
| Insurance vault | `insurance`, `market`, `asset_mint` | `deriveInsuranceAddress` |
| Leverage position | `leverage_position_v2`, `market`, `position_id` | `deriveLeveragePositionAddress` |
| Leverage delegation | `leverage_delegation_v2`, `leverage_position` | derive from seed tuple |
| Leverage collateral vault | `leverage_collateral`, `market`, `collateral_mint` | derive from seed tuple |
| LP token metadata | Metaplex `metadata`, token metadata program, `lp_mint` | `deriveTokenMetadataAddress` |

yLP and hLP mints are supplied to `initialize` and validated by mint authority, decimals, Token-2022 owner, transfer hook, fee-free extension rules, no freeze authority, vanity suffix, and zero supply at market creation. LP metadata is created in follow-up `initialize_lp_metadata` calls, one mint per transaction.

Referral accruals are market-specific liabilities. Their backing remains in the corresponding market interest vault until the referrer claims to the partner's current recipient.

## Event Surface

Indexers should consume Dusk events from the standalone Dusk IDL:

- `MarketCreated`, `MarketUpdated`, `MarketHealthUpdated`
- `LiquidityAdded`, `LiquidityRemoved`
- `YieldRecipientUpdated`, `YieldClaimed`, `MarketFeeLiabilityClaimed`, `ProtocolFeesClaimed`
- `SwapExecuted`, `SwapSettled`, `HlpRebalanced`
- `MarketCollateralDeposited`, `MarketCollateralWithdrawn`, `MarketDebtUpdated`
- `PositionLiquidated`
- `HlpOpened`, `HlpClosed`
- `LeveragePositionOpened`, `LeveragePositionClosed`, `LeveragePositionUpdated`, `LeveragePositionLiquidated`
- `LeverageDelegationUpdated`
- `ReferralInterestShareCapUpdated`, `ReferralPartnerConfigured`, `ReferralRecipientUpdated`, `ReferralBound`, `ReferralInterestAccrued`, `ReferralInterestClaimed`

Market-scoped Dusk events carry `MarketEventMetadata` with signer, market, and slot. Protocol-wide authority, referral-recipient, and referral-claim events instead expose their authority or signer directly because they are not tied to one market.

Indexers must treat `SwapExecuted` and `SwapSettled` as the same swap stream.
`SwapSettled` replaces `SwapExecuted` when a CPMM swap also executes hLP token
changes; it uses `asset_in_side` instead of repeating both mint addresses and
omits metadata to keep that composite path within its heap budget. Both events
contain the same fee and price telemetry. `end_price_nad` is the
invariant-preserving trade endpoint; `reserve_end_price_nad` is the final pool
marginal price after any retained surcharge has entered executable reserves.
Leverage position events expose their embedded AMM leg as `swap`; margin-only
updates set it to `None`.

## Core Invariants

- yLP supply is backed by paired base/quote principal accounting.
- No operation mints yLP without corresponding reserve value.
- yLP principal reserves exclude fee and interest vault balances; only retained dynamic surcharge may become new swap principal.
- Fee liabilities must be backed by fee and interest vault balances.
- Base fees and lending interest never fund concentration recentering.
- A normal trade checkpoints the current curve as economically neutral; retained dynamic surcharge is the only swap path that creates protected recentering budget.
- Recenter and parameter-ramp points are admitted only when their Dusk Concentrated AMM `Q` impairment is funded.
- CPMM and Dusk Concentrated AMM swaps, previews, lending risk, liquidation risk, leverage, and hLP use one applied curve definition.
- hLP NAV is `collateral_value - debt_value` and must not underflow.
- hLP debt shares stay matched to aggregate hLP vault debt.
- hLP operations never use yLP-denominated debt.
- Isolated leverage debt contributes to utilization without entering normal borrower health.
- Leverage collateral vault balances are matched by open leverage position collateral accounting.
- Delegated close must validate both the delegate's close approval and settlement approval return data.
- Individual borrower health uses all position collateral and the position's stored liquidation CF.
- Global-health contributions are debt-capped underwriting signals and never prevent collateral withdrawal or change another position's stored terms.
- Conservative risk depth uses the lower internally observed `Q` state and reconstructs pessimistic reserve shapes on the exact applied CPMM/Dusk Concentrated AMM curve; there is no external-oracle or hidden CPMM fallback for concentrated markets.
- Referral binding never changes requested principal, debt, interest, health, or liquidation terms; accruals are carved only from realized protocol interest revenue.
- Referral-interest claims are bounded by realized protocol revenue and pay only the partner's current designated recipient.
- Risk books update EMA values from cached pre-transition observations and store current observations for the next refresh.
- Liquidation follows the waterfall: borrower collateral, liquidator incentive, insurance, then bounded LP socialization.

## Verification

Useful focused checks while changing Omnipair Dusk (v2):

```bash
cargo fmt -p dusk -- --check
cargo check -p dusk --lib
cargo test -p dusk --lib -- --nocapture
cargo test -p leverage_delegate
anchor build -p dusk
anchor build -p leverage_delegate
npm run check-idl-current --prefix packages/dusk-sdk
npm run build --prefix packages/dusk-sdk
yarn test-litesvm
```

Run dusk-sdk builds whenever public IDL, account, event, seed, or instruction shapes change. `check-idl-current` must pass after `anchor build -p dusk` so committed client files match generated build artifacts.
