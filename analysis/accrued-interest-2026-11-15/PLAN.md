# Accrued-interest index: replacing the income receivable books

Status: plan, 2026-11-15. Base: `fix/nemesis-main-findings` at d8d3bff3. Nothing implemented.

## Why

LP interest is paid out only after borrowers pay it. Each holder's share is
tracked as pooled IOU units in four income receivable books. Those units live
in fixed-width numbers, so the books need epochs, in-place drain runs, archived
rolls (`roll_income_epoch`, the `IncomeEpoch` PDA), holder catch-up across
archives, and an accrual cap that forgoes interest when a book is full. Most of
the income findings from the last three audit passes came from this machinery
(INC-02, INC3-01..04, the dominant-borrower stall). The books also take most of
the emergency-liquidation heap margin.

The IOUs come from three rules held together (tranche plan, 2026-10-30):

1. Interest is claimable separately from the LP price.
2. Interest belongs to whoever held during the period it was earned.
3. Only cash borrowers have actually paid is paid out.

This plan drops rule 3 and keeps rules 1 and 2. Interest is credited to holders
when it accrues, through a plain per-holder index, and claims are paid in cash.
That is how lending pools such as Morpho and Euler already treat interest.

## Decisions (2026-11-15)

| Topic | Decision |
|---|---|
| LP interest (srLP, junior, hLP vaults) | Credited at accrual to a per-lane growth index; each holder's claim is balance × index change. Claims are paid in cash. |
| Defaulted interest | An ordinary pool loss. It goes through the existing waterfall: junior, then senior. There is no insurance fund. Junior capital therefore covers interest already credited to senior. |
| Credited interest (junior and senior) | Belongs to the holder and never absorbs losses. Losses hit capital only: junior, then senior. An interest-first waterfall was rejected: with claimable interest it rewards claiming before a loss lands, and making unclaimed interest absorb losses needs a shrink factor with the same precision problems as the books. |
| Protocol share | Paid only. One unpaid counter per asset; each interest repayment sends the protocol its pro-rata share at once. A default drops its unpaid share. |
| Operator share | Credited at accrual and claimed in cash, like LPs, and never absorbs losses. A clawback carried as a negative balance was rejected: it is gameable by an operator that stops earning. |
| Claim limit | Available cash only, the same as principal withdrawals. |
| Short liquidation recovery | Principal first for both borrow and leveraged liquidations; unpaid interest is then written off. A short recovery pays the protocol nothing, consistent with its paid-only share. The leverage `Liquidate` policy stops reusing the pro-rata repayment split. |
| Junior seed | Ordinary junior capital: it absorbs losses like other junior and can be wiped. Protecting it would move its loss onto senior. Junior APY shows the real number (display capped, e.g. ">999%"), and "—" while junior is wiped. |
| Accrual and pricing | Accrual never moves price or reserves; the credited claim offsets the new receivable. |

Consequences:

- An operator share credited before a default is covered by junior, then
  senior, because the pool fronted it. With a 30% operator share that is 30% of
  the defaulted base interest. Paid-only was considered and not chosen.
- v1 is not paid-only for the protocol: `Pair::update` takes the protocol fee
  from cash at each accrual and gives the uncovered part to LPs. Current v2 is
  paid-only, and this plan keeps v2's rule with less state.

## Ledger

Per asset, with every term in atoms:

```text
C   cash in the reserve vault, including interest paid but not yet claimed
D   borrower principal outstanding (public, leverage, hLP funding debt)
I   borrower interest outstanding (debt − principal), all shares
Q   credited, unclaimed interest owed to LP and operator holders
Pu  protocol share of I not yet repaid

C + D + I = P + Q + Pu        P = LP principal backing
curve_reserve = live_reserve − Q − Pu
```

Every interest event leaves `P` and `curve_reserve` unchanged:

| Event | Change | Effect on P |
|---|---|---|
| Accrual of `i` (LP+operator `a`, protocol `p`) | I += i, Q += a, Pu += p | none |
| Repayment of interest `x` (protocol part `x_p`) | C += x − x_p, I −= x, Pu −= x_p | none |
| Claim of `y` | C −= y, Q −= y | none |
| Write-off of interest `w` (protocol part `w_p`) | I −= w, Pu −= w_p | −(w − w_p), absorbed by the waterfall |

So depth, not price, absorbs claims and repayments. This is the same rule
NM-202 settled on ("hold price, less depth"), and `fund_interest` already
follows it today (curve unchanged, cash down).

hLP funding interest follows the same table. The opposite vault's debt grows,
`Q` grows for srLP and junior holders, and pricing nets the two so spot does
not move. The existing pay-or-carry rules for funding still decide when the
vault turns that debt into cash.

## Program changes

**New state** (Market, per asset): lane indexes for srLP and junior (the
existing `Fees.interest_growth_index_q64` and `TrancheIncome.interest_growth_q64`
become the live path), `Q`, `Pu`, and `operator_claimable`. hLP vaults keep
their existing vault interest indexes. Estimated under 200 bytes, replacing
`MarketIncomeBooks` (about 2,592 bytes).

**Accrual** (`accrue_side`, lending/mod.rs:1199): split the accrued interest
with the existing fee split (operator_revenue.rs:37-69): protocol into `Pu`;
operator into `operator_claimable` and `Q`; LP share into `Q` and the lane
indexes (junior `junior_income_share_bps`, senior pro rata across ordinary srLP
and the two hLP vaults). With no live junior, the junior lane credits the seed,
as today. `bounded_accrual_index` keeps only its u64 debt-domain clip; the book
capacity check and `check_borrow_income_capacity` go away, so interest is never
forgone for bookkeeping reasons.

**Repayment** (repay, add_leverage_margin, leverage settlement and close,
liquidation, flash and emergency liquidation): interest cash stays in the
reserve vault. The protocol's pro-rata share of the interest paid is moved to
its revenue liabilities as today (`credit_protocol_interest_revenue`). No
transfer into a separate income custody account, so the internal-move
Token-2022 gross-up disappears. A borrower's direct repayment still pays its
own transfer fee.

**Losses**: interest written off on default reduces `P` by the LP and operator
part and enters `absorb_funded_credit_loss` with the principal loss. The
protocol part is dropped from `Pu`.

**Harvest**: checkpoint the holder against the lane index (`YieldAccount::accrue`
already does this for the growth indexes), then pay interest from the reserve
vault, limited by available cash. Swap fees keep their
current funded index and vault. The transfer hook keeps checkpointing both
parties.

**Junior**: conversion, exit escrow and retired-burn checkpoint the junior index
instead of book claims. A wipe freezes the generation's final index in its
cohort, as cohorts already do for growth indexes; retired holders harvest from
the frozen index. `IncomeExposureEnd` and `junior_end` go away.

**hLP**: each vault's public-interest credit nets against the funding it owes
on the opposite asset, then publishes the remainder to hLP holders
(`settle_new_hlp_public_interest` already does this with book claims). hLP NAV
counts the vault's credited, unclaimed interest instead of `hlp_unpaid_income`.
Receivable conversion on exit (`capitalize_hlp_receivable`,
`quote_hlp_receivable_conversion`) goes away; exits are paid from NAV within
cash limits.

**Deleted**:
- `IncomeReceivableBook`, `ReceivableGrowth`, `IncomeEpoch`, the `income` field
  and `income_initialized` on `YieldAccount` (225 bytes per account)
- transitions/tranches/income_receivable.rs, lazy_income.rs, and most of
  market_income.rs (issue, sync, clear, collect, pending, allocations, holder
  checkpoint, exposure ends)
- `roll_income_epoch`, book catch-up in `checkpoint_income`, `fund_interest`,
  and `public_interest_advance` (advance, revenue escrow, dust floor)
- `IncomeEpochNeedsRollover`, `IncomeHistoryRequired`
- the per-book `InterestRevenueBook`, reduced to `Pu` per asset

## Layout and deploy

Breaking for every consumer: Market, YieldAccount and the instruction set
change. Mainnet has one live market (2026-11-15). It is wound down before the
upgrade instead of migrated: all liquidity removed, no open borrows or
leverage, and every yield, operator and protocol balance claimed. The upgraded
program cannot read the old layout, so anything left in that market's vaults
would be stuck. Devnet follows the clean-cut rule (drain or redeploy, no
migration). Market shrinks by roughly
2.4 KB, which returns emergency-liquidation heap margin (82 bytes today). The
IDL, fixtures, SDK and webapp are re-pinned together.

## SDK and webapp

- SDK: delete income-receivable.ts and the roll and prepare helpers in
  tranche-client.ts (`rollIncome`, `checkpointIncome` book catch-up,
  `prepareHolderIncome`, `prepareDrainedIncomeBooks`) and
  scripts/check-income-receivable.js. Claimable interest is computed from the
  lane index and the holder checkpoint.
- Webapp: delete `planDuskIncomePreparation`, its roll and checkpoint builders,
  and the IncomeEpoch rent line; claimable interest in yield-accounting.ts comes
  from the index alone. Rewrite tranche-accounting.mdx, keepers.mdx and the
  reference notes that describe rolls.
- Indexer APY reads `interestIndexDeltaQ64`. That index barely moves while the
  books are live, so srLP interest APY should read near zero today; it moves
  again under this plan. Not verified: the indexer was not read.

## Tests

Rewrite or delete the book suites (tranches/income_receivable, lazy_income,
interest_advance) and update lending_interest, hlp_funding,
liquidity_hlp_interest, liquidity_hlp_engine, liquidity_hlp_flash,
liquidation_economics, tranches/market, tranche_capital, junior_exit,
operator_revenue, transfer_hook, state/yield_account, benchmark_api,
tests/v2-final-smoke.test.ts and tests/utils/instruction-coverage.ts.

New properties, each as a proptest over random accrue, repay, claim, transfer,
convert, liquidate and default sequences:

- the ledger identity `C + D + I = P + Q + Pu` holds after every step
- accrual, repayment and claims never change `curve_reserve`
- total claims paid never exceed credited interest
- a holder's interest equals balance × index change over its holding periods,
  across transfers and conversions
- a default reduces junior before senior, by exactly the LP and operator share
  written off

Then the default-heap emergency-liquidation test, LiteSVM, and a Nemesis pass on
the new ledger.

## Locked defaults

These follow from the decisions above and keep today's behavior where it is
not part of the change:

- A claim pays what cash allows; the rest stays claimable. Claimable interest
  can exceed what is withdrawable right now, and the webapp shows both.
- Claims arrive net of the asset's Token-2022 transfer fee, as harvest does
  today. Direct repayments still pay their own fee.
- hLP funding interest is credited to ordinary srLP and junior only; vault-held
  srLP is excluded from that index's denominator, as today.
- Each hLP vault's public-interest credit first nets against the funding it
  owes; the remainder is credited to that vault's hLP holders. hLP NAV holds no
  unpaid coupon, so exits need no receivable conversion.
- Junior interest credited before a wipe stays claimable from the retired
  generation's frozen index.
- With no live junior, junior-lane interest credits the permanent seed, as
  today.
- `checkpoint_income` is deleted: a claimable balance is computed from the
  index without a catch-up. The transfer hook, deposits, withdrawals,
  conversions and harvest still checkpoint.
- `fund_interest` and the public interest advance are deleted; the SDK harvest
  builder stops prepending `fund_interest`.
- The protocol's repaid share keeps its current destination (referral, fee and
  buyback liabilities, then `settle_protocol_auction`).
- The one mainnet market is wound down before the upgrade, not migrated;
  devnet is a clean cut.

## Order of work

Everything lands on one branch off d8d3bff3 in a single pass, then it is
verified once:

1. Ledger, accrual, pricing and claims for srLP and junior; delete the books.
2. Repayment, protocol share and losses across every settlement path.
3. hLP netting, NAV and exits.
4. Junior conversion, wipe, cohorts and seed.
5. IDL, fixtures, SDK, LiteSVM.
6. Webapp and docs.
7. Full test suites, default-heap emergency liquidation, then a Nemesis audit.

## Open

None. Every decision is locked (2026-11-15).
