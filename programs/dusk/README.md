# Omnipair Dusk (v2)

Omnipair Dusk (v2) is the standalone Dusk market program. It uses market terminology, floating yield LP shares, aggregate hedged LP vault accounting, and isolated spot-margin leverage.

## Source Boundaries

- `instructions/`: Anchor account validation, inventory movement, slippage checks, and events.
- `transitions/`: atomic accounting mutations with small receipts for events and tests.
- `state/`: account layouts, embedded market books, and invariants.
- `tokens/`: validation for Token-2022 yLP and hLP mints.
- `math/`: fixed-point, GAMM, EMA, valuation, and interest helpers.
- `utils/`: shared accounting helpers used by transitions.

Instruction modules are split by domain: `market`, `liquidity`, `yielding`, `spot`, `lending`, `leverage`, `referral`, and `futarchy`.

## Public Instructions

Omnipair Dusk (v2) exposes the current market instruction set:

- `initialize`, `initialize_lp_metadata`, `update_config`, `set_reduce_only`
- `add_liquidity`, `remove_liquidity`
- `set_yield_recipient`, `claim_yield`
- `swap`
- `deposit_collateral`, `withdraw_collateral`, `borrow`, `repay`
- `set_referral_recipient`, `claim_referral_fees`
- `trigger_liquidation_auction`, `bid_liquidation_auction`, `settle_liquidation_auction_amm`
- `deposit_single_sided`, `withdraw_single_sided`
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

Swap fees and borrow interest are non-compounding liabilities. They are held outside principal reserves in side-specific fee and interest vaults and distributed through side-specific growth indexes. `YieldAccount` stores owner checkpoints, accrued revenue, and an optional external revenue recipient for treasury or protocol-owned liquidity flows.

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

## Referral Origination

Referral is opt-in on `borrow`, `open_leverage`, and `increase_leverage`. A referred action charges the Futarchy-configured origination rate, currently initialized to 10 bps and bounded by the compile-time 25 bps maximum:

```text
fee_debit = ceil(requested_principal * configured_bps / 10_000)
gross_debt = requested_principal + fee_debit
```

The action uses requested principal, rather than gross debt, for the borrower payout or leverage trade and transfers `fee_debit` from the same reserve to the referrer's canonical per-mint ATA. Asset-level transfer fees can reduce token-account credit, so instruction minimum-output checks still apply. All underwriting, cash, daily-limit, debt-share, and principal mutations use `gross_debt`. The caller supplies `max_acceptable_referral_fee_bps`; referral parameters must either both be present or both be absent, and a stale transaction fails if its maximum is below the configured rate.

`ReferralProfile` is protocol-wide and keyed only by the referrer authority. It stores a rotatable recipient, while accrued balances remain in standard ATAs owned by the profile PDA. `claim_referral_fees` drains one mint ATA to a token account owned by the current recipient. Referred transfers support both legacy SPL Token and Token-2022 assets, including transfer fees and transfer hooks supplied through remaining accounts.

## Swaps And Rebalancing

`swap` is the Dusk swap entry. It transfers inventory, routes swap fees to the fee vault, applies GAMM reserve movement, and checkpoints both aggregate hLP vaults in O(1).

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
| Referral profile | `referral_profile`, `referrer` | `deriveReferralProfileAddress` |
| Yield account | `yield`, `market`, `owner`, `asset_mint`, `token_kind` | `deriveYieldAccountAddress` |
| Insurance vault | `insurance`, `market`, `asset_mint` | `deriveInsuranceAddress` |
| Leverage position | `leverage_position_v2`, `market`, `position_id` | `deriveLeveragePositionAddress` |
| Leverage delegation | `leverage_delegation_v2`, `leverage_position` | derive from seed tuple |
| Leverage collateral vault | `leverage_collateral`, `market`, `collateral_mint` | derive from seed tuple |
| LP token metadata | Metaplex `metadata`, token metadata program, `lp_mint` | `deriveTokenMetadataAddress` |

yLP and hLP mints are supplied to `initialize` and validated by mint authority, decimals, Token-2022 owner, transfer hook, fee-free extension rules, no freeze authority, vanity suffix, and zero supply at market creation. LP metadata is created in follow-up `initialize_lp_metadata` calls, one mint per transaction.

Referral vaults are canonical associated token accounts for `(ReferralProfile, asset_mint)`, using the asset mint's token program. They are not market-specific, so fees for the same mint aggregate across Dusk markets.

## Event Surface

Indexers should consume Dusk events from the standalone Dusk IDL:

- `MarketCreated`, `MarketUpdated`, `MarketHealthUpdated`
- `LiquidityAdded`, `LiquidityRemoved`
- `YieldRecipientUpdated`, `YieldClaimed`, `MarketFeeLiabilityClaimed`, `ProtocolFeesClaimed`
- `SwapExecuted`, `HlpRebalanced`
- `MarketCollateralDeposited`, `MarketCollateralWithdrawn`, `MarketDebtUpdated`
- `PositionLiquidated`
- `HlpOpened`, `HlpClosed`
- `LeveragePositionOpened`, `LeveragePositionClosed`, `LeveragePositionUpdated`, `LeveragePositionLiquidated`
- `LeverageDelegationUpdated`
- `ReferralOriginationFeeUpdated`, `ReferralRecipientUpdated`, `ReferralOriginationFeePaid`, `ReferralFeesClaimed`

Market-scoped Dusk events carry `MarketEventMetadata` with signer, market, and slot. Protocol-wide authority, referral-recipient, and referral-claim events instead expose their authority or signer directly because they are not tied to one market.

## Core Invariants

- yLP supply is backed by paired base/quote principal accounting.
- No operation mints yLP without corresponding reserve value.
- yLP principal reserves exclude fee and interest vault balances.
- Fee liabilities must be backed by fee and interest vault balances.
- hLP NAV is `collateral_value - debt_value` and must not underflow.
- hLP debt shares stay matched to aggregate hLP vault debt.
- hLP operations never use yLP-denominated debt.
- Isolated leverage debt contributes to utilization without entering normal borrower health.
- Leverage collateral vault balances are matched by open leverage position collateral accounting.
- Delegated close must validate both the delegate's close approval and settlement approval return data.
- Individual borrower health uses all position collateral and the position's stored liquidation CF.
- Global-health contributions are debt-capped underwriting signals and never prevent collateral withdrawal or change another position's stored terms.
- Conservative risk depth uses one K EMA, reconstructed at the current spot ratio and capped by live depth; there are no spot/EMA-divergence or K-drawdown action halts.
- A referred borrow or leverage increase records requested principal plus fee debit as gross debt and removes that same total from reserve cash.
- Referral fee claims are bound to the canonical profile vault and current designated recipient.
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
