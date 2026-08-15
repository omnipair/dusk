# Omnipair V2 (Dusk)

Dusk is the standalone market program for Omnipair V2. It uses market terminology, floating yield LP shares, aggregate hedged LP vault accounting, isolated spot-margin leverage, direct-yLP parameter governance, and an optional oracle-less Dusk Concentrated AMM. See [`CONCENTRATION.md`](./CONCENTRATION.md) for the curve, recentering, fee, and protected-liquidity specification.

See [`COMPUTE_BENCHMARKS.md`](./COMPUTE_BENCHMARKS.md) for the complete
53-instruction LiteSVM CU table, including sample counts, averages, observed
maxima, headroom, and deterministic swap-path regression ceilings.

See [`AUDIT_STATUS.md`](./AUDIT_STATUS.md) for the authoritative disposition of
known internal findings. Dated files under the ignored `.audit/` directory are
historical work products and do not independently describe the current tree.

## Source Boundaries

Rust source follows the V1-inspired conventions in [`STYLE.md`](./STYLE.md).

- `instructions/`: Anchor account validation, token CPIs, custody reconciliation, slippage checks, and events. Small administrative entrypoints are grouped by domain; complex money paths remain separate.
- `state/`: one file per serialized Anchor account. Embedded values live with their owning account instead of becoming pseudo-state modules.
- `market/`: the four shared Market behaviors: AMM, liquidity, lending, and leverage. Operations are written directly without a generic transition or wrapper layer.
- `math/`: pure fixed-point concentrated-AMM/CPMM, dynamic-fee, EMA, valuation, and interest algorithms.
- `shared/`: reusable account, token, and arithmetic adapters that do not own protocol state transitions.

Instruction modules are split by domain: `market`, `governance`, `liquidity`, `spot`, `lending`, `leverage`, `referral`, and `futarchy`.

| Flow | Anchor adapter | Domain owner |
| --- | --- | --- |
| Swap and preview | `instructions/spot`, `instructions/prepare_swap`, `instructions/preview` | `state/market.rs`, `market/amm.rs` |
| yLP and hLP | `instructions/liquidity` | `state/market.rs`, `market/liquidity.rs` |
| Borrow and liquidation | `instructions/lending` | `state/borrow_position.rs`, `state/market.rs`, `market/lending.rs` |
| Isolated leverage | `instructions/leverage` | `state/leverage_position.rs`, `state/leverage_delegation.rs`, `market/leverage.rs` |
| Parameter governance | `instructions/governance` | `state/parameter_proposal.rs`, `state/proposal_support.rs`, `state/market.rs` |

## Public Instructions

Dusk exposes the current market instruction set:

- `initialize_market`, `initialize_lp_metadata`, `initialize_yield_accounts`, `initialize_lp_transfer_hook`, `set_market_reduce_only`
- `create_parameter_proposal`, `support_parameter_proposal`, `queue_parameter_proposal`, `execute_parameter_proposal`, `withdraw_parameter_support`
- `add_liquidity`, `remove_liquidity`
- `set_yield_recipient`, `claim_yield`
- `swap`
- `deposit_collateral`, `withdraw_collateral`, `borrow`, `repay`
- `configure_referral_partner`, `initialize_referral_accrual`, `set_referral_recipient`, `claim_referral_interest`
- `trigger_liquidation_auction`, `bid_liquidation_auction`, `settle_liquidation_auction_floor`
- `deposit_single_sided`, `withdraw_single_sided`
- `open_leverage`, `close_leverage`, `delegated_close_leverage`, `increase_leverage`, `decrease_leverage`, `add_leverage_margin`, `remove_leverage_margin`, `liquidate_leverage`
- `create_leverage_delegation`, `update_leverage_delegation`, `close_leverage_delegation`
- `preview_market`, `preview_add_liquidity`, `preview_swap`, `preview_borrow_capacity`, `preview_borrow_position`
- Futarchy and revenue administration: `init_futarchy_authority`, `update_futarchy_authority`, `update_protocol_revenue`, `update_revenue_recipients`, `update_protocol_auction_config`, `update_protocol_auction_recipients`, `update_protocol_auction_route`, `set_global_reduce_only`, `settle_protocol_auction`

`settle_protocol_auction` requires an explicit revenue `source` in addition to
the `fee` or `buyback` lane. `swap` sources sell from reserve-vault custody;
`interest` sources sell from the side-specific interest vault. Settlement
debits only the liability bucket selected by that lane/source pair.

Protocol-auction governance is intentionally retroactive for unsettled
inventory. A lane keeps each market/source inventory epoch's original start
slot, while settlement uses the lane's current accepted mint, price curve,
reference-age limit, and recipients. Changing the accepted mint can pause an
affected market until governance installs a matching route; existing inventory
is not repriced under a snapshotted historical configuration.

## Direct-yLP Parameter Governance

Markets have no manager or operator. Any direct yLP holder can create a typed
parameter proposal by burn-locking at least 1% of eligible direct yLP. Other
direct holders support it by burn-locking more yLP. Support is one-sided:
holders who do not support remain silent or exit. Strictly more than 50% of
eligible direct yLP queues the proposal for a seven-day wall-clock timelock,
followed by a seven-day execution window.

Eligible supply excludes yLP held inside the two hLP vaults and adds back yLP
already locked in governance. A lock continues earning both reserve-side yield
streams through a proposal-local virtual ledger. Queued support cannot be
withdrawn; after execution, expiry, staleness, or cancellation,
`withdraw_parameter_support` remints the exact locked amount and merges its
yield into the holder's normal `YieldAccount`s.

Each proposal changes exactly one family: the complete fee profile,
concentration shape plus its 216,000–1,512,000-slot ramp duration, IRM, the four
EMA half-lives, or the daily borrow limit. Independent family revisions make
competing proposals stale instead of silently combining them. Execution first
checkpoints old interest/EMA/risk state and rejects at 80% utilization.

Parameter bounds are enforced on creation and again on execution. Aggregate
base/divergence/volatility fee budgets are capped at 5,000 bps; the daily borrow
limit is capped at 3,000 bps. IRM defaults are 7,000 bps target utilization,
4 NAD steepness, and adjustment speed 20/year, with allowed ranges of
6,000–7,500 bps, 2–8 NAD, and 1–50/year respectively.

`max_daily_borrow_bps` sizes one public-lending borrow bucket per debt asset
against conservative depth. Only gross principal borrowed through the lending
`borrow` path consumes it; isolated leverage and direct or automatic hLP funding
do not, because those operations do not lend cash out of the market. The bucket
refills continuously over 24 hours with remainder accounting that is
checkpoint-frequency-independent while the absolute limit is fixed; it is a
leaky/token bucket, not an exact trailing-window sum. Conservative-depth
changes may resize the bps-derived absolute limit. Repayments and hLP/leverage
exits do not refund already consumed flow capacity.

## Token Model

Each market records three Token-2022 LP mints:

- `yLP`: the normal two-sided LP share for balanced base/quote liquidity.
- `hLP_base`: one-sided hedged LP shares targeting base exposure.
- `hLP_quote`: one-sided hedged LP shares targeting quote exposure.

yLP and hLP mints must be fee-free Token-2022 mints with an immutable transfer hook configured to the Dusk program (`TransferHook.authority = None`), mint authority set to the market PDA, and no freeze authority. `initialize_lp_metadata` creates Metaplex metadata for each LP mint with the market PDA as update authority. Production builds additionally enforce vanity suffixes: `yLP` for yLP and `hLP` for each hLP mint. Underlying asset mints may be SPL Token or Token-2022 mints accepted by the shared mint validator.

## yLP Liquidity

`add_liquidity` is the normal LP entry. Users deposit both market assets at the current market ratio and receive one fungible `yLP` token.

yLP shares are floating two-sided principal shares:

```text
base_claim  = user_ylp_shares * base_live_reserve  / total_ylp_supply
quote_claim = user_ylp_shares * quote_live_reserve / total_ylp_supply
```

There is no fixed 1:1 protected-principal LP or separate public fee-eligibility step. `remove_liquidity` burns yLP and returns pro-rata base and quote executable principal reserves subject to cash availability and user slippage bounds; it cannot redeem the protocol-owned protected recenter buckets.

Base swap fees, distributed dynamic surcharge, and borrow interest are non-compounding liabilities. Swap-fee liabilities stay physically in the reserve vault as `swap_fee_custody_balance`, outside executable `cash_reserve`; interest liabilities stay in the side-specific interest vault. Public-borrow interest uses the all-yLP Q64 lane and public carry. hLP funding interest publishes into ordinary yLP's existing interest-growth index using the operation-frozen total-yLP-minus-both-hLP denominator and a dedicated source carry. Both hLP checkpoints advance across that delta with zero eligible shares, and the source carry is cleared when only `MIN_LIQUIDITY` remains outside the hLP vaults. While recenter protection is being funded, retained dynamic surcharge accumulates in side-specific, non-quoteable protected reserves. A funded recenter deploys those atoms once; ordinary yLP withdrawals cannot claim them. `YieldAccount` stores its LP mint, owner checkpoints, Q64 remainders, accrued revenue, and an optional external revenue recipient for treasury or protocol-owned liquidity flows.

Token-2022 does not invoke transfer hooks for arbitrary `Burn`. An untracked
direct yLP burn is therefore an intentional, irreversible donation: Dusk's
internal yLP denominator does not shrink, the burned share's future yield is
not redistributed, and the destroyed balance cannot authorize principal
withdrawal. Parameter support is the explicit exception: Dusk checkpoints the
holder, records the burn in `governance_locked_ylp`, maintains virtual yield,
and authorizes the matching terminal remint.

A partial direct hLP burn is recognized lazily on the next deposit or withdrawal for that hLP side. Dusk first checkpoints nested yLP growth against the old stored supply, then replaces the stored hLP supply with the smaller nonzero live mint supply before pricing the operation. Burned hLP principal is donated to the remaining holders; historical nested yield attributable to the burned balance remains stranded, while future nested yield uses the reconciled live supply. If every hLP atom is burned directly, no holder remains to authorize the normal final exit: the side is a deliberately fail-closed zombie and later hLP deposits/withdrawals reject. There is no governance sweep or asynchronous recovery path. Normal exits must use Dusk's remove/withdraw instructions.

LP custody must also preserve an authority that can sign Dusk's claim and recipient-update instructions. A normal wallet works directly; a PDA owner works only when its controlling program invokes Dusk with `invoke_signed`. SPL multisig-owned LP token accounts are not supported because the multisig account itself cannot satisfy Dusk's owner-signer constraint. Sending LP tokens to unsupported custody can make that custody's accrued yield unreachable.

## hLP Vaults

**Development status:** Dusk has not been deployed. New hLP deposits are live
and use the same behavior in development and prospective release builds.

Active hLP uses the exact applied CPMM or Dusk Concentrated AMM. On every
active-hLP swap or swap-like leverage operation, both hLP numeraires are
planned jointly from one frozen state. Dusk applies the canonical base-then-
quote yLP/debt adjustments first, checkpoints the resulting executable curve
state, and only then runs the ordinary applied-curve quote and fee engine.

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

Closing hLP burns hLP shares, burns the vault's proportional yLP, repays the borrowed-side vault debt, and returns remaining target-side inventory to the user. hLP debt is denominated in the borrowed underlying asset and tracked on the aggregate hLP vault, not as borrower margin debt. Deleveraging that removes executable cash without moving tokens reclassifies those atoms into source-scoped, non-executable `hLP_backing_inventory`; partial and final hLP exits release the corresponding pro-rata or exact remainder. This preserves `physical vault >= cash + fee custody + hLP backing` without treating backing as a second NAV claim.

hLP funding debt pays the full indexed rate. Its realized interest never
rebates either the paying hLP or the opposite hLP: after measured Token-2022
credit and the protocol split, only operation-start non-hLP yLP supply receives
the LP portion. `MIN_LIQUIDITY` remains in that denominator as a permanently
burned backstop. With no ordinary yLP holder, the funded LP liability stays
backed but locked and cannot be captured by a later deposit. Eligibility is
payment-time rather than accrual-time; public-borrow interest and swap fees are
unchanged and may still accrue to hLP-owned yLP under their normal rules.

Automatic deleverage pays funding interest inside the paying hLP's
borrowed-asset yLP burn leg, never through an additional debit shared by the
other hLP. If accrued interest exceeds that leg, Dusk uses the exact applied
curve to retain only the target-side input required to buy the shortfall. The
resulting curve movement is included in the authoritative quote and hLP loss
guard. Direct hLP withdrawal keeps its separate exact-close settlement.

For either curve, the exact accepted endpoint must keep each active
vault's deposited-asset principal plus frozen public-interest claim within the
larger of one raw target atom and one part per million of its operation-start
economic NAV. hLP funding interest is not a claim because hLP-owned yLP is
ineligible for that source. Direct retained principal is preserved separately
and cannot hide trade loss. A joint exact post-trade correction removes the
remaining delta without granting a second loss budget. If rounding,
convergence, debt, or cash bounds prevent a safe worsening path, the operation
fails with `HlpSettlementUnavailable`; strictly restoring residual flow remains
admissible.

Every funding increase keeps total indexed hLP funding debt in the borrowed
asset within that asset's current cash. This prevents repeated deposits or
automatic rebalances from reusing the same cash headroom, but it is only an
admission bound. Dusk still does not implement terminal hLP insolvency recovery,
permissionless recapitalization, or a residual-loss waterfall for passive debt
growth; that remains an open High audit finding.

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

`swap` is the Dusk swap entry. It transfers inventory, applies the market's exact CPMM or Dusk Concentrated AMM reserve curve, charges the configured base plus divergence/volatility fees, records claimable fees as reserve-custodied liabilities excluded from executable liquidity, and checkpoints both aggregate hLP vaults in O(1).

The divergence potential is Huber-capped at the configured marginal share, and
both divergence and volatility receive explicit gross-input budgets. Together
with the base fee, configured component caps must sum to at most 5,000 bps.
Every accepted quote therefore leaves at least `ceil(gross_input / 2)` for
curve execution, including odd raw-token amounts. Launch markets support asset
decimals from zero through nine, and initialization rejects finer assets.
Routers and user slippage bounds decide whether the resulting market quote is
acceptable.

`concentrated_liquidity_share = 0` is exact CPMM. A positive share combines a nonzero full-range CPMM tail with one explicit concentrated band whose log-symmetric bounds are set by `range_width`. Quotes use at most three closed-form segments and two precomputed boundary crossings. Fees, EMA half-life, adjustment threshold, and recenter cadence remain separate controller settings. A trade is hedged and committed at its quoted endpoint before its observation can schedule a later, protected center move. Dusk never consults an external oracle.

hLP checkpointing computes endpoint NAV and reconstructs yLP ownership and funding debt algebraically so each vault's opposite-asset claim equals its debt to the canonical atom. The quote therefore prices the hedge through ordinary reserves; hLP leverage is not advertised as free trader-visible depth. There is no finite-difference, Jacobian, or Broyden solve in the swap path. A live-basis yLP burn still realizes accrued-but-unpaid interest, and positive funding transitions remain cash-capped. This is admission-only and never reserves cash from ordinary exits, repayment, liquidation, or deleveraging. Terminal insolvency recovery remains open.

## PDA Map

| Account | Seeds | SDK helper |
| --- | --- | --- |
| `Market` | `market_v2`, `base_mint`, `quote_mint`, `params_hash` | `deriveMarketAddress` |
| Reserve vault | `market_reserve`, `market`, `asset_mint` | `deriveMarketReserveVaultAddress` |
| Collateral vault | `market_collateral`, `market`, `asset_mint` | `deriveMarketCollateralVaultAddress` |
| Interest vault | `market_interest`, `market`, `asset_mint` | `deriveMarketInterestVaultAddress` |
| Borrow position | `borrow_position_v2`, `market`, `position_id` | `deriveBorrowPositionAddress` |
| Referral partner | `referral_partner`, `referrer` | `deriveReferralPartnerAddress` |
| Referral accrual | `referral_accrual`, `referral_partner`, `market`, `asset_mint` | `deriveReferralAccrualAddress` |
| Yield account | `yield`, `market`, `owner`, `lp_mint`, `asset_mint`, `token_kind` | `deriveYieldAccountAddress` |
| Parameter proposal | `parameter_proposal`, `market`, `proposer`, little-endian `nonce` | `deriveParameterProposalAddress` |
| Proposal support | `proposal_support`, `proposal`, `supporter` | `deriveProposalSupportAddress` |
| Insurance vault | `insurance`, `market`, `asset_mint` | `deriveInsuranceAddress` |
| Leverage position | `leverage_position_v2`, `market`, `position_id` | `deriveLeveragePositionAddress` |
| Leverage delegation | `leverage_delegation_v2`, `leverage_position` | derive from seed tuple |
| Leverage collateral vault | `leverage_collateral`, `market`, `collateral_mint` | derive from seed tuple |
| LP token metadata | Metaplex `metadata`, token metadata program, `lp_mint` | `deriveTokenMetadataAddress` |

yLP and hLP mints are supplied to `initialize_market`. The two asset mints and all three LP mints must be pairwise distinct, and each LP mint is validated by mint authority, decimals, Token-2022 owner, immutable Dusk transfer hook, fee-free extension rules, no freeze authority, vanity suffix, and zero supply at market creation. LP metadata is created in follow-up `initialize_lp_metadata` calls, one mint per transaction. The permissionless, idempotent `initialize_yield_accounts` creates both asset-stream accounts for one owner and LP mint; `initialize_lp_transfer_hook` creates and validates the canonical Token-2022 extra-account-meta PDA on-chain without a seeded client fixture.

Referral accruals are market-specific liabilities. Their backing remains in the corresponding market interest vault until the referrer claims to the partner's current recipient.

## Event Surface

Indexers should consume Dusk events from the standalone Dusk IDL:

- `MarketCreated`, `MarketReduceOnlyUpdated`, `MarketHealthUpdated`
- `LiquidityAdded`, `LiquidityRemoved`
- `YieldRecipientUpdated`, `YieldClaimed`
- `SwapExecuted`
- `MarketCollateralDeposited`, `MarketCollateralWithdrawn`, `MarketDebtUpdated`
- `BorrowPositionLiquidated`
- `HlpOpened`, `HlpClosed`
- `LeveragePositionOpened`, `LeveragePositionClosed`, `LeveragePositionUpdated`, `LeveragePositionLiquidated`
- `LeverageDelegationUpdated`
- `ProtocolAuctionConfigUpdated`, `ProtocolAuctionRecipientsUpdated`, `ProtocolAuctionRouteUpdated`, `ProtocolAuctionSettled`
- `ProtocolAuctionSplitUpdated`
- `ReferralInterestShareCapUpdated`, `ReferralPartnerConfigured`, `ReferralRecipientUpdated`, `ReferralBound`, `ReferralInterestAccrued`, `ReferralInterestClaimed`
- `ParameterProposalCreated`, `ParameterProposalSupported`, `ParameterProposalQueued`, `ParameterProposalExecuted`, `ParameterProposalSupportWithdrawn`

Most market-scoped Dusk events carry `MarketEventMetadata` with signer, market,
and slot. Compact hot-path receipts expose `market` and their relevant actor
directly; the transaction already supplies the slot and signature. Protocol-wide
authority, referral-recipient, and referral-claim events likewise expose their
authority or signer directly because they are not tied to one market.

`SwapExecuted` is the single canonical spot-swap receipt whether or not inline
hLP settlement changes tokens. It identifies the input by `asset_in_side`,
reports the trader's exact debit and net output credit, separates the three
fee components and retained surcharge, and records the final live reserves
after every inline state change. Derived prices, fee totals, controller
telemetry, and hLP residuals remain in previews or account state instead of the
event. The total swap fee is the sum of the three fee components; the claimable
portion is that total minus `retained_fee`. Swap, hLP, and lending-liquidation
receipts use the same CPI-event mechanism as every other Dusk event, so
indexers recover all protocol events reliably from inner instructions.
Leverage position events expose the same compact fee and amount receipt as
`swap`; only the actual claimable credit is added because Token-2022 fees can
make it differ from the nominal claimable debit. Margin-only updates set it to
`None`.

Liquidity and hLP receipts also carry the final executable reserves and supply
values needed to advance an indexer snapshot without reconstructing protocol
math. `LiquidityRemoved` reports reserve-vault debits separately from the
owner's net credits, because Token-2022 transfer fees can make those amounts
different.

## Core Invariants

- yLP supply is backed by paired base/quote principal accounting.
- No operation mints yLP without corresponding reserve value.
- yLP principal reserves exclude reserve-custodied swap-fee liabilities, protected recenter reserves, and interest-vault balances. Retained surcharge becomes yLP principal only in the center move it funds.
- A physical reserve vault must contain at least executable `cash_reserve + swap_fee_custody_balance + base_hlp_backing_inventory + quote_hlp_backing_inventory + protected_recenter_reserve`; protocol transitions conserve that accounted total exactly, while unsolicited token donations are tolerated but remain non-executable. Interest liabilities must be backed by the interest vault.
- Base fees and lending interest never fund concentration recentering.
- A normal trade checkpoints the current curve as economically neutral; retained dynamic surcharge is the only swap path that creates protected recentering budget.
- Recenter and parameter-ramp points are admitted only when their Dusk Concentrated AMM `Q` impairment is funded.
- CPMM and Dusk Concentrated AMM swaps, previews, lending risk, liquidation
  risk, leverage, and predictive hLP positioning use one applied curve
  definition.
- Every hLP funding increase leaves projected aggregate indexed funding debt no greater than current borrowed-asset cash; the cap never blocks debt-reducing paths and does not provide bounded loss or terminal insolvency recovery.
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

Useful focused checks while changing Omnipair V2 (Dusk):

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
