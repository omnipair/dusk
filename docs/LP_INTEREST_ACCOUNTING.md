# LP interest accounting

Implemented in PR #45. The initial accounting work began at
`5c0aca7fcb582418f1c5b99061d4ff7605b6f185`; later hLP recovery changes
are also integrated into that PR.

## Accrued-interest ledger (2026-11-15)

Borrower interest is credited to holders when it accrues and claims are paid
in reserve cash. The income receivable books, their epochs and rolls, the
public interest advance, `fund_interest`, `checkpoint_income`,
`roll_income_epoch` and the per-asset interest vaults are removed (market
layout 19).

Per asset, with `C` reserve cash plus the flash receivable, `D` borrower
principal (public, leverage and hLP funding), `I` recognized borrower
interest, `Q` credited and unclaimed interest, `Pu` the protocol share of `I`
not yet repaid and `P` LP principal:

```text
C + D + I + Δ = P + Q + Pu
curve_reserve = max(0, live_reserve + funding_recognized + deficit − credited − protocol_unpaid)
```

`Δ` is the asset's recorded interest deficit (below); it is zero in a
market whose LP capital has always covered its losses.

`I` is `public_recognized + funding_recognized` in the side's
`InterestLedger`. Every instruction ends with both sources recognized: the
recorded values equal the debt they measure.

| Event | Change | Effect on `P` |
|---|---|---|
| Accrual of `i` (LP and operator `a`, protocol `p`) | `I += i`, `Q += a`, `Pu += p` | none |
| Repayment of interest `x` (protocol part `x_p`) | `C += x − x_p`, `I −= x`, `Pu −= x_p` | none |
| hLP funding payment `x` | `I −= x`, cash leaves only for `x_p` | `−x` (the vault spends principal it owns) |
| Claim of `y` | `C −= y`, `Q −= y` | none |
| Write-off of interest `w` (protocol part `w_p`) | `I −= w`, `Pu −= w_p` | `−(w − w_p)`, junior first |

Accrual, repayment, netting and claims leave `curve_reserve` and the price
unchanged. Depth absorbs claims and repayments.

- **Accrual split.** The protocol markup is added on top of base interest:
  protocol `= gross × m / (10,000 + m)`. The operator receives its share of
  the rest; the LP share goes to the junior lane (or the permanent seed while
  junior is wiped), then senior. Senior public interest is split pro rata
  across ordinary srLP and the srLP held by the two hLP vaults; hLP funding
  interest credits ordinary srLP and junior only. Sub-atom splits carry in
  Q64 fractions, so the running protocol and operator totals stay within one
  atom of their exact share.
- **Netting.** At accrual, the vault that borrows an asset first nets its new
  public credit in that asset against the funding interest it owes there, by
  burning its debt shares; the remainder is credited to its holders. Netting
  moves neither the vault's principal nor its recorded live backing, so the
  price does not move; it is skipped when cash cannot cover its protocol
  share. Only accrual nets, so a prepared hLP plan never sees its vault debt
  change between preparation and consumption.
- **Protocol share.** The protocol is paid only. Each interest payment moves
  `floor(paid × Pu / I)` from cash to `protocol_pending`, which is booked to
  the referral, fee and buyback liabilities by the instruction (referral
  first). A write-off drops `ceil(w × Pu / I)`; removing every outstanding
  atom drains `Pu` exactly. A spot swap pays the protocol share of funding as
  cash allows; leverage, liquidation and hLP paths require it.
- **Operator share.** Credited at accrual into `operator_claimable` and paid
  in cash like LP interest. A recipient handoff seals the outgoing
  recipient's credited balance in its `OperatorRevenueAccount`; it stays in
  `credited` until claimed.
- **Defaults.** Liquidations pay principal first; unpaid interest is then
  written off. A short recovery pays no interest and the protocol nothing.
  Written-off interest that was credited is an ordinary pool loss, absorbed
  by junior capital before senior. Credited interest, including the
  operator's, never absorbs losses.
- **Claims.** Harvest checkpoints the holder against the lane index and pays
  from reserve cash, limited by the cash present; the rest stays claimable.
  Claims arrive net of the asset's Token-2022 transfer fee. Direct borrower
  repayments pay their own transfer fee. No other interest movement is a
  token transfer.
- **Junior.** Conversion, exit escrow and retired burns checkpoint the junior
  index. A wipe freezes the generation's final index in its cohort, and
  retired holders harvest credited interest from it.
- **hLP NAV.** A vault's NAV is its srLP share of `curve_reserve` plus the
  opposite-asset debt it holds; interest credited to the vault's holders is
  outside NAV, so exits need no coupon conversion.
- **Frozen hLP vaults.** Creditors come before equity. A frozen (insolvent)
  vault still nets same-asset interest against the funding it owes, but the
  rest of its public interest is held on the vault
  (`held_base_interest`, `held_quote_interest`) instead of reaching its
  holders. Held interest stays in `credited`, so accrual moves neither the
  curve nor the price. The terminal close, native or flash, applies it to
  the debt the sale or payment leaves unrecovered, borrowed-asset interest
  first and target-asset interest at the terminal EMA mark: the applied part
  moves from `credited` into LP principal and the vault's unrecovered debt
  falls by exactly its value. Held interest never creates a surplus; what
  the debt does not need is published to the retired cohort in the asset it
  was held in. A recovery that unfreezes the vault publishes the held
  interest to its holders in full. Interest published before the freeze
  stays theirs.

**Interest deficit.** Credited interest can exceed all LP capital in an
asset, so a write-off can be larger than `P`. The liquidation still goes
through: LP capital goes to zero, junior then senior, and the part of the
loss capital cannot absorb is recorded as `InterestLedger::deficit` (u128,
per asset, never decreases). `absorb_funded_credit_loss` records it before
allocating the rest to capital, and every instruction ends with no
unrecorded gap. Credited claims are not reduced: they are paid from the cash
that remains, first come and limited by cash, and the checked subtraction on
`credited` keeps every payout within what was credited. Once claims exceed
what the pool can ever pay, late claimers receive nothing. While either asset
has a deficit, `add_liquidity`, `deposit_single_sided` and `borrow` refuse
with `InterestDeficit`, because new capital would fund old claims and no
price can move to value new credit. The wiped asset has no curve, so the
market never trades again: spot and leverage swaps, and liquidations and
leverage closes that sell through the curve, refuse with
`InsufficientLiquidity` instead of pricing against an empty side, where any
input would buy the other side's whole reserve. Every other curve reader
(`update_at_slot`, the leverage-margin prelude, the borrow-protection
snapshot, risk refresh and the transition finalizers) treats the market like
an empty one (`Market::curve_wiped`): it skips the curve and hLP price
checkpoints and keeps risk, prices and the valuation curve at their last
observation. Price-free exits therefore keep working: harvest and the
operator, referral and protocol claims (first come, from remaining cash),
repayment, collateral deposits, withdrawals by debt-free positions (repay
first otherwise), srLP exits (the healthy asset's principal, zero of the
wiped one's), flash liquidation at the last observation, and burning a
terminally closed vault's hLP receipts. A position that still owes either
asset cannot withdraw collateral against the frozen price, and an hLP exit,
which needs a curve price, refuses with `InterestDeficit`; a vault the
terminal close would accept (EMA collateral no longer covering its funding
debt) is frozen at that mark, so its new interest is held for its creditors
and the terminal close is its holders' exit. With a deficit, the terminal
close (native or flash) also accepts a solvent vault (product decision
2026-11-17): its debt is settled at the close's EMA price, held interest
applies as above, and recovery above the debt is the holders' remainder.
The native close returns it in kind (the borrowed claim first, then the
target claim at the sale's average price) after it pays the caller bounty;
the flash close credits the buyer's payment above the debt. A solvent
wind-down absorbs no loss and never raises a deficit. The native close
settles a borrowed-asset debt with target inventory the pool keeps, which
on a wiped borrowed asset would grow its deficit, so when the vault's
borrowed asset has a deficit the native close refuses with
`InterestDeficit` (solvent or insolvent vault) and the vault closes by
flash, where the buyer pays the borrowed asset. Holders then burn their
receipts as for an insolvent vault.

Dated sections below record earlier validation. Where they describe
receivables, coupons in hLP NAV or interest custody, the ledger above
supersedes them.

## Terminal hLP receipt cohorts

A terminal hLP close can leave old receipts in wallets after the vault's srLP
backing and funding debt have reached zero. Starting a new deposit against the
same supply would give old receipts a claim on the new deposit. Dusk instead
rotates that side to a fresh hLP mint. The old mint's final yield indexes are
frozen in its cohort PDA, so old receipts retain funded yield already earned
without sharing in later deposits. Holders may transfer, harvest and burn old
receipts; the new mint has a separate vault, supply and yield-account namespace.
Rotation is permissionless only after terminal closure and does not alter the
other hLP side or the srLP mint.

## Full-value controller accounting (2026-10-19)

The accounting implementation uses live-reserve claims consistently for hLP deposit
funding, entry admission, NAV, hedge reconstruction and recovery quotes. The
opposite-token funding leg matches the ratio used to mint the underlying srLP.
Existing funded holder claims remain separate from this NAV.

Trading still uses the ordinary curve coordinates, with public unpaid interest
excluded. The transient integrated state carries separate book-value offsets:

```text
book_base  = ordinary_curve_base  + base_book_offset
book_quote = ordinary_curve_quote + quote_book_offset

live_base  = book_base  + base_hLP_equity  + quote_hLP_debt
live_quote = book_quote + quote_hLP_equity + base_hLP_debt
curve[i]   = live[i] - curve_exclusion[i]
curve_exclusion = credited + protocol_unpaid - funding_recognized
```

The book offsets provide cash-flow coordinates during one quoted trade.
Trader cash changes the curve and book ledgers by the same amount. Ownership
and opposite-asset debt are reconstructed from full book backing; public
receivables remain excluded from pricing principal.

The price calculation must include that reconstructed ownership. Freezing an
old curve point and repricing only after settlement was insufficient: a
fee-free 10,000-token round trip could gain 0.116057 tokens initially and
0.001803 on a repeated cycle in one test. The corrected quote starts from the
canonical hedged point and certifies the completed endpoint against the same
curve. A few residual-based candidates are followed, when needed, by bounded
bisection; every accepted candidate independently satisfies the invariant.
Under the accrued-interest ledger no hLP NAV holds an unpaid coupon, so the
certified search runs only for carried funding or frozen vaults.

The regression runs three successive round trips across both directions,
both interest assets, 1x/4x concentration, 20k/200k hLP deposits, 25k/250k
receivables and 1k/10k/100k trades. It requires no positive trader return,
including with zero swap fees. Separate tests verify full NAV, cash
conservation, repayment/writeoff accounting and the quoted final price after
compounding and recovery incentives.

The reserve/debt identity retains its existing atom-sized reconciliation
bound. Final cache, price and volatility observations come from the
atom-rounded settlement plan. Owner leverage closes use the measured-input
settlement's existing debt-coverage and payout checks; only delegated closes
need the additional pre-callback payout quote.

Opposite-asset residuals are valued at the actual concentrated curve mark,
not the raw reserve ratio. The extreme funding regression verifies economic
NAV before and after rehedging, in addition to checking that the operation
executes. Cash must cover the actual funding payment.

Native coverage includes both interest assets and both hLP targets at 1x and
4x concentration; swaps, cash withdrawals, public debt repayment, principal
writeoff, canceled interest, current-holder yield and recovery from negative
curve-only equity with positive full NAV. Interest accrual alone leaves the
trading price unchanged. Repayment removes the receivable and funds yield
once; writeoff reduces current LP value rather than creating yield.

The SDK `get.previewHlpDepositCapacity` helper uses the native full-value
admission and funding ceiling. `Ready` is a current funding/hedge status, not a
promise that every smaller deposit passes amount-specific rounding checks.

## hLP funding and pricing inventory

The ordinary pricing point is reconstructed from recorded hLP reserve backing,
not from the indexed funding liability. Accrued funding changes neither cash
nor those recorded reserve legs. Idle accrual therefore leaves the pricing
point and its cached geometry unchanged, including outside concentrated bands.

The full indexed liability remains in NAV, solvency, funding-income and hedge
calculations. The integrated settlement values each vault's funding residual
at the funded curve mark and uses separate ownership offsets to reconcile the
entire liability with actual inventory. Accrued interest is not additional
principal or executable liquidity. Frozen vaults keep their liabilities.

Payment settles the liability already charged to the paying vault's NAV at
accrual. The paid funding stays in reserve cash; only its protocol share
leaves cash, so a spot swap carries funding only when cash cannot cover its
output plus that share. Persistent, signed hLP
funding offsets bridge the resulting ownership books to the unchanged pricing
point. They are virtual trading coordinates, never redeemable assets or cash.
Both vaults settle through one endpoint, preserve their full pre-payment NAV
at the same mark within raw-atom rounding, and retain their complete hedge.
Funding accrual and payment alone leave both pricing coordinates and the
cached concentration geometry unchanged. Ordinary LP mint/burn scales this
separation with ordinary ownership; an hLP close does not discard it. Cash
availability, full indexed debt, and principal-loss accounting remain binding.

Regressions cover exact payment-only point/cache/price equality on both tails,
both vaults settling once with unchanged full NAV, partial and full hLP exits,
ordinary LP mint/burn with active and closed vaults, and repeated fee-free
round trips. Idle update observations equal the first swap's starting price.
Native and flash terminal closes share the same funding-only pricing
adjustment; collateral recovery and principal losses still change inventory.

## Validation and compute cost

The complete local CI sequence passes for this accounting correction: 513
Dusk tests in each default/production profile, the EMA settlement invariant,
delegate/faucet tests, formatting, hygiene, code shape, clippy, TypeScript,
SBF builds, build identity, IDL freshness, and 70 SDK checks. The strict LiteSVM
suite passes 136 tests, exercises 71/71 instructions and checks all 18 required
compute scenarios. Generated SDK interfaces are unchanged by this correction.

The new instruction regression opens real public debt in each asset, applies
12.5% index accrual (including hLP funding), settles funding once, then checks
previews and reverse trades with both hLPs at 1x/4x concentration. This isolates
repeatable pricing credit from the existing one-time funding-recovery reward.
The native tests separately check the first trade after public accrual with
no funding premium, plus repeated cycles and larger receivables.

Full-value settlement has a compute cost. The measured concentrated hLP
funding case increases from 355,721 to 835,599 CU; the new concentrated public
interest case peaks at 881,839 CU. An ordinary concentrated swap falls from
266,593 to 188,344 CU after reusing prepared curve boundaries. The plain
same-slot swap is 97,627 CU, below the unchanged 100,000-CU architectural
limit. The 1,355,000-CU transaction guard is also unchanged. Per-scenario
ceilings are the successful finished-binary measurements plus exactly 5%.
These fixture measurements do not establish that every composition fits;
external hLP flash execution needs its own transaction budget tests.

## Harvesting and flash recovery

Interest is already credited when a holder harvests: the holder is
checkpointed against its lane index and paid from reserve cash as cash
allows. hLP vault interest is published to the vault's holder index at
accrual, after netting against the funding the vault owes. Swap fees keep
their funded index and custody. `BorrowInterestAccrued` reports the protocol,
operator and LP credit and the amount netted; `BorrowInterestPaid` reports
the interest paid and its protocol share.

The hLP flash interface adds `preview_hlp_flash`, `begin_hlp_flash`, and
`settle_hlp_flash`. Recovery applies to an entire target-token vault. A holder
cannot recover only their own receipt's hedge or assign the cost to new holders.

1. **Quote:** a solvent vault can be quoted when indexed funding debt exceeds
   its opposite-token claim, or when its hedge is frozen. There is no 9/8
   threshold for external recovery. The solvent quote supplies the debt-token
   funding gap and returns enough
   target tokens at the symmetric EMA to pay an affordable discount, up to the
   existing 5% ceiling. Both token-transfer fees are included. No new timer is
   introduced. The entire remaining vault must retain positive full-value NAV.
2. **Begin:** the program freezes the quote, buyer, market, slot and exact future
   settlement instruction. Target reserve cash leaves; a matching temporary
   receivable preserves backing and is never spendable cash. The affected
   vault cannot mint or redeem hLP while its recovery is outstanding.
3. **Route:** the buyer may supply their own inventory or sell the collateral
   through another venue or Dusk. Swaps and independent market actions compose
   with the actual state. An active flash cohort does not also pay a native swap
   recovery bonus. There is no state snapshot restored over intervening trades.
4. **Settle:** only new, measured credit in a dedicated session payment vault
   meets the obligation. Existing donations and credits sent directly to the
   market reserve do not qualify. The program restores the opposite-token
   hedge, settles funding and srLP changes, refunds unused new payment, and
   closes the session. A missing or failing settle reverts the entire transaction.

When current-slot symmetric-EMA collateral value is no greater than debt, the
quote instead offers the entire target-token claim. The full path requires
explicit keeper opt-in. The flash quote freezes the target claim, borrowed-token
claim, and fixed payment. Terminal settlement revalidates EMA insolvency, while
same-market swaps may compose without repricing the session's loss allocation.
The full transaction still reverts if any bound payment, cash or
reserve invariant fails.

Both native terminal closure and flash terminal closure allocate the remaining
funded principal loss to junior first, then senior. There is no outside coverage
vault or draw budget. The caller can cap the total socialized principal loss.
Recovery pays funding principal first; funding interest the recovery cannot
pay is written off. That interest was credited at accrual, so its LP and
operator part is a pool loss absorbed with the principal loss, junior first.
Native closure retains seized inventory in the pool; flash closure physically
releases the quoted target tokens. Credited claims remain untouched.
Any flash surplus is credited to the retired cohort's holder index, whose
receipts remain burnable. Closing one insolvent vault does not require repairing the
other insolvent vault first.

The SDK exposes `dusk.hlpLiquidations.preview` and `buildFlash`. Its route
callback receives the exact **net payment-vault credit** required. Incoming
transfers must gross up separately. Builders return instructions without
signing or submitting, and reject unapproved full closure. A versioned
transaction with a prepared lookup table is appropriate for composed routes;
final instruction order must stay unchanged because settle's index is bound.

Cash limits still matter. If target cash cannot fund an external quote, the
quote rejects; the native terminal path can retain inventory and write off
unpaid funding interest. A preview does
not guarantee external liquidity, profitability, or successful inclusion.
The complete audit item's review status is recorded in
[the remediation ledger](audit/MODEL_REVIEW_CLOSEOUT.md).

## Finite numerical representation

Virtual reserves and intermediate indexed-debt arithmetic use u128. Physical
SPL custody and individual debt settlement amounts remain u64; signed receipt
deltas use i128 so the full unsigned token range can be reported.

Accrual keeps one numeric clip: the shared borrow index stops where a debt
bucket would leave the u64 domain, and the accrual slot still advances.
Interest is never forgone for bookkeeping reasons. Under that clip
`I < 2^66` and `Q < I + 2^64`, so the u128 ledger cannot overflow and a spot
swap never fails in recognition, netting or funding settlement. Growth
indexes overflow only above 2^64 atoms per share.

Rounding: the protocol share of a payment rounds down and of a write-off
rounds up; Q64 carries keep the accrual split and lane distributions exact
over time; a share burn that moves a phantom atom between principal and
interest is recognized like any accrued atom. `credited` exceeds the sum of
holder entitlements only by sub-atom index carries.

## Interest fees

Base borrower interest is split between LPs and the operator. The operator's
share is 0–100% of base interest; it receives no swap revenue. The global
protocol interest fee is a markup added on top of base interest, controlled
by FutarchyAuthority. With $100 base interest, a 30% operator share and 20%
protocol markup, borrowers owe $120: $70 LP interest, $30 operator interest
and $20 protocol revenue.

Rates are frozen as interest accrues. Each market checkpoints elapsed debt at
its cached markup, then adopts the latest global markup for future accrual.
An operator-share edit also accrues the previous interval before installing
its new share.

The LP and operator shares are credited at accrual. The protocol share is
paid only, from the interest borrowers actually repay; referral revenue comes
from that paid protocol share. An operator share credited before a default is
covered by junior, then senior, because the pool fronted it.

## Harvest and flash validation (2026-10-20)

The implementation passes 523 native Dusk tests in each default/production
profile, including a 108-case harvest conservation matrix. That matrix covers
both interest assets, 1×/4×/10× concentration, funding gaps, and capped/uncapped
transfer fees. It checks ordinary srLP value and both hLPs at a common mark,
including newly published yield and retained capital. Historical funded claims
are preserved.

The SDK has 77 passing checks. The strict LiteSVM suite has 140 passing tests,
76/76 instructions exercised and all 19 required compute scenarios. Its flash
matrix simulates 24 combinations of target direction, 1×/4× concentration,
SPL/Token-2022 assets and inventory/AMM/terminal routes. Twenty combinations
execute. The four direct Dusk routes that combine 4× concentration with both
active hLP vaults are retained as explicit compute-feasibility rejections; the
ordinary swap remains the native Dusk recovery path. The matrix checks actual
SDK simulation and execution, preview parity, packet size with a real lookup
table, missing-settlement and underpayment rollback, full-close opt-in and
exclusion of pre-begin payment-vault donations. Native tests also exercise two
independent flash sessions with a swap between begin and settle and
principal-loss coverage.

The strict full-suite hLP flash maximum observed before commit is 1,333,784 CU.
The 1,355,000-CU guard and existing swap ceilings were not raised. This composed
route has limited extra compute capacity: larger real routes still require
simulation and can exceed the transaction budget. These checks validate the
covered execution paths; they do not replace the final full-PR audit.
