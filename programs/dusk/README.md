# Omnipair Dusk (v2)

Omnipair Dusk (v2) is the standalone Dusk market program. It uses market terminology, floating yield LP shares, aggregate hedged LP vault accounting, and isolated spot-margin leverage.

## Source Boundaries

- `instructions/`: Anchor account validation, inventory movement, slippage checks, and events.
- `transitions/`: atomic accounting mutations with small receipts for events and tests.
- `state/`: account layouts, embedded market books, and invariants.
- `tokens/`: validation for Token-2022 yLP and hLP mints.
- `math/`: fixed-point, GAMM, EMA, valuation, and circuit-breaker helpers.
- `utils/`: shared accounting helpers used by transitions.

Instruction modules are split by domain: `market`, `liquidity`, `yielding`, `spot`, `lending`, `leverage`, and `futarchy`.

## Public Instructions

Omnipair Dusk (v2) exposes the current market instruction set:

- `initialize`, `update_config`, `set_reduce_only`
- `add_liquidity`, `remove_liquidity`
- `set_yield_recipient`, `claim_yield`
- `swap`
- `deposit_collateral`, `withdraw_collateral`, `borrow`, `repay`, `liquidate_borrow_position`
- `deposit_single_sided`, `withdraw_single_sided`
- `open_leverage`, `open_collateral_margin_leverage`, `close_leverage`, `close_collateral_margin_leverage`, `delegated_close_leverage`, `increase_leverage`, `decrease_leverage`, `add_leverage_margin`, `remove_leverage_margin`, `deposit_leverage_collateral`, `withdraw_leverage_collateral`, `liquidate_leverage`
- `create_leverage_delegation`, `update_leverage_delegation`, `close_leverage_delegation`
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

Debt-margin opening keeps the original flow:

```text
user debt-asset margin + isolated debt borrow
  -> internal GAMM swap
  -> collateral held in a leverage collateral vault
  -> debt tracked in isolated debt buckets
```

Collateral-margin opening lets the same directional position be funded with the
held asset:

```text
user collateral-asset margin stays in the leverage vault
  + isolated debt borrow swapped with exact output
  -> target collateral multiplier
  -> debt tracked in the same isolated debt buckets
```

Debt-margin closes sell all collateral, repay debt, and settle in the debt
asset. Collateral-margin closes buy exactly the debt required for repayment and
return untouched collateral. Thus either directional exposure can be funded and
settled with either token in the market. Users can also deposit or withdraw
collateral independently from repaying or drawing debt.

Users can increase or decrease exposure, add or remove margin, and close the position. Liquidation is permissionless once closeout value falls below the maintenance threshold. Isolated leverage debt contributes to utilization and interest accrual, but it is kept separate from normal borrower debt and aggregate hLP vault debt.

Owners can approve a position-scoped `LeverageDelegation` PDA for a delegate program. Delegated close uses a before-hook approval payload and an after-hook settlement payload, allowing keeper-style take-profit or stop-loss execution while binding the close to the expected market, owner, position, delegation, output mint, recipient, and residual amount.

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
| Yield account | `yield`, `market`, `owner`, `asset_mint`, `token_kind` | `deriveYieldAccountAddress` |
| Insurance vault | `insurance`, `market`, `asset_mint` | `deriveInsuranceAddress` |
| Leverage position | `leverage_position_v2`, `market`, `position_id` | `deriveLeveragePositionAddress` |
| Leverage delegation | `leverage_delegation_v2`, `leverage_position` | derive from seed tuple |
| Leverage collateral vault | `leverage_collateral`, `market`, `collateral_mint` | derive from seed tuple |
| LP token metadata | Metaplex `metadata`, token metadata program, `lp_mint` | `deriveTokenMetadataAddress` |

yLP and hLP mints are supplied to `initialize` and validated by mint authority, decimals, Token-2022 owner, transfer hook, fee-free extension rules, no freeze authority, vanity suffix, and zero supply at market creation. LP metadata is created in follow-up `initialize_lp_metadata` calls, one mint per transaction.

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

Every Dusk event carries `MarketEventMetadata` with signer, market, and slot.

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
- Market health uses recognized debt-bearing collateral for borrower debt; idle collateral contributes zero.
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
