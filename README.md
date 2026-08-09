<p align="center">
  <img src="assets/omnipair-dusk-hero.png" alt="Omnipair V2 (Dusk)" width="100%" />
</p>

> **Experimental software.** Omnipair V2 (Dusk) is unaudited, incomplete, and under active development. It is published for research, review, and testing only. Do not deploy it to mainnet, integrate it in production, or use it with real funds until the implementation, tests, audits, and launch process are complete.

# Omnipair V2 (Dusk)

**Dusk** is the Omnipair V2 protocol architecture: an oracle-less lending
protocol on Solana.

Dusk is the next generation of Omnipair: a standalone market program that brings swaps, lending, yield-bearing liquidity, leveraged LP vaults, and isolated spot-margin leverage into one capital-efficient protocol without relying on external price oracles.

## Overview

Omnipair's GAMM (Generalized Automated Market Maker) combines an AMM with an integrated lending market. Dusk markets can use exact constant product or the optional oracle-less Dusk Concentrated AMM with amplified near-center depth and exact CPMM tails. Liquidity providers deposit both sides of a pair, traders swap against the unified reserves, and borrowers can use one side of the market as collateral to borrow the other.

Dusk keeps that core Omnipair GAMM idea and rebuilds it around a market-native account model:

- **Oracle-less markets**: pricing and risk use in-protocol reserve state, EMA books, and conservative settlement references instead of external oracle feeds.
- **Optional autonomous concentration**: the Dusk Concentrated AMM concentrates depth around an internal center, recenters only through its funded bounded controller during genuine user operations, and can ramp to or from exact CPMM without changing invariant families elsewhere in the protocol.
- **Path-aware bounded fees**: an outward divergence surcharge targets trending inventory stress while a separate volatility surcharge prices repeated chop. Each component has an explicit gross-input budget, and aggregate fees can never exceed 50% of the trader's input.
- **Unified liquidity and lending**: LP inventory backs both swaps and borrow demand, letting capital serve multiple protocol flows.
- **Standalone Dusk program**: Dusk has its own program ID, IDL, account model, event surface, and SDK helpers.
- **Yield-bearing LP shares**: `yLP` represents a two-sided liquidity claim while reserve-side yield is checkpointed through base and quote growth indexes.
- **Leveraged LP vaults**: base and quote `hLP` mints are aggregate 2x LP vault shares that target one-sided market exposure through explicit hLP live-reserve accounting.
- **Isolated leverage**: traders can open market-local leverage positions that borrow one side, swap through the GAMM, hold the opposite side as collateral, delegate TP/SL close execution, and liquidate through the same reserve accounting.
- **Permissioned referral revenue sharing**: Futarchy-listed referrers can bind to new borrow or leverage debt and earn a configured share of the DAO's realized interest revenue without changing borrower debt or rates.
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
  accrue claimable fees as non-executable reserve liabilities
  trigger O(1) hLP vault checkpoints when needed

Borrowers
  deposit collateral
  borrow the opposite market asset
  receive a stored liquidation CF under V1-style dynamic underwriting

hLP users
  deposit one market asset
  receive aggregate leveraged LP vault shares
  close by burning hLP and settling the vault's funding debt

Leverage users
  deposit margin in one market asset
  borrow the same debt side internally
  swap borrowed notional into the opposite collateral asset
  repay, unwind, or get liquidated against market-local reserves

Referrers
  are listed by Futarchy as protocol-wide ReferralPartners with an interest share
  bind that partner when a borrower or leverage user opens new debt
  accrue a share of realized DAO interest revenue per market and mint
  claim accrued revenue to the partner's designated recipient

Direct yLP holders
  burn-lock at least 1% of eligible direct yLP to sponsor a typed parameter proposal
  add support until the proposal holds strictly more than 50% of eligible direct yLP
  wait through a seven-day timelock before permissionless execution
  remint locked yLP, with its virtual yield preserved, after the proposal becomes terminal
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

Base swap fees, distributed dynamic surcharge, and borrow interest do not auto-compound into principal reserves. Swap-fee liabilities stay physically in the reserve vault as `swap_fee_custody_balance`, outside executable `cash_reserve`; interest liabilities stay in the side-specific interest vault. Both are tracked through side-specific growth indexes and claimed through `claim_yield`. Only dynamic surcharge retained while the protected recentering budget is below target becomes reserve principal; once funded, new surcharge returns to claimable yLP/hLP yield.

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

## Permissioned Referral Revenue Sharing

Futarchy may list any wallet or application as a referrer and configure its share of protocol interest revenue. A listed partner can be supplied when a borrow debt side is first opened or when a leverage position is opened. Dusk snapshots the capped share at that point. The partner and snapshotted share remain bound until the debt side is fully repaid or the leverage position closes; increasing existing debt cannot replace or reprice them. Deactivating a partner blocks new bindings only, so existing positions retain their agreed referral economics.

Referral never increases principal, debt, interest, LTV utilization, or liquidation risk. Borrowers receive and owe the same amounts as unreferred borrowers. When an interest payment credits the market interest vault, Dusk calculates:

```text
protocol_interest_revenue = floor(actual_interest_vault_credit * protocol_interest_bps / 10_000)
bound_referral_share      = min(partner_share_bps, runtime_cap_bps) at initial binding
referral_accrual          = floor(protocol_interest_revenue * bound_referral_share / 10_000)
```

Later partner, cap, or active-status changes apply only to new bindings. The referral amount is carved only from the DAO's configured share of realized interest; LP allocations are unchanged. Using actual vault credit keeps Token-2022 transfer fees from creating an unbacked claim.

Each `ReferralAccrual` is scoped to one partner, market, and asset mint. Funds remain in the market interest vault while the account records the claimable liability. The partner authority may rotate its designated recipient, and `claim_referral_interest` pays that recipient using the asset mint's SPL Token or Token-2022 program and transfer hooks.

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

Direct Token-2022 burns bypass transfer hooks. Dusk lazily reconciles a partial hLP burn before the next hLP deposit or withdrawal: existing nested yield is checkpointed against the old supply, the smaller nonzero live supply becomes the pricing denominator, and the burned principal benefits remaining holders. Burning the entire live hLP supply is unrecoverable and leaves that hLP side fail-closed; there is intentionally no asynchronous recovery instruction or governance sweep. Clients should always exit through Dusk's withdrawal instruction.

## Risk Model

Dusk is designed around market-local risk accounting:

- Lending is isolated by market.
- Individual health and liquidation use all collateral held by the position and its stored liquidation CF.
- Debt-capped global-health contributions improve new-borrow underwriting without locking collateral or changing existing terms.
- Conservative depth uses internal `Q` observations and reconstructs the exact applied CPMM/Dusk Concentrated AMM shape at pessimistic EMA prices; borrowing uses the lower of symmetric and directional price EMAs, while liquidation uses the symmetric EMA.
- Isolated leverage has its own position state and debt buckets.
- Price and risk books use cached EMA state to reduce same-transaction spot manipulation.
- hLP settlement uses cached settlement references and divergence guards.
- Swaps stay live when hLP leverage-up is cash-constrained; unexecuted rebalance is stored as `residual_exposure`.
- Each debt asset has one shared 24-hour leaky/token bucket for gross new
  principal from fixed lending, isolated leverage, direct hLP funding, and
  automatic hLP funding. For a fixed absolute limit, refill is independent of
  checkpoint frequency; repayments and exits do not refund flow capacity, and
  the bucket is not an exact trailing-window sum. Changes in conservative
  market depth may resize the bps-derived absolute limit.
- The borrow admission floor, shared borrow-flow limits, insurance, and LP
  socialization bound how losses move through the system.

## Instruction Surface

Dusk exposes simple market actions:

```text
initialize
initialize_lp_metadata
set_reduce_only
create_parameter_proposal
support_parameter_proposal
queue_parameter_proposal
execute_parameter_proposal
withdraw_parameter_support
add_liquidity
remove_liquidity
set_yield_recipient
claim_yield
swap
deposit_collateral
withdraw_collateral
borrow
repay
configure_referral_partner
initialize_referral_accrual
set_referral_recipient
claim_referral_interest
trigger_liquidation_auction
bid_liquidation_auction
settle_liquidation_auction_floor
deposit_single_sided
withdraw_single_sided
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
preview_market
preview_add_liquidity
preview_swap
preview_borrow_capacity
preview_borrow_position
```

Futarchy and protocol revenue administration:

```text
init_futarchy_authority
update_futarchy_authority
update_protocol_revenue
update_revenue_recipients
update_protocol_auction_config
update_protocol_auction_recipients
update_protocol_auction_route
set_global_reduce_only
settle_protocol_auction
```

Market parameters are deliberately split into five typed proposal families:
fees, concentration shape and ramp duration, IRM, EMA half-lives, and the daily
borrow limit. A proposal snapshots that family's revision, so execution becomes
stale if another proposal changes the same family first. Execution is blocked
at 80% utilization, while repayments, liquidations, collateral additions, and
cash-available LP exits remain live. Fee parameters are bounded to a combined
5,000 bps gross-input budget; IRM defaults are 70% target utilization, 4x curve
steepness, and adjustment speed 20/year.

Protocol-auction settlement always specifies both `lane` (`fee` or `buyback`)
and `source` (`swap` or `interest`). The source is never inferred: swap revenue
is sold from reserve-vault custody and debits the matching swap-fee liability;
interest revenue is sold from the side-specific interest vault and debits the
matching interest-fee liability.

Auction configuration is intentionally retroactive for unsettled inventory:
the local lane/source epoch keeps its original start slot, but settlement uses
the current accepted mint, price parameters, reference-age limit, and
recipients. An accepted-mint change may pause a market until governance updates
its route; no historical config version is stored per inventory epoch.

## Integrator Notes

Dusk is a standalone program and should be integrated through its own IDL, program ID, and market account model:

- Use the Dusk IDL and market PDAs for markets.
- Do not sort Dusk market mints client-side. The creator's `base_mint` and `quote_mint` order defines the market and its price direction.
- Treat yLP and hLP mints as distinct Token-2022 token concepts. yLP is the two-sided normal LP token; hLP tokens are aggregate leveraged LP vault shares.
- Use the referral builders for referred debt actions so the partner and accrual PDAs plus any Token-2022 transfer-hook accounts are included atomically.
- Use the parameter-proposal builders so sponsorship/support burns, virtual-yield checkpoints, proposal/support PDAs, and terminal remints remain atomic.
- Consume Dusk events from the standalone IDL, including market, liquidity, swap, debt, liquidation, yield, hLP, leverage, leverage-delegation, and referral events.

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
- yLP principal reserves exclude reserve-custodied swap-fee liabilities and interest-vault balances.
- A physical reserve-vault balance equals executable `cash_reserve + swap_fee_custody_balance`; interest liabilities are separately backed by the interest vault.
- Synthetic hLP live reserve is not directly withdrawable cash; swaps, withdrawals, debt repayment, and interest realization are still constrained by cash reserves.
- hLP NAV is `collateral_value - debt_value` and must not underflow.
- hLP solvency is enforced through NAV, cash headroom, settlement references, divergence guards, and balanced rebalance math.
- Dusk does not enforce `R_hLP_live[i] <= D_hLP_funding[i]` per asset; hLP live depth is a balanced GAMM coordinate, not a standalone per-asset liability.
- hLP debt shares stay matched to aggregate hLP vault funding debt.
- hLP operations never use yLP-denominated debt.
- Isolated leverage debt contributes to utilization without contaminating normal borrower health checks.
- Referral binding never changes principal, debt, interest, health, or liquidation terms; referral claims are bounded liabilities carved from realized protocol interest revenue.
- Referral claims can only debit the matching `ReferralAccrual` from its market interest vault and pay a token account owned by the partner's current designated recipient.
- Leverage collateral vault balances are matched by open leverage position collateral accounting.
- Delegated close requires both a close approval payload and a settlement approval payload from the approved delegate program.

## Changed Invariants From GAMM V1

The core GAMM reserve/lending relationship is preserved, while the swap invariant is now configurable:

- The market is still priced from in-protocol reserves, not external oracles.
- `peak_depth = 0, fade_scale = 0` is exact V1-style CPMM; positive values activate the independently implemented Dusk Concentrated AMM.
- `peak_depth` is the extra marginal-depth multiplier at the center, while `fade_scale` controls how much balance-factor error is tolerated before that extra depth fades toward CPMM. They are the only invariant knobs; fee, EMA, and recenter controls are separate.
- Swaps and conservative lending/liquidation shapes use the same applied curve.
- Normal borrow and repay paths still preserve `R_live = R_cash + D_cash_backed`.
- Cash constraints still matter: virtual depth can quote, but only cash can leave vaults or settle realized liabilities.
- LP minting and burning still use the V1-style proportional reserve math with permanently locked minimum liquidity.
- Base swap fees remain reserve-custodied outside executable cash, while borrow interest remains in the interest vault; both stay outside principal reserves and are distributed through yield accounting.
- Dynamic surcharge is claimable after the AMM's protected budget is funded; before then it is retained as the only fee-derived recentering principal.

Dusk extends the invariant set only where hLP needs native 2x LP tracking:

- V1 had no hLP component, so `R_hLP_live = 0`.
- Dusk allows only hLP transitions to mutate `R_hLP_live`.
- hLP leverage-up/deleverage updates are balanced reserve-coordinate moves, designed to preserve spot while changing depth.
- hLP funding debt affects utilization and funding cost, while hLP NAV and settlement guards enforce vault solvency.
- Cash-constrained hLP leverage-up does not block swaps; unexecuted rebalance is carried as `residual_exposure`.

## Program ID

| Network | Program ID |
| --- | --- |
| Mainnet | `358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv` |
| Devnet | `358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv` |

## Verification

Core Omnipair V2 (Dusk) verification gates:

```bash
anchor build -p dusk
anchor build -p leverage_delegate
cargo fmt -p dusk -- --check
cargo check -p dusk --lib
cargo test -p dusk --lib -- --nocapture
cargo test -p leverage_delegate
npm run check-idl-current --prefix packages/dusk-sdk
npm run build --prefix packages/dusk-sdk
yarn test-litesvm
```

Run the dusk-sdk build whenever public IDL, account, event, seed, or instruction shapes change. `check-idl-current` must pass after `anchor build -p dusk` so committed client files match the generated build artifacts.

## Security And Status

Dusk is the standalone market program for Omnipair V2.

Before Dusk is treated as production-ready, it should complete final security
review, release artifact verification, and owner signoff for app, SDK, indexing,
analytics, aggregators, and deployment.
