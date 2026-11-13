# LP interest accounting

Implemented in PR #45. The initial accounting work began at
`5c0aca7fcb582418f1c5b99061d4ff7605b6f185`; later hLP recovery changes
are also integrated into that PR.

## Selected behavior (2026-10-18)

- Keep V1-style ownership: accrued public borrower interest is included in LP
  share value. Entrants pay for their share of the existing receivable.
- Apply the same economic backing to srLP mint/burn and hLP share valuation.
- An exiting holder receives the permitted cash withdrawal and retains no
  separate unpaid-interest entitlement. Existing funded yield is distinct.
- Repayment converts a receivable into cash; it must not count the same income
  twice. Cancellation removes the receivable from current share value.
- Cash availability and backing checks remain binding. Notional reserves do
  not authorize spending tokens the reserve vault does not hold.
- Preserve the existing swap-fee split: compounded fees enter share principal;
  separate swap fees retain their funded per-holder index.
- Keep the current AMM pricing model: public unpaid interest remains excluded
  from `curve_reserve`. The later V1-style swap-pricing proposal was declined.

The previously proposed historical claim ledger and proportional funding
policy are superseded. They are not part of this implementation.

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
curve[i]   = live[i] - unpaid_public_interest[i]
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
When no active hLP owns unpaid interest, the quote retains its analytic path.

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

## Vault-wide harvesting and flash recovery

`fund_interest` funds accrued public interest from available cash and settles
new hLP income at vault scope. It checkpoints historical holder growth first,
uses only new income to cover each vault's funding gap, jointly restores the
opposite-token hedge, and publishes the remaining new yield. Existing funded
claims cannot be spent on recovery. New income in the borrowed asset is used
first, then new target-asset income; transfer fees are charged to that vault.
Both vaults use one frozen pre-rebalance mark for the ownership allocation.

Funding alone leaves swap reserves unchanged. The user approved the existing
hedge reconstruction changing the AMM mark within the harvest transaction.
The hedge is vault-wide, including under concentration; its target is opposite-
token neutrality, not a fixed 2x gross leverage ratio.

`HlpInterestAllocated` reports new published yield, custody debits retained as
hLP capital, and measured reserve credits. `PublicInterestFunded` reports the
cash advance and allocations. Repayment uses the same new-income settlement.
Gross `BorrowInterestPaid` credits minus capital debits reconcile net interest
custody; these gross credit events must not be interpreted as net vault growth.

The SDK exposes `dusk.yield.fundInstruction`, `previewFunding`, and
`buildHarvest`. The latter composes funding with an existing holder harvest;
`fundAccruedInterest: false` collects already-funded claims independently.
Funding checks reserve cash, current transfer fees and the complete hedge
settlement. A simulation is not a reservation of cash or a guarantee of inclusion.

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
Funding interest is realized only to the extent that cash can pay it; the rest
is canceled when the cohort closes and does not become principal loss.
Native closure retains seized inventory in the pool; flash closure physically
releases the quoted target tokens. Historical funded claims remain untouched.
Any flash surplus becomes funded yield of the retired cohort, whose receipts
remain burnable. Closing one insolvent vault does not require repairing the
other insolvent vault first.

The SDK exposes `dusk.hlpLiquidations.preview` and `buildFlash`. Its route
callback receives the exact **net payment-vault credit** required. Incoming
transfers must gross up separately. Builders return instructions without
signing or submitting, and reject unapproved full closure. A versioned
transaction with a prepared lookup table is appropriate for composed routes;
final instruction order must stay unchanged because settle's index is bound.

Cash limits still matter: unpaid receivables are NAV, not flashable token cash.
If target cash cannot fund an external quote, the quote rejects; the native
terminal path can retain inventory and cancel unpaid interest. A preview does
not guarantee external liquidity, profitability, or successful inclusion.
The complete audit item's review status is recorded in
[the remediation ledger](audit/MODEL_REVIEW_CLOSEOUT.md).

## Finite numerical representation

Virtual reserves and intermediate indexed-debt arithmetic use u128. Physical
SPL custody and individual debt settlement amounts remain u64; signed receipt
deltas use i128 so the full unsigned token range can be reported. New borrowing
must preserve these domains and the capacity of existing income books.

At an extreme accrual boundary the market limits the shared index before
issuing any additional debt or fee/LP receivable. Excess interest is forgone,
not deferred, and the accrual slot still advances. Existing obligations are
never independently clipped. Current APR continues evolving, and future
interest can resume when all dependent books have room. Exhausted income-unit
denominations retain the existing permissionless archive/roll path after
settlement. The index resets only when every public and hLP debt share on that
asset has been extinguished; historical income claims are retained.

`roll_income_epoch` archives a book only once at least half of its units are
repaid, or once any of its counters (units, unpaid, paid per unit, or a lane's
growth) reaches half its numeric range. Holders carry their units across
archived epochs in bounded catch-up batches, so rolling a barely repaid book
would let anyone fragment that catch-up into many epochs without freeing
capacity. Rolling needs no keeper: anyone may call it, the SDK's
`prepareDrainedIncomeBooks` rolls fully drained books as market maintenance,
and holders run `prepareHolderIncome` before a transfer, exit or claim.

## Interest fees and cash advances

Base borrower interest is split between LPs and the operator. The operator's
share is 0–50%; it receives no swap revenue. The global protocol interest fee
is a markup added on top of base interest, controlled by FutarchyAuthority.
With $100 base interest, a 30% operator share and 20% protocol markup,
borrowers owe $120: $70 LP income, $30 operator revenue and $20 protocol revenue.

Rates are frozen as interest accrues. Each market checkpoints elapsed debt at
its cached markup, then adopts the latest global markup for future accrual.
An operator-share edit also accrues the previous interval before installing
its new share. No later rate change re-prices the earlier fee book. Repayments
and defaults are pooled within each asset/source receivable book.

Operator entitlement follows collection: the recipient at payment receives
the fee, including payment of previously accrued interest. On a handoff,
already collected whole-atom cash is sealed for the outgoing recipient;
unpaid fee amounts and sub-atom rounding carry follow the new recipient. Claims
are backed by measured interest-vault cash and pay the recipient's canonical
ATA. Claims never call an external governance program.

Each asset tracks gross advanced interest and aggregate protocol/operator
revenue escrow. Only LP income is advanced into interest custody; fees remain
unpaid until borrower cash arrives. Frozen fee escrow stays in reserve custody,
excluded from executable cash and LP value. Repayment recycles the advance;
only fresh interest and released fee escrow transfer to interest custody.
Measured Token-2022 credit is divided between the LP and fee books. Referral
revenue comes only from the collected protocol allocation.

Let `U` be gross public unpaid interest, `A` the outstanding gross advance,
`R` the live reserve, and `C` available cash. Funding `a` records:

```text
A' = A + a
R' = R - a
C' = C - a
curve' = R' - (U - A') = R - (U - A)
```

Thus funding alone does not change executable curve reserves. Borrower debt
is unchanged. A clearance removes its proportional advance and frozen fee;
the final clearance drains integer remainders. Paid advances replenish cash.
Canceled advances return the held fee portion to cash. The LP portion
already paid is a real loss if the borrower defaults; it is not paid again,
and junior-first principal loss allocation does not cover canceled interest.

The repayment kernel applies the debit and recycled credit together. A
valid internal repayment must not fail because it tries to collect cash for
already-funded interest a second time. The same reconciliation is wired into
ordinary borrowing, leverage repayment/close, external flash recovery and
native liquidation accounting.

SDK functions `quoteInterestAdvanceAmounts` and `allocateRepaidInterest`
provide integer accounting amounts, including the effective Token-2022 fee
and the historical fee-book split. Use the simulated `previewFunding` for hLP feasibility and `buildHarvest`
for transaction composition.

### Historical gross-income rehedge diagnostic

A native diagnostic funds interest, publishes its gross hLP-owned LP share,
and applies the existing full-value hedge reconstruction. This is a tested
comparison, **not the proposed net-yield harvest implementation**. Funding
leaves the raw curve and quoted price unchanged. The subsequent existing
hedge reconstruction can change the quoted price.

The fixture starts with 1,000,000 tokens per ordinary reserve, deposits
200,000 tokens into each hLP, and has 400,000 USDC of public principal. With
4x concentration and a 20% protocol interest share:

| Accrued USDC interest | Price before funding/rehedging | Price after | Change |
| ---: | ---: | ---: | ---: |
| 25,000 | 0.999984250 | 0.999977720 | -0.000653% |
| 250,000 | 0.998593574 | 0.997806262 | -0.078842% |
| 800,000 | 0.988868015 | 0.971366965 | -1.769806% |

The two larger cases deliberately stress large unpaid balances; they are
not yield forecasts. The diagnostic excludes token transfer fees and new
hLP funding interest. It verifies that rehedging preserves each hLP's
remaining NAV at the fixed pre-rehedge mark within token rounding. The
changed AMM mark is a separate effect. The user subsequently accepted hedge
rebalancing moving the mark. Historical funded claims are not used here.

The implemented net-yield checkpoint differs from this gross-income diagnostic:
it restores funding using only newly earned income before publishing the rest.
The integration test covers both hLPs and srLP, SPL Token and Token-2022, both
interest assets, 1x/4x concentration, a 20%-to-60% protocol rate change, actual
cash-funded harvests and repayment conservation including withheld transfer fees.
The native allocation matrix covers 108 cases, including 1x/4x/10x concentration,
three unpaid-interest amounts, funding accrual and capped/uncapped transfer fees.
It checks each vault's value at the common mark and preserves historical claims.

### Validation of the advance accounting

The complete required local CI sequence passed: formatting, hygiene, helper
shape, Clippy, 518 Dusk tests in both default and production profiles, the
production check, the EMA settlement invariant, delegate/faucet tests,
TypeScript, Dusk/delegate/faucet SBF builds, build identity, regenerated IDLs,
and 74 SDK tests. Strict LiteSVM passed 136 tests, exercised all 71 instructions
and all 18 required compute scenarios, with no pending tests. Existing CU
ceilings were retained. Ordinary swaps measured 98,640 CU against the unchanged
strict 100,000 limit; the concentrated public-interest fixture reached 883,010 CU.

At the previous advance-ledger checkpoint, the instruction regression seeded
a funded accounting/custody state, then ran
real repayment transfers with both hLPs at 1x and 4x concentration and both debt
assets. It freezes 20% on the funded half, changes the current rate to 60%, and
verifies custody, exact fresh/frozen protocol allocation, LP growth without a
second payout, and full ledger clearance. The new integration replaces this seeded advance with real funding and harvest
instructions. Native tests cover zero-starting-cash internal
repayment, concentrated emergency cancellation, and both hLPs through repayment
and writeoffs. SDK tests cover effective Token-2022 transfer fees, mixed frozen
and fresh net-credit allocation, cash caps and u64 precision. The new instruction fixture also covers fee-bearing Token-2022 advances,
harvests and repayments.

Market layout version 4 includes the two per-asset advance ledgers, temporary
flash receivables, and per-vault recovery locks; all four SDK
IDL/type files are regenerated with the source change. This is a development
interface change, not a deployed migration.

## Harvest and flash validation (2026-10-20)

The implementation passes 523 native Dusk tests in each default/production
profile, including a 108-case harvest conservation matrix. That matrix covers
both interest assets, 1×/4×/10× concentration, funding gaps, and capped/uncapped
transfer fees. It checks ordinary srLP value and both hLPs at a common mark,
including newly published yield and retained capital. Historical funded claims
are preserved. Existing repayment/writeoff tests exercise the shared advance
ledger and its frozen protocol allocation.

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
