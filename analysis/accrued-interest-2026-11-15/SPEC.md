# Accrued-interest index: implementation spec

Status: spec, 2026-11-15. Base `refactor/accrued-interest-index` at d8d3bff3.
Implements [PLAN.md](PLAN.md); every PLAN decision is taken as given. Paths
are relative to `programs/dusk/src` unless they start with `packages/`,
`tests/`, `scripts/` or `docs/`. Line numbers are at d8d3bff3.

Two facts about today's code shape the spec:

- Junior is initialized by the first funded `add_liquidity`
  (transitions/liquidity/mod.rs:175-176). Every `fees.junior.initialized ==
  false` interest branch (non-tranche funding index, `take_new_interest`,
  `receivable_hedge == false` pricing) is therefore reachable only in unit
  tests. This spec deletes those branches; tests that build markets without
  junior keep working because `TrancheIncome::allocate` returns the whole
  credit as senior when junior is uninitialized (tranche_income.rs:68-72).
- `Fees.interest_growth_index_q64` and the hLP srLP interest pass-through are
  inert in tranche markets: nothing in production writes
  `unallocated_interest_liability`, and the funding credit that moves the
  index runs only on the non-tranche branch. The index starts moving again
  under this spec.

## 0. Design choices

1. **One per-asset `InterestLedger`** in `Fees` holds recognized outstanding
   interest per source, Q, Pu, the operator balance, the split carries and a
   transient protocol bucket. It replaces `MarketIncomeBooks`,
   `InterestRevenueBook`, `PublicInterestAdvance` and the interest-vault
   counters.
2. **`cash_reserve` is PLAN's C.** It counts every reserve-vault atom owned by
   LP principal or by credited interest. Repaid interest stays in it; claims
   leave from it. Claims, withdrawals, swaps and borrows share it.
3. **`curve_reserve = live_reserve + funding_recognized − credited −
   protocol_unpaid`.** Funding interest stays out of `live_reserve` (the live
   identity is unchanged); the curve formula adds it back, so funding accrual
   nets to zero. A funding payment changes the curve by exactly the amount it
   does today, so the existing funding pricing offsets stay as they are.
4. **The interest vault is retired.** Holder, seed, operator and protocol
   interest custody all move to the reserve vault. No internal token transfer
   of interest remains, so no internal Token-2022 gross-up remains.
5. **hLP vaults settle public interest at accrual.** Each accrual allocates the
   vaults' senior share, nets the borrowing vault's same-asset share against
   its funding owed, and publishes the rest to the vault holder index. Vaults
   never hold unpublished interest at an instruction boundary, so the
   integrated curve state loses every coupon term and receivable conversion
   disappears (see Ambiguity 1).
6. **Protocol share leaves cash at payment.** Every interest payment (borrower
   repayment, hLP funding payment, netting) moves `floor(x · Pu / I)` from cash
   into `protocol_pending`. `book_protocol_interest` then splits it into the
   existing referral, fee and buyback liabilities.

## 1. State layout

### 1.1 New `InterestLedger` (state/market.rs, a field of `Fees`)

```rust
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct InterestLedger {
    /// U_pub as last recognized: Σ over fixed and isolated buckets of
    /// (debt − principal), clamped as in `public_unpaid_interest`.
    pub public_recognized: u128,
    /// U_f as last recognized: the opposite vault's indexed funding debt in
    /// this asset minus its `debt_principal` (includes carried funding).
    pub funding_recognized: u128,
    /// Q: credited, unclaimed interest of every holder lane, the junior seed,
    /// both hLP holder indexes, holders' checkpointed `accrued_interest_amount`,
    /// the operator, and index rounding dust. Includes `operator_claimable`.
    pub credited: u128,
    /// Pu: protocol share of recognized interest not yet paid or written off.
    pub protocol_unpaid: u128,
    /// Current operator recipient's credited, unclaimed share.
    pub operator_claimable: u128,
    /// Q64 sub-atom carries of the accrual split (as InterestRevenueBook today).
    pub protocol_fraction_q64: u64,
    pub operator_fraction_q64: u64,
    /// Protocol share already removed from `cash_reserve`, still in the
    /// reserve vault, awaiting `book_protocol_interest`.
    pub protocol_pending: u64,
}
```

Invariants (checked in `assert_market_invariants`, ledger.rs:507-559):

- `public_recognized == public_unpaid_interest(asset)` and
  `funding_recognized == hlp_funding_debt(asset) − opposite vault
  debt_principal` at every instruction boundary.
- `protocol_unpaid ≤ public_recognized + funding_recognized`, and
  `protocol_unpaid == 0` when that sum is 0.
- `operator_claimable ≤ credited`; `junior.seed_interest_amount ≤ credited`.
- `live_reserve + funding_recognized ≥ credited + protocol_unpaid` (P ≥ 0),
  enforced by `curve_reserve`'s checked subtraction.

### 1.2 `Fees` (state/market.rs:566-603)

| Field | Change |
|---|---|
| `public_interest_advance: PublicInterestAdvance` | delete |
| `interest_vault_balance: u64` | delete (vault retired) |
| `interest_liability: u64` | delete; replaced by `ledger.credited: u128` |
| `unallocated_interest_liability: u64` | delete (accrual indexes directly) |
| `revenue_interest_liability: u64` | delete (operator in ledger; protocol carry gone) |
| `hlp_funding_interest_growth_remainder_scaled: u64` | delete (one denominator, one carry) |
| `interest_growth_index_q64: u128` | repurposed: live srLP lane index, denominator ordinary srLP (§4.1) |
| `interest_growth_remainder_scaled: u64` | kept: carry for that index |
| `referral_interest_liability`, `interest_protocol_fee_liability`, `interest_buyback_fee_liability` | kept; custody moves to the reserve vault |
| `junior: TrancheIncome` | unchanged layout; `interest_growth_q64` becomes the live junior lane |
| `ledger: InterestLedger` | new, 104 bytes |

`Fees`: 671 → 703 bytes.

### 1.3 `MarketSide`, `HlpVault`, `Market`

- `MarketSide.interest_vault: Pubkey` (state/market.rs:943): delete. With the
  `Fees` growth, `MarketSide` stays 920 bytes.
- `HlpVault` (state/market.rs:619-671): delete
  `base_interest_checkpoint_q64`, `quote_interest_checkpoint_q64`,
  `base_interest_remainder_q64`, `quote_interest_remainder_q64` (the srLP
  interest pass-through; vault srLP no longer earns from the srLP index).
  Keep the holder-side `*_interest_growth_index_q64`,
  `*_interest_growth_remainder_scaled` and `unallocated_*_interest_amount`.
  392 → 344 bytes.
- `Market.income: LazyIncomeBooks` (state/market.rs:709): delete; remove its
  line from `reload_from_account_bytes` (763) and the import at line 4.
- `Market` Borsh size: 7,094 → about 4,406 bytes (−2,592 books, −96 vaults;
  sides net zero). `MARKET_LAYOUT_VERSION` 18 → 19 (constants.rs:152-153;
  rewrite the doc comment at 150-151).

### 1.4 Other accounts

| Account | Change | Size |
|---|---|---|
| `YieldAccount` (state/yield_account.rs:37-64) | delete `income_initialized`, `income`; `escrow_balance` stays | 492 → 267 |
| `IncomeReceivableClaim` (yield_account.rs:9-16) | delete type | — |
| `JuniorCohort` (state/junior_cohort.rs:9-17) | delete `income_end` (first field) | 572 → 344 |
| `OperatorRevenueAccount.claimable` (state/operator_revenue.rs:12) | `[u64; 4]` → `[u128; 2]` (per asset) | 106 → 106 |
| `IncomeEpoch` (state/income_epoch.rs) | delete account and file | — |
| `HlpCohort.final_vault` | shrinks with `HlpVault` | −48 |

### 1.5 Transient types

- `IntegratedCurveState` (transitions/liquidity/hlp/integrated.rs:58-64 and
  constructor sites): delete `hlp_unpaid`, `hlp_capitalized`,
  `base_unpaid_interest`, `quote_unpaid_interest`, `receivable_hedge`.
- `ConcentratedHlpTransition.hlp_capitalized` (engine.rs:105): delete.
- `HlpRebalanceReceipt.interest_transfer_fee` (engine.rs:51-55): delete;
  `interest_paid` stays.
- `LeverageLifecycleState` (leverage.rs:228-246): delete
  `base_interest_advance`, `quote_interest_advance`.
- `LeverageLifecycleTransition`: delete `socialized_advance_loss`,
  `phantom_unpaid_interest`; add `interest_written_off: u64`.
- Delete `InterestRevenueCredit`, `InterestAdvanceClearance`,
  `DirectRepaymentTransfer`, `HlpNewInterestSettlement`, `FundingFeePayment`.

## 2. Ledger identity and curve reserve

### 2.1 Mapping PLAN symbols to fields (per asset)

```text
C   = cash_reserve + flash_receivable
D   = Σ fixed/isolated principal + opposite vault debt_principal (= borrowed-side hlp_live)
I   = public_recognized + funding_recognized
Q   = ledger.credited
Pu  = ledger.protocol_unpaid
P   = curve_reserve
```

The live identity (ledger.rs:589-593) is unchanged:

```text
live_reserve = cash_reserve + Σ(fixed + isolated debt) + flash_receivable + hlp_live
             = C + D_public_principal + U_pub + hlp_live
```

The curve reserve replaces amm/mod.rs:1191-1197:

```text
curve_reserve(asset) = live_reserve + funding_recognized − credited − protocol_unpaid
                     = C + D + I − Q − Pu = P
```

Checked arithmetic in u128, `BrokenInvariant` if negative, then
`u64::try_from` as today. Add `Market::curve_exclusion(asset) -> i128 =
credited + protocol_unpaid − funding_recognized`; every place that subtracts
`unrealized_interest(asset)` from a live coordinate subtracts this instead.
`unrealized_interest` is deleted; `public_unpaid_interest` stays (recognition
input).

With nothing claimed, repaid or written off since accrual, `Q + Pu = I`, so
`curve_reserve = live − U_pub`, today's value. Ordinary pricing, srLP
mint/burn (`reserve_for_burn(curve_reserve)`), hLP basis and the risk curve
all keep reading `curve_reserve`.

### 2.2 Event table

Δ for one asset. `x_p = floor(x·Pu/I)`, `w_p` as §4.3. `a` is the
LP-and-operator part of a credit, `p` the protocol part.

| Event | Δcash | Δlive | Δfunding_rec | Δcredited | ΔPu | Δcurve |
|---|---|---|---|---|---|---|
| Public accrual `i = a + p` | 0 | +i | 0 | +a | +p | 0 |
| Funding accrual `f = a + p` | 0 | 0 | +f | +a | +p | 0 |
| Vault netting `n` (§4.4) | −n_p | −n_p | −n | −n | −n_p | 0 |
| Public repayment, principal r, interest x | +r+x−x_p | −x_p | 0 | 0 | −x_p | 0 |
| Funding payment x by a vault | −x_p | −x_p | −x | 0 | −x_p | −x |
| Claim y (holder, seed, operator) | −y | −y | 0 | −y | 0 | 0 |
| `book_protocol_interest` | 0 | 0 | 0 | 0 | 0 | 0 |
| Public write-off: principal s, interest w | 0 | −(s+w) | 0 | 0 | −w_p | −(s+w−w_p) |
| Funding write-off w (terminal close) | 0 | 0 | −w | 0 | −w_p | −(w−w_p) |
| Junior liquidation credit j | +j | +j | 0 | +j | 0 | 0 |

Row checks: repayment `Δcurve = −x_p − (−x_p) = 0`; funding payment
`Δcurve = −x_p − x + x_p = −x`; netting `−n_p − n + n + n_p = 0`; claim
`−y + y = 0`.

Accrual, repayment, netting and claims leave `curve_reserve` unchanged.
Write-offs reduce P by the LP-and-operator part, which is exactly the amount
passed to `absorb_funded_credit_loss` (junior, then senior). The funding
payment row is the vault spending pool principal it owns (rehedge burns vault
srLP worth x); its curve change is identical to today's (`cash −= x,
live −= x` today, `Δcurve = −x`).

### 2.3 hLP funding and spot

- **Accrual.** Funding interest grows the vault debt and `funding_recognized`
  by f while credits plus Pu grow by f. `curve_reserve`, `HlpCurveBasis`
  (built from `curve_reserves_nad` and recorded `hlp_live`, amm/mod.rs:774-884)
  and the ordinary point are unchanged. No pricing offset is involved.
- **Payment.** Unchanged pricing mechanics. The plan still computes final
  curve reserves with `−paid` (engine.rs:907-933), `consume` still calls
  `preserve_hlp_pricing_point` (engine.rs:1462-1464), withdraw still preserves
  `pricing_point_before` (1841-1843, 1925-1927), and terminal paths still call
  `exclude_hlp_funding_payment_from_pricing` (engine.rs:577,
  flash_terminal.rs:145). `rescale_unpriced_funding_inventory`,
  `bound_unpriced_funding_inventory` and `scale_hlp_pricing_offsets` are
  unchanged. Only the cash and live debit inside the payment changes, from x
  to `x_p` (§5.6).
- **Not chosen:** adding funding interest to `live_reserve`. It would change
  the live identity checked in every reserve path. The formula above confines
  the change to `curve_reserve`, `curve_exclusion` and
  `projected_curve_reserve_from`.

## 3. Custody

### 3.1 Payers

| Payout | Source | Ledger effect |
|---|---|---|
| srLP, junior, hLP holder interest (harvest) | reserve vault | `pay = min(accrued_interest_amount, cash_reserve)`; cash, live, credited −= pay |
| Junior seed interest (`fund_junior_seed`) | stays in reserve vault | moved from cash into swap-fee custody, §5.8 |
| Operator interest | reserve vault | `pay = min(claimable, max_debit, cash_reserve)` |
| Referral interest | reserve vault | `referral_interest_liability −= amount` |
| Protocol interest auction | reserve vault (`ProtocolRevenueSource::Interest`) | `interest_{protocol,buyback}_fee_liability −= amount` |
| Swap fees | reserve vault (unchanged) | `swap_fee_custody_balance` (unchanged) |

### 3.2 Interest vault retirement

Delete the interest vault from `InitializeMarket` (instructions/market/
initialization.rs:219, 230), every Accounts struct (Repay, AddLeverageMargin,
CloseLeverage, DecreaseLeverage, FlashLiquidationAccounts, WithdrawSingleSided,
CloseInsolventHlp, Harvest, FundJuniorSeed, ClaimOperatorRevenue,
ClaimReferralInterest, FundInterest) and the `HlpSwapAccountLayout`
remaining-account prefix (instructions/accounts.rs:90-91, 116-122, indexes 3
and 4; shift later indexes down by two). Delete `validate_interest_accounts`.
`settle_protocol_auction` for the Interest source sells from `reserve_vault`
(settle_protocol_auction.rs:99).

Keeping the vault for protocol revenue alone was considered: it needs a
reserve-to-interest-vault transfer of `x_p` at every payment, with a
Token-2022 fee charged to someone, and keeps two accounts on every hLP swap.

### 3.3 Backing assertions

`Fees::assert_backed` (revenue.rs:123-149) keeps only its swap half. Interest
backing is:

1. `require_reserve_custody` (instructions/accounts.rs:305-317) becomes

   ```text
   vault ≥ cash_reserve + swap_fee_custody_balance + hlp_backing_inventory
         + protected_recenter_reserve + referral_interest_liability
         + interest_protocol_fee_liability + interest_buyback_fee_liability
         + ledger.protocol_pending
   ```

   (the advance escrow terms go). Every instruction that moves reserve-vault
   tokens already calls it last; add it to referral claim, operator claim and
   protocol auction settle.
2. Payouts debit `credited` with `checked_sub` (`UnbackedFeeLiability`), so
   total claims paid never exceed total credited.
3. `curve_reserve` fails on P < 0.

Q is deliberately not backed by cash: claims are limited by `cash_reserve`
and the pool fronts unpaid interest.

### 3.4 Token-2022

| Transfer | Who pays the fee |
|---|---|
| Borrower repayment wallet → reserve (repay, add_leverage_margin, repay_leverage) | borrower: `gross = gross_for_credit(fee, repay_credit)`; the extra `interest_transfer_fee` is gone |
| Flash buyer session vault → reserve (repaid + junior + protocol fee, one transfer) | buyer, `effective_gross_for_credit(sum)` as today |
| Reserve → holder, operator, referral, auction buyer | recipient receives net (as today) |
| Reserve → liquidation reward, owner, terminal caller bounty | unchanged |
| hLP funding, netting, leverage/liquidation interest, seed funding | no transfer |

Delete `pay_interest_transfer_fee`, `pay_settled_interest_transfer_fee`,
`pay_hlp_funding_transfer_fees` (market_income.rs:350-436) and every fee term
in hLP cash floors.

## 4. Kernels (new file transitions/interest_ledger.rs)

Every function below is `#[inline(never)]` and allocates nothing.

### 4.1 `Market::credit_interest(asset, source, gross: u128) -> Result<InterestCredit>`

`source ∈ {Public, Funding}`. Called only by `recognize_interest`. Split in
u64-sized chunks (at most two for Public, since each debt bucket is ≤ u64 by
the accrual clip). For each chunk `g`:

```text
p  = split_with_fraction(g, markup, 10_000 + markup, &mut protocol_fraction_q64)
o  = split_with_fraction(g − p, operator_share_bps, 10_000, &mut operator_fraction_q64)
lp = g − p − o
protocol_unpaid    += p
operator_claimable += o
credited           += o + lp
senior = fees.junior.allocate(lp, BorrowInterest)          // junior index, or seed when wiped
supply_s = srlp_supply − junior_reserve_shares             // senior supply
if Public:
    v_b = floor(senior · base_vault.srlp_shares  / supply_s)
    v_q = floor(senior · quote_vault.srlp_shares / supply_s)
else:
    v_b = v_q = 0
ordinary = senior − v_b − v_q
supply_o = supply_s − base_vault.srlp_shares − quote_vault.srlp_shares   // ≥ MIN_LIQUIDITY
(Δ, carry) = distribute_growth_q64(ordinary, supply_o, interest_growth_remainder_scaled)
interest_growth_index_q64 += Δ; interest_growth_remainder_scaled = carry
vault_credit = (v_b for base vault, v_q for quote vault), in `asset`
```

`split_with_fraction` moves from transitions/operator_revenue.rs:12-19. The
markup and operator share are the cached values at accrual
(`protocol_interest_markup_bps`, `operator.interest_share_bps`); both are
checkpointed before they change, as today. Then, for Public only:

- the vault whose borrowed asset is `asset` (quote vault when `asset == Base`)
  nets its `vault_credit` first (§4.4) and publishes the remainder;
- the other vault publishes its whole `vault_credit`.

Publishing is `vault.publish_new_interest(asset, amount)` (hlp/harvest.rs:39-53,
kept), which feeds the holder index or `unallocated_*` when `hlp_supply == 0`.

The receipt `InterestCredit { protocol, operator, junior, seed, ordinary,
vault_published: [u64; 2], netted }` feeds the accrual event (§5.1).

### 4.2 `Market::recognize_interest(asset) -> Result<()>`

```text
// Funding first: public crediting nets against funding_recognized (§4.4).
fund_now = hlp_funding_debt(asset) − opposite_vault.debt_principal   (saturating, as sync today)
if fund_now > funding_recognized: credit_interest(asset, Funding, fund_now − funding_recognized)
if fund_now < funding_recognized: settle_funding_interest(asset, 0, funding_recognized − fund_now, Required)
funding_recognized = fund_now
pub_now = public_unpaid_interest(asset)
if pub_now > public_recognized: credit_interest(asset, Public, pub_now − public_recognized)   // may net, lowering funding_recognized and vault debt together
if pub_now < public_recognized: settle_interest(asset, Public, 0, Required)                  // write-off of the drift
public_recognized = pub_now
```

A decrease reaches this function only as share-burn drift of an atom or two
in an hLP path (for example `clear_debt_repay`, hlp/mod.rs:103-141). It is
written off like any other unpaid interest: `recognize_interest` passes the
returned `lp_loss` to `absorb_funded_credit_loss`. Every instruction ends
with recognized == computed and accrual only increases interest, so accrual
never takes the write-off branch.

Called by `accrue_side` after the index update (replacing both sync pairs,
lending/mod.rs:1110-1111 and 1229-1230), by the same-slot path of
`accrue_interest_to_slot` (lending/mod.rs:1371-1395) in place of the two
`accrue_side::<false>` calls, and at the end of every debt-increasing path
(borrow, leverage `Borrow` policy, hLP plans that raise vault debt). It
credits the borrow rounding atom (lending/mod.rs:1659-1671) and the share-burn
phantom atom in the instruction that creates them, so the next accrual moves
nothing but interest.

### 4.3 `Market::settle_interest(asset, source, paid: u64, protocol: ProtocolCash) -> Result<InterestSettlement>`

For `Public` it is called after the path has mutated debt shares and
principal, and measures `removed` from the debt. For `Funding` the caller
passes `removed = paid + written_off` explicitly, because hLP plans install
the vault's debt after their settlement step and already know both amounts
exactly (`due − carried`, or the terminal cancelation). Every hLP path ends
with `recognize_interest`, and `assert_market_invariants` checks recognized ==
computed.

```text
Public:  before = public_recognized; now = public_unpaid_interest(asset)
         if now > before { require paid == 0; credit_interest(asset, Public, now − before); public_recognized = now; return zero }
         removed = before − now
Funding: removed = paid + written_off (caller); require removed ≤ funding_recognized
require paid ≤ removed
w = removed − paid
I = public_recognized + funding_recognized            (before this call)
if removed == I:                                       (all outstanding interest leaves)
    w_p = ceil(w · Pu / I); x_p = Pu − w_p
else:
    x_p = floor(paid · Pu / I)
    w_p = min(ceil(w · Pu / I), Pu − x_p)
protocol_unpaid −= x_p + w_p;  recognized[source] −= removed
match protocol:
    Required     => cash_reserve −= x_p (CashReserveUnderflow), live −= x_p
    IfCashAllows => c = min(x_p, cash_reserve); cash −= c; live −= c; x_p = c
protocol_pending += x_p
return { protocol_paid: x_p, written_off: w, protocol_written_off: w_p,
         lp_loss: w − w_p }
```

`IfCashAllows` is used only where a spot swap applies a prepared plan
(Ambiguity 7). Any forgone protocol share stays in P.

Thin wrappers used by the paths:

- `settle_public_debt(asset, cash_repaid, interest_paid, debt_removed) ->
  Result<u64 /* protocol_paid */>` replaces `settle_public_debt_reserves`
  (interest_advance.rs:284-324): `cash += cash_repaid`;
  `live = live + cash_repaid − debt_removed`; `settle_interest(Public,
  interest_paid, Required)`; then
  `absorb_funded_credit_loss(asset, debt_removed − cash_repaid − w_p)`.
- `settle_funding_interest(asset, paid, written_off, protocol)` replaces
  `debit_cash_for_hlp_interest` (engine.rs:1989-2017): `settle_interest(
  Funding, …)` and nothing else (no cash or live debit of `paid`). The
  caller absorbs `lp_loss` when `written_off > 0`.

### 4.4 Netting `Market::net_vault_funding(asset, credit: u64) -> Result<u64>`

For the vault V that borrows `asset`, called from the Public
`credit_interest`. `recognize_interest` credits Funding first, so the
funding being netted is already credited to ordinary srLP and junior:

```text
owed = funding_recognized                    // V is the only funding debtor in `asset`
n_max = min(credit, owed)
burn  = largest debt shares with debt(shares) − debt(shares − burn) ≤ n_max
        and debt(shares − burn) ≥ V.debt_principal
n     = debt(shares) − debt(shares − burn)
n_p   = floor(n · Pu / I)                    // I before netting
if n == 0 or n_p > cash_reserve: return 0   // publish the whole credit instead
V.debt_shares −= burn
funding_recognized −= n; credited −= n; protocol_unpaid −= n_p
cash_reserve −= n_p; live −= n_p; protocol_pending += n_p
publish credit − n
```

`debt_principal` and `hlp_live` are untouched, so the pricing basis and frozen
carve-outs are unchanged. Frozen vaults net like active ones and hold the
rest for their creditors (Ambiguity 8).
Netting never fails: an insufficient-cash case skips it.

### 4.5 `Fees::book_protocol_interest(split, referral: u64) -> Result<()>`

`require referral ≤ protocol_pending`; `credit_protocol_interest_revenue(
protocol_pending, split, referral)` (revenue.rs:78-99, unchanged);
`protocol_pending = 0`. Called by:

- `record_interest_paid` (§5.3) with `referral = floor(protocol_paid ·
  referral_share_bps / 10_000)` from this payment only;
- `accrue_market_interest` (instructions/accounting.rs:139-168) after accrual,
  with `referral = 0` (takes `futarchy.protocol_auction_split` alongside the
  markup);
- the transfer hook after its accrual (transfer_hook.rs:105-134; the
  `FutarchyAuthority` is already read at accounts[14]);
- every hLP instruction after its plan, with `referral = 0`;
- `settle_protocol_auction`, before reading the liability.

`protocol_pending` may stay nonzero between instructions (it is counted in
`require_reserve_custody`). There is no zero assertion, so a path that forgets
to book can never make a later swap revert.

### 4.6 Claims `MarketSide::pay_interest(amount) -> Result<u64>`

```text
pay = min(amount, cash_reserve)
cash_reserve −= pay; live_reserve −= pay; ledger.credited −= pay (checked)
```

Used by harvest, operator claim, `fund_junior_seed`. Curve unchanged.

### 4.7 `Market::protocol_share_of(asset, paid) -> u64`

`floor(paid · Pu / I)` on the current state. Used by cash floors to predict
`x_p` in the same order the transition will apply it: leverage lifecycle
(public) first, then the hLP plan (funding). It is exact because both run on
the same post-accrual state.

## 5. Mutation paths

Each item: today, then after. "Settles" means §4.3. "Books" means §4.5.

### 5.1 Accrual (`accrue_side`, lending/mod.rs:1079-1252)

- Today: sync both books before and after the index update; `live +=
  public interest` (1199-1210); `bounded_accrual_index` clips by book capacity
  (accrual.rs:80-155 via `accrual_index_fits` → `income_accrual_fits`).
- After: keep `live += accrued public interest`. Delete both sync pairs and
  call `recognize_interest(asset)` once after the index is installed.
  `bounded_accrual_index` returns `min(desired, ceiling)` with the existing
  per-bucket u64 ceiling (accrual.rs:110-119). That ceiling alone keeps every
  bucket ≤ u64, so delete `accrual_index_fits` and the bisection
  (accrual.rs:44-77, 131-154). Interest is never forgone for bookkeeping.
  `BorrowInterestAccrued` (emitted at instructions/accounting.rs:186-201)
  gains `protocol_credit`, `operator_credit`, `lp_credit` and
  `vault_netted` (u64 each).
- `accrue_market_interest_data` (accounting.rs:170-177): delete the two
  `require_settled` calls.

### 5.2 Borrow

- `borrow_with_market_fees` (lending/mod.rs:1562-1706): replace
  `check_borrow_income_capacity` (1646; accrual.rs:6-38) with
  `check_borrow_debt_domain`, which keeps only its first check (fixed and
  isolated debt ≤ u64, `DebtMathOverflow`, accrual.rs:21-24). Call
  `recognize_interest(asset)` after the rounding-atom live adjustment.
- Leverage `Borrow` policy (open/increase): same check at leverage.rs:819;
  `recognize_interest` after commit.

### 5.3 Borrower repayments

`record_leverage_interest` (instructions/leverage/settlement.rs:689-815) is
replaced by `record_interest_paid(market, asset, futarchy, referral ctx,
interest_paid, protocol_paid, event ctx)`. It computes the referral from
`protocol_paid`, calls `accrue_referral_interest_with_protocol` with
`ReferralInterestQuote::with_protocol_revenue(interest_paid, protocol_paid,
bps)` (state/referral_partner.rs:80-128, drop the `interest_vault_credit` and
`available_fee_custody` arguments), books, and emits `BorrowInterestPaid`
(without `interest_vault_credit`). There is no transfer, no `take_payment`,
no hLP checkpoint and no `settle_new_public_hlp_income`. Callers: REPAY:214,
ALM:217, CL:798, DL:260, FLI:707, FAS:245, EL:259.

- **Fixed repay** (instructions/lending/repay.rs:137-306;
  `repay_with_finalization_and_fee`, lending/mod.rs:1753-1843). Credit
  `= repayment_for_max(fee.unwind_credit(max_gross))`; delete
  `direct_repayment_credit` and `direct_repayment_transfer`
  (interest_advance.rs:124-194). `gross = gross_for_credit(fee,
  cash_repaid)`; measured reserve credit must equal `cash_repaid`. Replace
  the call at 1809 with `settle_public_debt(asset, cash_repaid,
  interest_paid, aggregate_reduction)`. The `principal_credit` and
  `interest_paid` split (1772-1779) is unchanged.
- **add_leverage_margin / repay_leverage**
  (instructions/leverage/add_leverage_margin.rs:118-;
  `repay_leverage_debt_from_curve`, leverage.rs:1820-1898). Same as fixed
  repay; the call at 1848 becomes `settle_public_debt`. The pro-rata
  classification of a voluntary partial repayment
  (`isolated_clearance_for_max`, lending/debt.rs:236-302) is unchanged.

### 5.4 Leverage lifecycle kernel (transitions/leverage.rs:438-799)

Common to every non-`Spot`, non-`Borrow` policy:

- Delete the advance clearing block (663-682) and `restored_advance` from the
  live and cash updates (684-701).
- Interest stays in cash. `cash_debit_out` drops its `+ interest_paid` term
  and `extra_live_debit_out` becomes 0 in Decrease (481-487), Close
  (531-536), SettleLiquidation (597-601) and Liquidate (650-655).
- `apply_leverage_lifecycle_transition` (1012-1048): replace the sync at
  1038-1046 with `settle_interest(asset_in.opposite(), Public,
  interest_paid, Required)`, and put `protocol_paid` and `w_p` on the
  returned transition so the instruction can pass them to
  `record_interest_paid`.
- `projected_curve_reserve_from` (369-397): `projected = start_curve +
  (live_post − live_start)`. Ledger fields do not change inside the kernel.

`Liquidate` (603-656) becomes principal-first (PLAN). It replaces
`realized_interest_split` at 617-618 with:

```text
principal_paid = min(repay_credit, position_principal)
interest_paid  = repay_credit − principal_paid
principal_written_off = position_principal − principal_paid
```

`Liquidate` runs only in tests and the benchmark today; every on-chain
liquidation already uses `SettleLiquidation` or `LiquidationLossAllocation::
quote`, which are principal-first (state/borrow_position.rs:158-176). Fix the
doc on `realized_interest_split` (math/risk.rs:131-137) and the stale comment
at lending/debt.rs:304-307. Partial leverage liquidation
(flash_settlement.rs:119-126) requires `recovery ≥ debt`, so its pro-rata
split writes nothing off and stays.

Full-liquidation reconciliation (733-775) for `Liquidate` and
`SettleLiquidation { full: true }`:

```text
interest_written_off = aggregate_debt_written_off − principal_written_off   // principal first
socialized_principal_loss = principal_written_off
no live adjustment in the kernel (phantom_unpaid_interest deleted)
expected: projected curve after kernel == curve_before − gross_amount_out
```

`apply_leverage_socialized_loss` (1084-1141) then debits `live_reserve` by
`principal_written_off + interest_written_off`. `settle_interest` has already
dropped `w_p` from Pu. It absorbs `principal_written_off +
interest_written_off − w_p` and keeps its inventory snapshot and rebase
order. The early return tests both written-off amounts.

### 5.5 Swap cash floors (transitions/amm/swap.rs)

- Delete the advance branches (129-144, 537-558).
- Decrease (561-580) and Close (581-620): `cash_out = gross_out − cash_repaid
  + protocol_share_of(debt, interest_paid)`.
- `bind_liquidation` (109-162): `cash_out = gross_out − repayment +
  protocol_share_of(debt, interest_paid)`.
- hLP floors `interest_cash_floors` (engine.rs:1234-1253): per borrowed asset,
  `protocol_share_of(asset, paid) + out` (drop the `fees` parameter and the
  inverse fee).
- `interest_transfer_fees` boxes (swap.rs:55-59; settlement.rs:74-87,
  614-626) and their parameters on `prepare_leverage_swap` and
  `prepare_ema_liquidation` (amm/liquidation.rs:100-112) go away.

### 5.6 hLP plan and consume (transitions/liquidity/hlp/engine.rs)

- `prepare_concentrated_hlp_transition_from_end` (712-1144) works in curve
  coordinates. In the preserve branch (907-933),
  `non_debt[i] = curve_reserve(i) − old_hlp_live(i) − paid(i) +
  carried(i)`. In the quote branch (934-972), `non_debt = ordinary + equity`
  with no `receivable_hedge` adjustment. `canonical_debt_for_proportional_
  claim_with_carry` (185-263) takes `non_debt` directly and loses its
  opposite-receivable parameter. `final_curve[i] = non_debt[i] +
  final_hlp_live(opposite)` with no `unrealized` subtraction (1055-1068).
  Delete `unrealized` (739-743).
- `consume` (1255-1476): delete capitalization (1354-1360). Replace
  `debit_cash_for_hlp_interest` (1362-1367) with
  `settle_funding_interest(asset, paid, 0, protocol)`, where `protocol` is
  `IfCashAllows` when called from `PreparedSwap::apply` and `Required`
  otherwise. The identity check (1368-1441) computes `identity_curve =
  identity_live − curve_exclusion(asset)` after that settlement. Tolerances
  are unchanged. After installing the final debt shares, funding interest
  equals `carried` exactly (`final_hlp_live = final_debt − carried`), so
  `funding_recognized == carried` holds without a recognition step.
- Receipt NAV (1077-1093) is equity only. `uses_book_hedge`
  (integrated.rs:151), `reconstruct_hlp_endpoint` (409-444, endpoint debt =
  funded opposite claim + carry) and `materialized_hlp_endpoint` (596-615)
  drop every `hlp_unpaid` term. The certified search (638-644) runs only for
  carry or frozen vaults.
- NAV basis: `hlp_full_underlying_amount` (2567-2585) and
  `hlp_exit_underlying_amount` (2542-2565) both become the vault's srLP share
  of `curve_reserve`. Delete the non-tranche `live_reserve` branch.
  `hlp_share_nav_nad_with_prices` and the entry comment at 1579 lose
  "accrued public interest/coupons".
- Deposit gate (1557-1569): delete the `income_accrual_fits` check. Keep the
  `cash ≥ funding debt` gate for new principal (1570-1576).

### 5.7 hLP paths

- **Integrated curve state** (amm/mod.rs:1212-1572). Without hLP
  (1232-1296): book offsets are `−hlp_funding_*_pricing_offset_nad`; delete
  `base/quote_interest` and the `receivable_hedge` branch. With hLP
  (1298-1572): delete the `hlp_unpaid` matrix (1335-1347), the
  `unrealized × srlp_shares / supply` terms in book claims and opposite nets
  (1348-1383), and the capitalization block (1514-1531). Book offsets are
  `reserves − ordinary − active_equity − (opposite debt or frozen live)`,
  with no interest term.
- **Spot swap** (transitions/amm/swap.rs:197-666; instructions/spot/swap.rs).
  The carry decision (506-536) uses the new floors. Delete
  `pay_hlp_funding_transfer_fees` (340-344) and the interest leg of
  `apply_single_hlp_rebalance_token_changes` (instructions/spot/swap.rs:
  466-509: transfer, measured credit, `record_inline_hlp_interest_credit`).
  Delete the interest debits from projected custody (226-271). The
  instruction books with `referral = 0`. `BorrowInterestPaid` for funding
  carries `protocol_interest_revenue = x_p`.
- **Leverage swaps** (`settle_inline_hlp_accounts`, settlement.rs:198-314):
  delete the interest transfer, measured credit and record (251-311). Keep
  srLP token settlement. Book.
- **deposit_single_sided** (instruction 286-313): delete fee payment and the
  interest transfer. Book.
- **withdraw_single_sided** (engine.rs:1748-1986; instruction 281-370):
  delete capitalization (1816-1818, 1900-1902) and `retire_hlp_receivables`
  (1963-1965). `debit_cash_for_hlp_interest` (1839, 1923) becomes
  `settle_funding_interest(borrowed, interest_paid, 0, Required)`, followed by
  `recognize_interest` at the end of the transition. Keep
  `preserve_hlp_pricing_point(pricing_point_before)`. Delete the instruction's
  `pay_interest_transfer_fee` (308-312), transfer (313-326), credit (328-334)
  and `record_hlp_interest_credit` (349-356), and the `borrowed_interest_vault`
  account (58). Book.
- **Flash recovery** (transitions/liquidity/hlp/flash.rs; instructions/
  hlp_flash.rs). Quote (110-116): `credit = gap.max(protocol_share_of(
  borrowed, interest).saturating_sub(cash_reserve)).max(frozen as u64)`.
  Begin: delete capitalization (185-189). Settle: plan as §5.6; delete
  `pay_hlp_funding_transfer_fees(Settled)` (instruction 367-377) and the
  interest leg of `settle_inline_hlp_accounts`. Book.
- **Flash terminal** (flash_terminal.rs:17-179). `interest_paid =
  loss.interest_paid` (55-71: the cash cap goes). Settle with
  `settle_funding_interest(borrowed, interest_paid, due − interest_paid,
  Required)`; the canceled remainder is the write-off. The direct debit
  (96-111) shrinks to nothing: the protocol share leaves inside the
  settlement and the surplus stays in cash. The surplus credit (129-139)
  becomes `credited += surplus; publish_new_interest(borrowed, surplus)` with
  no transfer and no fee, the same curve change as today's cash debit. Delete
  `retire_hlp_receivables` (140) and the sync (141). Then
  `absorb_funded_credit_loss(borrowed, principal_loss + lp_loss)` (142) and
  `exclude_hlp_funding_payment_from_pricing(borrowed, interest_paid)` (145).
  Canceled funding interest is now a pool loss (PLAN: defaulted interest).
- **close_insolvent_hlp** (engine.rs:468-700; instruction 163-249).
  `interest_paid = loss.interest_paid` (524-533). Delete capitalization
  (535-537), `retire_hlp_receivables` (573) and the sync (574). Settle with
  `settle_funding_interest(borrowed, interest_paid, due − interest_paid,
  Required)`. The caller bounty
  (`interest_paid · HLP_TERMINAL_CALLER_BPS / 10_000`, Ambiguity 6) is paid
  from pool cash: debit cash and live by it for the caller transfer, and add
  it to the absorbed loss. Absorb `principal_loss + lp_loss + bounty`. Keep
  `exclude_hlp_funding_payment_from_pricing(interest_paid)`. Delete
  `pay_interest_transfer_fee` (209-212), the transfer (213-225) and
  `record_hlp_interest_credit` (242-249). Book.
- **hLP pass-through** (transitions/liquidity/hlp/mod.rs:147-216): delete the
  two interest `accrue_fee_liability_with_remainder` calls, the two interest
  `credit_hlp_growth` calls and the interest checkpoint and remainder writes.
  Swap fees are unchanged. `drain_hlp_vault_unallocated_yield` (259-298)
  drops the deleted remainders.
- **Delete** `settle_new_hlp_public_interest` and `take_new_interest`
  (hlp/harvest.rs:20-37, 60-229; keep `publish_new_interest`),
  `settle_new_public_hlp_income` and `HlpInterestAllocated`
  (settlement.rs:833-954), and `record_hlp_interest_credit` /
  `record_inline_hlp_interest_credit` (instructions/liquidity/hlp/mod.rs:
  117-219).
- **Cohorts**: rotate_cohort, burn_retired_receipt and `HlpCohort::
  yield_indexes` are unchanged. A terminally closed vault has zero srLP, so
  accrual allocates nothing to it.

### 5.8 Holders and junior

- **Harvest** (instructions/liquidity/harvest.rs:157-325). Delete
  `checkpoint_holder_income` (201-205) and `interest_vault`. Checkpoint with
  `accrue` as today, then `interest = pay_interest(accrued_interest_amount)`
  (cash-limited). Subtract `interest` from `accrued_interest_amount` and pay
  swap fees in full. Pay both from `reserve_vault`. Require `swap_fee + interest
  > 0`. `prepare_yield_claim` and `settle_yield_claim` (ledger.rs:432-503)
  lose `interest_vault_balance` and carry-forward-interest, and
  `clear_claimed` clears swap fees fully and interest by the paid amount.
  `YieldClaimed` gains `interest_remaining: u64`.
- **Transfer hook** (instructions/transfer_hook.rs): delete `ends` (141-150)
  and `checkpoint_holder_income` in `checkpoint_transfer_party` (424-426);
  keep `accrue` (427-431). Book after accrual.
- **add/remove liquidity** (srlp/add_liquidity.rs:298-316;
  remove_liquidity.rs:155-181): delete the `carry_forward_interest` calls and
  `checkpoint_holder_income`; keep `accrue`. In `remove_liquidity`
  (transitions/liquidity/mod.rs:345-363), zero `interest_growth_remainder_
  scaled` when ordinary supply returns to `MIN_LIQUIDITY` (replaces the
  funding-remainder reset).
- **initialize_yield_accounts** (196-203): `accrue(0, indexes)` only.
- **convert** (instructions/liquidity/tranches/convert.rs:136-160): delete
  `checkpoint_holder_income` (147-148); keep both `accrue` calls with escrow
  as today.
- **burn_retired** (78-96): delete `checkpoint_holder_income`; accrue from
  `cohort.yield_indexes`.
- **Junior wipe** (`absorb_funded_credit_loss`, transitions/tranches/
  market.rs:59-96): delete the `junior_end` block (83-91). Once
  `junior_reserve_shares == 0`, `allocate` routes junior interest to the seed,
  so the wiped generation's index stops on its own.
- **New generation** (instructions/liquidity/tranches/initialize.rs:59-98):
  delete `old.income_end` and the `junior_end` reset (79-80); `final_income`
  already freezes both indexes.
- **Seed**: `checkpoint_junior_seed` (tranches/market.rs:34-50) keeps only
  the two `checkpoint_seed` calls. `fund_junior_seed` (instructions/
  liquidity/tranches/seed.rs:43-79; transitions/tranches/seed.rs:14-44):
  `d = pay_interest(seed_interest_amount)`, `seed_interest_amount −= d`,
  `swap_fee_custody_balance += d`, `swap_fee_liability += d`,
  `seed_swap_amount += d`. No transfer, no interest-vault account.
  `compound_junior_seed` is unchanged. The seed absorbs losses as ordinary
  junior capital already (permanent receipts sit in `junior_receipt_supply`);
  no program change.
- **Funded value**: `funded_reserve_value` (tranches/market.rs:123-152) sums
  `curve_reserve` at the mark.
- **Junior liquidation coverage** (`record_junior_liquidation_credit`,
  ledger.rs:122-140): `credited += j` instead of the vault counters. Custody
  is in §5.10.

### 5.9 Operator and protocol

- **Operator claim** (instructions/operator_revenue.rs:77-172). Current epoch:
  `pay_interest(min(ledger.operator_claimable, max_debit))`, then
  `operator_claimable −= paid`. Archive epoch: draw from
  `archive.claimable[asset]`, still through `pay_interest` (the archived
  amount is part of `credited`). Transfer from `reserve_vault`. Delete the
  `revenue_interest_liability` and vault-balance lines.
- **Recipient handoff** (instructions/operator.rs:253-289):
  `archive.claimable[a] = ledger[a].operator_claimable;
  operator_claimable = 0` for both assets (Ambiguity 2).
- **Share edit**: unchanged (accrue first, then install).
- **Referral claim** (instructions/referral/accrual.rs:160-211): pay from
  `reserve_vault`; `settle_referral_interest_claim(amount)` subtracts the
  liability and no longer overwrites a vault balance (ledger.rs:347-356);
  `require_reserve_custody`.
- **Protocol auction** (settle_protocol_auction.rs:90-110, 213-255): book
  first; Interest source sells from `reserve_vault`; the custody check is
  `require_reserve_custody` after the transfer;
  `settle_protocol_auction_liability` (revenue.rs:160-189) only subtracts the
  liability.

### 5.10 Flash, AMM-flash and emergency liquidation

- **flash_liquidation** (instructions/flash_liquidation.rs:517-785;
  `settle_flash_recovery`, flash_settlement.rs:148-214): the loss quote is
  already principal-first. Replace FS:206-207 with `settle_public_debt(asset,
  principal_credit + interest_paid, interest_paid, debt)`. The session vault
  pays `repaid + junior + protocol` to the reserve vault in one transfer
  (629-668; the exact-credit check is on the sum). Then
  `record_junior_liquidation_credit(junior)` adds `cash += junior; live +=
  junior; credited += junior`. `record_liquidation_protocol_credit(protocol)`
  (ledger.rs:143-165) books the liabilities without a vault counter (the
  atoms are custody). `record_interest_paid(loss.interest_paid,
  protocol_paid)`. Delete the vault post-check (758-762).
- **flash_amm_settlement** (instructions/flash_amm_settlement.rs:69-343) and
  **emergency_liquidation** (instructions/emergency_liquidation.rs;
  transitions/amm_liquidation.rs). The swap kernel debits cash for the whole
  output as today. Fee debits become net amounts:
  `fee_debits` (AL:49-54) and FAS:107-118 drop `effective_gross_for_credit`.
  The junior and protocol fee transfers to the interest vault
  (FAS:209-237, EL:216-249) are deleted. Credit the junior fee back with
  `record_junior_liquidation_credit` (cash, live and credited `+= j`). Book
  the protocol fee with `record_liquidation_protocol_credit`; its atoms stay
  in the reserve vault, already outside cash. `record_interest_paid` (FAS:245,
  EL:259). Delete the vault post-checks (FAS:296-300, EL:297-301).
  `apply_emergency_liquidation` without a swap (AL:347-350) uses
  `settle_flash_recovery(…, 0, full)`; everything is written off through
  `settle_public_debt`.
- **Borrow-position liquidation** (`settle_internal_liquidation`,
  lending/liquidation.rs:250-347, benchmark and tests only): `settle_public_
  debt` at 324.

### 5.11 Pricing, quotes and scratch markets

- `market_config.rs:531` (reset), 766 and 887 (snapshot and restore of
  `income`): delete. The ledger is restored with `base_side`/`quote_side`.
- `amm/liquidation.rs:114-118` (income reset on the kept reference) and
  `liquidation_sizing.rs:33, 61` (income clone): delete.
- `instructions/preview.rs:1241-1245`: delete the `FundingFeePayment` use; the
  preview runs the same consume.
- `srlp_exchange_rate_nad` (ledger.rs:175-184) divides `curve_reserve`, not
  `live_reserve`.
- Risk and borrow headroom read `cash_reserve` (tranches/market.rs:100-102),
  which now includes paid-unclaimed interest. That interest is borrowable
  and swappable, as the claim-limit decision implies.

## 6. Rounding and remainders

| Step | Rounding | Carry kept by | Favors |
|---|---|---|---|
| Protocol split at accrual | floor | `protocol_fraction_q64` | LP/operator now; protocol cumulatively exact |
| Operator split | floor | `operator_fraction_q64` | LP now; operator cumulatively exact |
| Junior split | floor | `TrancheRevenueCarry.interest_remainder_bps` | senior now |
| Vault allocations `v_b`, `v_q` | floor | none (ordinary gets the remainder) | ordinary srLP, ≤ 2 atoms per accrual |
| Lane index (srLP, junior, hLP) | floor | `*_growth_remainder_scaled` | dust stays in `credited` |
| Holder checkpoint | floor | `YieldAccount.*_remainder_q64` | pool |
| Protocol share of payment | floor `x·Pu/I` | Pu keeps the remainder | LP cash; Pu drains to 0 when I does |
| Protocol share of write-off | ceil `w·Pu/I`, capped at `Pu − x_p` | — | LP loss minimized |
| Netting share burn | debt reduction ≤ credit | unnetted credit is published | vault holders |
| Claims | `min(claimable, cash)` | remainder stays claimable | — |

Invariant: Σ(holder, seed, operator payouts + netting) ≤ Σ credited LP and
operator interest + junior liquidation credits + flash surplus credits. It is
enforced by the floors and by the checked `credited` subtraction. `Pu ≤ I`
holds after every floor and ceil above (floor: `Pu' < Pu·I'/I + 1 ≤ I' + 1`;
ceil: `Pu' ≤ Pu·I'/I ≤ I'`). Interest indexed onto the 1,000 burned
`MIN_LIQUIDITY` srLP shares is never claimed and stays in `credited` for good,
as book units on that lane do today.

## 7. Deletions

**Files**: state/income_epoch.rs; transitions/tranches/income_receivable.rs;
transitions/tranches/lazy_income.rs; transitions/tranches/market_income.rs
(whole file: the fee helpers at 350-436 go with the vault);
transitions/interest_advance.rs (keep `gross_for_credit` in token.rs or
inline); instructions/liquidity/tranches/checkpoint_income.rs;
instructions/liquidity/tranches/income_epoch.rs;
instructions/liquidity/fund_interest.rs; tests listed in §9.

**Functions and types**: `IncomeReceivableBook`, `IncomeReceivableClaim`,
`ReceivableGrowth`, `IncomeEpoch`, `IncomeExposureEnd`, `MarketIncomeBooks`,
`LazyIncomeBooks`, `InterestRevenueBook` and its impl
(transitions/operator_revenue.rs, keep only `split_with_fraction` moved to
interest_ledger.rs), `InterestRevenueCredit`, `PublicInterestAdvance`,
`fund_public_interest*`, `settle_public_debt_reserves`,
`direct_repayment_credit/transfer`, `check_borrow_income_capacity`,
`income_accrual_fits`, `accrual_index_fits`, `checkpoint_holder_income`,
`checkpoint_seed_receivables`, `hlp_unpaid_income`,
`capitalize_hlp_receivable`, `quote_hlp_capitalization_for_value`,
`quote_hlp_receivable_conversion`, `retire_hlp_receivables`,
`sync/issue/clear/collect_income_receivable`,
`record_receivable_income_credit`, `quote_interest_revenue_credit`,
`pay_*interest_transfer_fee*`, `FundingFeePayment`,
`debit_cash_for_hlp_interest`, `record_hlp_interest_credit`,
`record_inline_hlp_interest_credit`, `record_hlp_funding_interest_credit_with_
supply` (ledger.rs:285-345), `carry_forward_interest(_with_supply)`
(395-430), `settle_new_hlp_public_interest`, `take_new_interest`,
`settle_new_public_hlp_income`, `record_leverage_interest`,
`unrealized_interest`, `YieldAccount::credit_interest_growth` (fold into
`accrue`), `validate_interest_accounts`.

**Instructions** (lib.rs): `checkpoint_income` (59-61), `roll_income_epoch`
(76-78), `fund_interest` (263-271).

**Errors** (errors.rs): `IncomeEpochNeedsRollover` (412-413),
`IncomeHistoryRequired` (414-415). `OperatorRevenueArchiveRequired` stays.

**Events**: `PublicInterestFunded` (fund_interest.rs:40-45),
`HlpInterestAllocated` (settlement.rs:833-841); fields
`BorrowInterestPaid.interest_vault_credit` (events.rs:631),
`ReferralInterestAccrued.interest_vault_credit` (362),
`HlpRebalanced` interest fee (214-215). `FeesReceipt` loses
`interest_vault_balance`; `interest_liability` → `credited: u128`.

**SDK** (packages/dusk-sdk/src):
- Delete income-receivable.ts.
- interest-accounting.ts: delete `quoteInterestAdvanceAmounts` and
  `allocateRepaidInterest`; fix the `hlpFundingInterest` doc.
- tranche-client.ts: delete `incomeBookMayRoll`, `incomeBookAdvancesInPlace`,
  `rollIncome`, `checkpointIncome`, `archivedIncomeEpochs`,
  `prepareHolderIncome`, `prepareDrainedIncomeBooks`; `fundSeed` drops
  `interestVault`.
- yield-client.ts: delete `FundInterestParams`, `fundInstruction`,
  `previewFunding` and `fundAccruedInterest`; `buildHarvest` builds harvest
  only.
- constants.ts: delete `deriveIncomeEpochAddress` (700-703).
- type-aliases.ts: delete aliases 13, 40, 50-54, 105, 199, 209.
- index.ts: update re-exports (42-45, 51).
- write.ts: drop `fundInterest` from `ORACLE_READ_INSTRUCTIONS` (2470).
- operator-client.ts:131: read `fees.ledger.operatorClaimable`.
- Every builder: drop interest-vault accounts and the two hLP swap
  remaining-account slots.
- Add claimable projection: index plus a replay of §4.1 since the last
  accrual.
- Regenerate `types_v2.ts` and `idl_v2.json`.

**SDK scripts**: delete check-income-receivable.js; trim
check-interest-accounting.js (15-57, 80-87, 113-126), check-tranches.js (4,
62-64), check-program-id.js (13, 195, 280-281), check-market-readers.js (92),
check-write-program-accounts.js (13, 336-357); remove both scripts from
`test:previews` (package.json:64).

**Repo scripts**: scripts/check_one_use_helpers.js:431, 439-440;
scripts/derive_operator_fixtures.cjs:63, 72-73;
scripts/joint-shock-replay/README.md:37-38.

**Docs**: docs/LP_INTEREST_ACCOUNTING.md (157-441 rewritten around §2-§4),
packages/dusk-sdk/README.md (1064-1126, 1228-1308+), programs/dusk/README.md
(43-44). Webapp work is as PLAN states.

## 8. Constraints

**Default-heap emergency liquidation (32 KiB, tiered oracle position).**
Today the decoded books take 2,520 B and the measured margin is 82 B. After:

| Change | Heap |
|---|---|
| No `MarketIncomeBooks` decode (and no copy-on-write clone in scratch markets) | −2,520 B |
| `HlpVault` −48 B × 2 in the decoded Market and in the one scratch clone | −192 B |
| One fewer account entry (interest vault) in the entrypoint | −120 B |
| `record_leverage_interest` no longer runs `settle_new_public_hlp_income` (integrated state, plan, event) when hLP is active | not counted; measure |
| `BorrowInterestAccrued` +32 B per buffer | +32 B |

Estimated margin is about 2.9 KB, against 82 B today. Confirm with the
allocation trace on the tiered state (33,844 B pre-fix case) and with
tests/v2-final-smoke.test.ts:12059.

**SBF 4 KB frames.** New kernels are `#[inline(never)]` with a few u128
locals and no U256. `IntegratedCurveState` loses about 145 B (two coupon
matrices, two u128s, a bool), so the curve-solve and plan frames get
smaller. `LeverageLifecycleState` loses 64 B. Rebuild with `build-sbf` and
grep `frame|stack offset|overwrites values in the frame`.

**Compute.** Removed: Borsh decode and encode of 2,592 bytes on every
Market-touching instruction; one Token CPI and reload per repayment; one or
two per hLP funding settlement; two fee CPIs per liquidation; the integrated
hLP plan inside repayments with active hLP; the certified search triggered
by vault coupons (the 881,839-CU concentrated public-interest case should
fall to the analytic path). Added: one `credit_interest` per asset with debt
per accrual (estimate 4-8k CU, measure) and netting. Same-slot accrual
becomes a recognition compare. The plain same-slot swap (97,627 CU) has no
debt and takes the unchanged early return. Re-baseline the LiteSVM ceilings
from new measurements; none should rise.

**Swaps never revert on interest accounting.**
- The accrual ledger is u128 and cannot overflow under the per-bucket u64
  debt clip (I < 2^66; Q < I + 2^64).
- Netting skips when cash is short.
- Recognition only credits.
- Spot swaps settle funding with `IfCashAllows`, and their carry floor needs
  only the protocol share.
- `protocol_pending` is never asserted zero.
- Growth-index overflow is unchanged from today (needs > 2^64 atoms per
  share).
- Leverage, liquidation and hLP entry/exit/flash paths may still refuse with
  `InsufficientLiquidity` or `CashReserveUnderflow` when cash cannot cover
  output plus protocol share, as today.

## 9. Test plan

**Delete**: tests/transitions/tranches/income_receivable.rs,
tests/transitions/tranches/lazy_income.rs,
tests/transitions/interest_advance.rs.

**Rewrite**:
- tests/transitions/operator_revenue.rs: split carries, handoff archive, cash
  limit.
- tests/transitions/lending_interest.rs: drop the book, advance and
  `fund_interest` cases at 451, 474, 603, 637, 676, 693, 735, 794, 813, 840,
  956, 981, 1024 and 1052; keep the rate cases.
- tests/transitions/hlp_funding.rs (565, 778, 819, 1087, 1137, 1175, 1226).
- tests/transitions/liquidity_hlp_interest.rs: NAV without coupons, netting,
  publish.
- tests/transitions/liquidity_hlp_engine.rs, liquidity_hlp_flash.rs.
- tests/transitions/liquidation_economics.rs,
  liquidation_ema_settlement.rs, liquidation_ema_closeout.rs,
  liquidation_ema_inventory_economics.rs.
- tests/transitions/leverage.rs: 2014; 2688 becomes "repay with zero starting
  cash".
- tests/transitions/tranches/market.rs (52, 74, 97, 200, 227),
  tranche_capital.rs.
- tests/instructions/transfer_hook.rs, tests/state/yield_account.rs,
  ledger_fees.rs, ledger_accounting.rs, support/ledger.rs,
  support/revenue.rs, tests/instructions/liquidity/hlp/mod.rs, layout
  fixtures (tests/instructions/accounts.rs:386-397, spot/swap.rs:50-62).
- benchmark_api.rs (875, 1541-1542, 1707-1708, 1779) and
  programs/dusk/benchmark/benchmark_api.rs (2612, 3328, 3659, 4745, 4812).
- tests/utils/instruction-coverage.ts (139-140, 158).
- tests/v2-final-smoke.test.ts (2579, 2662, 5650, 5995, 6991, 7343, 9922,
  12426-12560, 13109, 13341).

**New native properties** (tests/transitions/interest_ledger.rs). Use a
proptest over random sequences of accrue(dt), borrow, repay (partial or full,
Token-2022 fee), leverage open/decrease/close, borrow and leverage
liquidation (full short, principal-only, partial-interest, partial), hLP
deposit/withdraw, spot swap with funding (pay and carry), every claim kind,
srLP and junior transfer, junior request/convert/cancel, wipe and new
generation, operator handoff, auction settle. Check after every step and per
asset:

1. Identity: `cash + flash + D + I == curve_reserve + credited + Pu`, with D
   and I recomputed from debt shares; recognized == computed; live identity.
2. `curve_reserve` is bit-identical across accrual, netting, repayment,
   protocol booking and every claim. A funding payment changes it by exactly
   `−paid` and the executable spot by 0 (existing atom bound).
3. Σ payouts ≤ Σ credits; `credited − Σ holder entitlements` (after
   checkpointing everyone) lies within the dust bound (one atom per lane
   distribution plus one per holder checkpoint).
4. Each holder's paid plus accrued interest equals `Σ floor(balance ×
   Δindex)` over its holding periods, across transfers, junior escrow,
   conversion and retired-cohort harvest, within one atom per checkpoint.
5. Default: junior reserve value falls by `min(junior value, s + w − w_p)`
   and senior takes the rest; `credited` and `operator_claimable` are
   unchanged; Pu falls by `w_p`.
6. `Pu ≤ I`; `Pu == 0` when `I == 0`; Σ protocol paid ≤ Σ protocol accrued.
7. A short recovery below principal pays `interest_paid == 0` and protocol 0,
   on borrow, leverage `SettleLiquidation`, `Liquidate`, and both terminal
   hLP paths.
8. Netting: vault NAV changes at accrual by `−funding accrued + netted`; spot
   is unchanged.
9. A spot swap never fails in recognition, netting or funding settlement for
   any cash level ≥ its output.

**Integration**: default-heap EL test (v2-final-smoke:12059) plus the
oversize-heap test (12085); full LiteSVM suite and CU scenarios;
`build-sbf` frame grep; IDL freshness. Then a Nemesis pass on the new ledger.

## 10. Ambiguities

Choices the code forces that PLAN.md does not settle and that change
user-visible behavior. **Resolved 2026-11-15: option (a) for items 1-8;
item 9 as described there.**
Items 1, 2 and 6 were decided by the product owner; 3 and 7 follow the locked
paid-only and never-block-swaps rules; 4, 5 and 8 follow the cash-only claim
rule and keep vault behavior uniform.

1. **hLP netting scope and timing.** (a) At accrual, net only against funding
   owed in the same asset and publish the rest at once; price-neutral, no
   trigger. (b) Hold a vault credit in NAV and net both assets at a settle
   point with today's gap rule; moves the mark and needs a trigger now that
   `fund_interest` is gone. Recommend (a).
2. **Operator handoff.** Credited, unclaimed operator interest at a recipient
   change (a) is sealed to the outgoing recipient or (b) follows the new one,
   as unpaid fees do today. Recommend (a).
3. **Liquidation that repays all principal and part of the interest.** The
   protocol gets (a) its pooled `Pu/I` share, like any repayment, or (b)
   nothing until LP and operator interest is whole. Recommend (a).
4. **Funding pay-or-carry floor.** A payment now needs only its protocol
   share in cash. (a) Floor = protocol share + output, so carry is rare. (b)
   Keep today's `paid + output` floor as a throttle. Recommend (a).
5. **Junior liquidation-coverage fees.** (a) Reserve vault, cash-limited like
   all interest. (b) Keep the interest vault for this source, fully backed.
   Recommend (a).
6. **close_insolvent_hlp caller bounty.** (a) Paid from pool cash and
   absorbed with the terminal loss, junior first. (b) Paid from the protocol
   share. (c) Dropped. Recommend (a).
7. **Protocol share that cash cannot cover in a spot swap's apply.** (a) Pay
   what cash allows and forgo the rest, which stays in LP principal. (b)
   Revert. Recommend (a).
8. **Frozen hLP vaults.** (a) Net at accrual like active vaults. (b) Skip
   netting and publish all their public interest. Recommend (a).
   **Resolved 2026-11-16 by the product owner:** (a), with creditors before
   equity. A frozen vault still nets same-asset interest against the funding
   it owes; the rest of its public interest is not published to its holders
   but held on the vault (`HlpVault::held_base_interest` /
   `held_quote_interest`). Held interest stays in `credited`, so accrual
   never moves `curve_reserve` or the price. A terminal close (native or
   flash) applies it to the debt its sale or payment leaves unrecovered,
   borrowed-asset interest first, then target-asset interest valued at the
   terminal EMA mark (`hlp_ema_collateral_value_in_borrowed`); the applied
   part leaves `credited` into LP principal and the unrecovered debt falls
   by exactly its value. The rest is published to the retired cohort in the
   asset it was held in, so held interest never creates a surplus. A
   recovery that unfreezes the vault publishes the held interest to its
   holders in full. Interest published before the freeze stays the
   holders'. With an interest deficit, a vault the terminal close would
   accept is frozen at its EMA marks (no curve price exists), so the rule
   still applies.
9. **Write-off larger than LP principal.** Interest credited at accrual can
   exceed all LP capital in an asset, so a write-off can take `P` below zero.
   **Resolved 2026-11-15 by the product owner:** the liquidation goes through.
   LP capital goes to zero (junior, then senior), and the part of the loss
   capital cannot absorb is recorded as `InterestLedger::deficit` (u128, per
   asset, never decreases). `curve_reserve = max(0, live + F + deficit − Q −
   Pu)`, so `C + D + I + deficit = P + Q + Pu`; `absorb_funded_credit_loss`
   records the deficit before allocating the rest to capital, and
   `assert_interest_ledger` requires every gap to be recorded. Credited
   claims are not haircut: they are paid from remaining cash, first come, and
   `pay_interest`'s checked subtraction on `credited` still bounds every
   payout. `add_liquidity`, `deposit_single_sided` and `borrow` refuse
   with `InterestDeficit` while either asset has a deficit, since new
   capital would fund old claims and no price can move to value new
   credit. The wiped asset has no curve: swaps and curve-routed
   liquidations refuse with `InsufficientLiquidity`, and every other curve
   reader treats the market like an empty one, keeping risk at its last
   observation, so price-free exits (claims, repayment, collateral deposits,
   debt-free withdrawals, srLP exits, flash liquidation) keep working (audit
   AI-001). Indebted positions cannot withdraw collateral and hLP exits
   refuse, both with `InterestDeficit` (recheck R-1, R-5).
   **Wind-down, decided 2026-11-17 by the product owner:** in a market with a
   deficit anyone may wind down any hLP vault, solvent or not, through the
   terminal close (native or flash). Its debt is settled at the close's EMA
   price, held interest applies as in Ambiguity 8, and the remainder goes to
   the retired cohort's holders (native: in kind after the caller bounty;
   flash: the buyer's payment above the debt), who then burn their
   receipts. A solvent wind-down absorbs no loss and never raises a
   deficit: when the vault's borrowed asset has a deficit the native close
   refuses with `InterestDeficit` (it would settle that debt with target
   inventory and grow the deficit, for insolvent vaults too) and the vault
   closes by flash, where the buyer pays the borrowed asset. Outside a
   wiped market the close keeps its insolvency gate.
   **Interest claims, reconfirmed 2026-11-17 by the product owner:** claims
   on credited interest are limited by reserve cash only; there is no
   utilization-based pause.
