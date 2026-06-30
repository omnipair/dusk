<p align="center">
  <img src="assets/omnipair-dusk-hero.png" alt="Omnipair Dusk (v2)" width="100%" />
</p>

# Omnipair Dusk

**Omnipair Dusk (v2)** is an oracle-less lending protocol on Solana.

Dusk is the next generation of Omnipair: a standalone market program that brings swaps, lending, yield-bearing liquidity, leveraged LP vaults, and isolated spot-margin leverage into one capital-efficient protocol without relying on external price oracles.

## Overview

Omnipair's GAMM (Generalized Automated Market Maker) combines a constant-product market maker with an integrated lending market. Liquidity providers deposit both sides of a pair, traders swap against the unified reserves, and borrowers can use one side of the market as collateral to borrow the other.

Dusk keeps that core Omnipair GAMM idea and rebuilds it around a market-native account model:

- **Oracle-less markets**: pricing and risk use in-protocol reserve state, EMA books, and conservative settlement references instead of external oracle feeds.
- **Unified liquidity and lending**: LP inventory backs both swaps and borrow demand, letting capital serve multiple protocol flows.
- **Standalone V2 program**: Dusk has its own program ID, IDL, account model, event surface, and SDK helpers.
- **Yield-bearing LP shares**: `yLP` represents a two-sided liquidity claim while reserve-side yield is checkpointed through base and quote growth indexes.
- **Leveraged LP vaults**: base and quote `hLP` mints are aggregate 2x LP vault shares that target one-sided market exposure through explicit hLP live-reserve accounting.
- **Isolated leverage**: traders can open market-local spot-margin positions that borrow one side, swap through the GAMM, hold the opposite side as collateral, delegate TP/SL close execution, and liquidate through the same reserve accounting.
- **Cached risk books**: risk checks roll EMA values from cached observations so settlement does not depend on a same-instruction manipulated spot.
- **Bounded liquidation waterfall**: liquidations move through borrower collateral, liquidator incentive, insurance, then bounded LP socialization.

## How It Works

Each Dusk market is defined by a base mint, quote mint, and market parameters. The market records principal reserves, fee and interest liabilities, borrower debt, yield accounts, and aggregate hLP vault state.

```text
Liquidity providers
  deposit base + quote
  receive yLP
  claim swap fees and borrow interest through yield indexes

Traders
  swap base <-> quote
  pay fees into side-specific fee vaults
  trigger O(1) hLP vault checkpoints when needed

Borrowers
  deposit collateral
  borrow the opposite market asset
  remain bounded by recognized-collateral health checks

hLP users
  deposit one market asset
  receive aggregate leveraged LP vault shares
  close by burning hLP and settling the vault's funding debt

Leverage users
  deposit margin in one market asset
  borrow the same debt side internally
  swap borrowed notional into the opposite collateral asset
  repay, unwind, or get liquidated against market-local reserves
```

## Token Model

Dusk markets use Token-2022 mints for protocol LP surfaces:

| Token | Meaning |
| --- | --- |
| `yLP` | Floating two-sided LP share for normal liquidity providers |
| base `hLP` | Aggregate leveraged LP vault share targeting base exposure |
| quote `hLP` | Aggregate leveraged LP vault share targeting quote exposure |

Normal LPs enter with `add_liquidity`, depositing both assets at the current market ratio:

```text
asset_claim = user_ylp_shares * live_reserve / total_ylp_supply
```

Fees and borrow interest do not auto-compound into principal reserves. They accrue in fee and interest vaults, are tracked through side-specific growth indexes, and are claimed through `claim_yield`.

## Isolated Leverage

Dusk also includes isolated spot-margin leverage. A leverage position is a market-local account owned by the user:

```text
user margin + isolated borrow
  -> internal GAMM swap
  -> collateral held in a leverage collateral vault
  -> debt tracked in isolated debt buckets
```

Users can increase or decrease exposure, add or remove margin, close the position, or be liquidated if the closeout value falls below maintenance requirements. Isolated debt contributes to utilization and interest accrual, but it is kept separate from normal borrower debt and hLP vault debt.

Owners can also approve a leverage delegate program for a position. The delegate flow uses a before-hook approval and after-hook settlement approval, so keepers can execute take-profit or stop-loss closes into a custody PDA without receiving unchecked control over the position.

## hLP Vaults

Each market maintains two aggregate hLP vaults:

- `hLP_base`: users deposit base and the vault funds the quote leg.
- `hLP_quote`: users deposit quote and the vault funds the base leg.

Opening an hLP position mints vault shares against aggregate vault NAV. The target-side deposit is reserve cash; the funded side is tracked as hLP funding debt and an explicit hLP live-reserve component:

```text
user target asset
  -> hLP vault records opposite-side funding debt
  -> market credits balanced live reserves
  -> vault receives yLP
  -> user receives hLP_target
```

Closing burns hLP shares, removes the vault's proportional yLP liquidity, repays funding debt, realizes any interest from borrowed-side cash, and returns remaining target-side inventory to the user.

## Risk Model

Dusk is designed around market-local risk accounting:

- Lending is isolated by market.
- Borrow health uses recognized debt-bearing collateral, not idle collateral balances.
- Isolated leverage has its own position state and debt buckets.
- Price and risk books use cached EMA state to reduce same-transaction spot manipulation.
- hLP settlement uses cached settlement references and divergence guards.
- Swaps stay live when hLP leverage-up is cash-constrained; unexecuted rebalance is stored as `pending_rebalance`.
- Market health, insurance, and LP socialization bound how losses move through the system.

## Instruction Surface

The V2 program exposes simple market actions:

```text
initialize
update_config
set_reduce_only
add_liquidity
remove_liquidity
set_yield_recipient
claim_yield
swap
deposit_collateral
withdraw_collateral
borrow
repay
liquidate
open_hedge
close_hedge
open_leverage
close_leverage
delegated_close_leverage
increase_leverage
decrease_leverage
add_leverage_margin
remove_leverage_margin
liquidate_leverage
create_leverage_delegation
update_leverage_delegation
close_leverage_delegation
```

Futarchy and protocol revenue administration mirror the legacy Omnipair flow:

```text
init_futarchy_authority
update_futarchy_authority
update_protocol_revenue
update_revenue_recipients
set_global_reduce_only
claim_protocol_fees
```

## Integrator Notes

Dusk is not a drop-in account rename for Omnipair V1. Integrations should route by program generation and program ID:

- Use the Dusk IDL and market PDAs for V2 markets.
- Store V1 pair metrics and Dusk market metrics separately at the source level, then aggregate them under the Omnipair brand in analytics.
- Do not sort Dusk market mints client-side. The creator's `base_mint` and `quote_mint` order defines the market and its price direction.
- Treat yLP and hLP mints as distinct Token-2022 token concepts. yLP is the two-sided normal LP token; hLP tokens are aggregate leveraged LP vault shares.
- Consume Dusk events from the standalone V2 IDL, including market, liquidity, swap, debt, liquidation, yield, hLP, leverage, and leverage-delegation events.

## Core Invariants

Dusk keeps a live reserve coordinate for each side of the market:

```text
R_live[i] = R_cash[i] + D_cash_backed[i] + R_hLP_live[i]
```

where `i` is base or quote. Without hLP live depth this collapses to the V1 GAMM reserve invariant:

```text
R_live[i] = R_cash[i] + D_cash_backed[i]
```

That gives Dusk the same normal lending behavior as V1: cash-backed borrow decreases cash and increases debt by the same amount, so borrowing does not move the GAMM price.

```text
borrow a:
  R_cash[i]        -= a
  D_cash_backed[i] += a
  R_live[i]         unchanged
```

hLP adds a named synthetic live-reserve coordinate, not an unnamed exception. hLP funding debt is part of total utilization and accrues interest, but it is not same-side cash-backed reserve debt:

```text
D_total[i] = D_cash_backed[i] + D_hLP_funding[i]
```

Only `D_cash_backed` expands `R_live` through normal cash-backed interest accrual. hLP funding interest is carried by hLP debt/NAV and is settled from borrowed-side cash when realized.

Spot-neutral hLP rebalancing moves both live-reserve sides proportionally:

```text
dR_hLP_live[base]  / R_live[base]
= dR_hLP_live[quote] / R_live[quote]

P = R_live[quote] / R_live[base]
P' = P
```

That preserves spot, but not depth: finite swap quotes can change when hLP live depth changes. Swap-triggered hLP updates are therefore quote-aware and O(1), and never iterate over user positions.

Other invariants:

- yLP supply is backed by reserve-side principal accounting.
- No operation mints yLP without corresponding reserve value.
- yLP principal reserves exclude fee and interest vault balances.
- Fee liabilities are backed by fee and interest vault balances.
- Synthetic hLP live reserve is not directly withdrawable cash; swaps, withdrawals, debt repayment, and interest realization are still constrained by cash reserves.
- hLP NAV is `collateral_value - debt_value` and must not underflow.
- hLP solvency is enforced through NAV, cash headroom, settlement references, divergence guards, and balanced rebalance math.
- Dusk does not enforce `R_hLP_live[i] <= D_hLP_funding[i]` per asset; hLP live depth is a balanced GAMM coordinate, not a standalone per-asset liability.
- hLP debt shares stay matched to aggregate hLP vault funding debt.
- hLP operations never use yLP-denominated debt.
- Isolated leverage debt contributes to utilization without contaminating normal borrower health checks.
- Leverage collateral vault balances are matched by open leverage position collateral accounting.
- Delegated close requires both a close approval payload and a settlement approval payload from the approved delegate program.

## Changed Invariants From GAMM V1

The core GAMM primitive is intentionally preserved:

- The market is still priced from in-protocol reserves, not external oracles.
- Normal borrow and repay paths still preserve `R_live = R_cash + D_cash_backed`.
- Cash constraints still matter: virtual depth can quote, but only cash can leave vaults or settle realized liabilities.
- LP minting and burning still use the V1-style proportional reserve math with permanently locked minimum liquidity.
- Swap fees and borrow interest remain outside principal reserves and are distributed through fee and yield accounting.

Dusk extends the invariant set only where hLP needs native 2x LP tracking:

- V1 had no hLP component, so `R_hLP_live = 0`.
- Dusk allows only hLP transitions to mutate `R_hLP_live`.
- hLP leverage-up/deleverage updates are balanced reserve-coordinate moves, designed to preserve spot while changing depth.
- hLP funding debt affects utilization and funding cost, while hLP NAV and settlement guards enforce vault solvency.
- Cash-constrained hLP leverage-up does not block swaps; unexecuted rebalance is carried as `pending_rebalance`.

## Program ID

| Network | Program ID |
| --- | --- |
| Mainnet | `358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv` |
| Devnet | `358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv` |

## Verification

Core V2 verification gates:

```bash
anchor build -p omnipair_v2
cargo fmt -p omnipair-v2 -- --check
cargo check -p omnipair-v2 --lib
cargo test -p omnipair-v2 --lib -- --nocapture
npm run build --prefix packages/program-interface
yarn test-litesvm
```

Run the program-interface build whenever public IDL, account, event, seed, or instruction shapes change.

## Security And Status

Dusk is the standalone V2 Omnipair program. It is separate from the legacy V1 pair program and should be integrated by program generation, IDL, and program ID.

Before Dusk is treated as production-ready, it should complete final security review, release artifact verification, and owner signoff for app, SDK, indexing, analytics, aggregators, and deployment.
