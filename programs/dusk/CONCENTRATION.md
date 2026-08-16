# Explicit Concentrated AMM

## Product contract

Dusk combines three product requirements in one deterministic transition:

1. a concentrated band improves execution near a sticky internal center;
2. volatility and distance-from-center fees charge toxic flow;
3. each hLP ends with its opposite-asset claim matched by funding debt to the canonical atom.

The hedge is priced through ordinary reserves. Synthetic hLP leverage is not
counted as free trader-visible liquidity. No caller witness, invariant search,
finite-difference Jacobian, or Broyden correction is used.

## Curve

Let `X` and `Y` be real ordinary reserves, `Lt > 0` the full-range tail
liquidity, `Lc` the concentrated liquidity, and `sl`, `su` the lower and upper
square-root prices. The curve is continuous across three explicit branches:

```text
inner:       (X + Lc/su) (Y + Lc*sl) = (Lt + Lc)^2
lower tail:  (X - Lc*(1/sl - 1/su)) Y = Lt^2
upper tail:  X (Y - Lc*(su - sl)) = Lt^2
```

`range_width_nad` places log-symmetric bounds around `center_price_nad`.
`concentrated_liquidity_share_nad = Lc/(Lt+Lc)` controls concentration. A zero
share and zero width select exact CPMM. Governance bounds the share through the
existing maximum-amplification policy, and the tail is always nonzero.

Exact-input and exact-output quotes are conservative integer formulas. A swap
crosses no more than two precomputed boundaries, so it executes at most three
closed-form segments.

## Swap ordering

1. Advance any previously deferred, protected center target.
2. Preview the gross closed-form path only to measure toxicity.
3. Freeze base, volatility, and distance fees.
4. Quote the net input once on the explicit curve.
5. Reconstruct both hLP positions algebraically at the quoted endpoint.
6. Commit cash, live reserves, yLP shares, debt, fee/yield checkpoints, and the
   trade observation once.
7. Let that observation update EMA/volatility and schedule a possible center
   move for a later swap.

## Launch protection and governance

Ordinary yLP liquidity may be seeded before `start_time`; swaps, lending,
leverage, and hLP funding remain unavailable until that exact timestamp.
Markets may configure a bounded launch base fee that decays in O(1) from the
launch fee to the normal base fee using either a linear schedule or sixteen
deterministic exponential steps. Distance and volatility charges remain
additive under the existing total-fee cap.

The launch fee scheduler and buy-size limiter solve different problems and
may be composed:

- The **time scheduler** protects *when*: every early swap pays the scheduled
  base fee, in both directions, and that premium decays with wall-clock time.
- The **buy-size limiter** protects *how much*: during its own bounded window,
  only swaps buying the configured launch asset pay an additional stepped
  premium. The first reference amount adds nothing; each full or partial
  reference amount after it adds the governed increment, capped by the
  governed maximum fee.
- The ordinary distance-from-center fee protects *where the path moves* and
  therefore continues to charge repeated outward flow after launch.

The limiter is stateless per swap: it adds no PDA, account, keeper, or
inventory gate. Splitting a buy across transactions can reduce the size
premium, although the sequential swaps still move the curve and incur the
path-dependent distance fee. Dusk intentionally does not include an Alpha
Vault or privileged early-order allocation.

The time schedule and size limiter are fields of the existing timelocked fee
governance family. A market can enable either mechanism, both, or neither.

The sticky-center threshold, step, and minimum adjustment interval form an
independent timelocked governance family. A governance update invalidates any
deferred center target so the next real operation evaluates the new policy
against current reserves. No manager/operator or maintenance crank is used.

The current swap never moves the center used to price itself. Retained
surcharge is credited after the endpoint into a custody-backed, non-quoteable
Base/Quote recenter bucket. It is excluded from yLP NAV and withdrawals. A
later admitted center move atomically deploys the bucket as reserve principal
only when the resulting curve leaves yLP principal unimpaired.

## hLP hedge

Perfect hedge means zero net opposite-asset exposure, not literal constant
gross 2x leverage away from a balanced 50/50 point. For each vault:

```text
opposite-asset yLP claim - opposite-asset funding debt = 0
```

At a balanced CPMM point this is the familiar 2x result. On an off-center
concentrated curve the LP inventory weights differ, so the gross leverage
needed for the same one-sided Yield Basis exposure changes with the curve.

The implementation reconstructs debt and yLP ownership from final ordinary
reserves using canonical floor/ceil claim rules. Both active hLPs are checked at
atom precision after Spot, leverage, and liquidation transitions.

### Passive funding recovery

If indexed funding debt grows above an hLP's canonical opposite-asset claim,
a Spot swap whose input is that borrowed asset receives an hLP-funded output
improvement. Stress is measured as `debt / opposite_claim`: the discount ramps
linearly to 500 bps at 17/16 and reports critical stress at 9/8. Matching input,
the funding gap, and remaining hLP target equity cap the tranche.

The complete trader input still executes on the ordinary curve. Only the bonus
output is paid by reducing the stressed hLP's target equity; ordinary yLP shares
and published fee claims are untouched. The final transition pays accrued
funding interest and selects debt directly from the final proportional yLP
claim, using a fixed adjacent-atom rounding certificate rather than an
iterative solver. At the 9/8 critical boundary, anyone may submit the same
transition through `liquidate_hlp`. It remains available in reduce-only mode,
uses the ordinary swap account layout, and rejects unless the caller's input
actually receives a critical hLP-funded recovery tranche. Existing arbitrage
and liquidation bots can therefore recover idle markets without a dedicated
keeper.

If passive funding consumes all marked hLP collateral before that recovery
executes, `liquidate_exhausted_hlp` closes the vault through a terminal
waterfall. It first retires the vault-owned yLP position, then draws the
borrowed-asset insurance vault, credits the payable portion of funding interest
to yLP, and socializes only the caller-capped remainder of that accrued funding
interest. A loss reaching original debt principal is rejected as a broken
hedge/accounting invariant. Published hLP fee claims are not seized; the
remaining hLP tokens are burnable for zero principal after closure. The
terminal instruction has its own insurance/interest/yLP accounts, so ordinary
swaps keep their existing account list.

## Accounting invariants

For each asset:

```text
R_live = R_cash + cash-backed debt + hLP backing
```

hLP funding debt is a vault liability and is not added again as an AMM reserve.
Only transferable cash may leave a vault. Claimable fees and protected
recenter reserves remain custodied but outside executable principal until
their respective ownership/admission rules release them.

Lending and liquidation reserve-at-price projections use the same explicit
three-branch inverse as swaps. Proportional yLP changes scale both liquidity
tranches. A protected center change reconstructs the explicit state through
the same positive closed-form constructor after deploying its locked bucket;
ordinary withdrawals cannot deploy or redeem that bucket.

## Preview surface

Preview reports the band bounds, current branch, ordinary reserves, final spot,
fee breakdown, hLP debt deltas, and the hLP recovery gap/match/discount/bonus/
critical flag. Ordinary swap arguments are unchanged.

## Acceptance

- exact CPMM compatibility when concentrated share is zero;
- no iterative quote or hLP solve in production;
- conservative exact-input/exact-output replay at branch boundaries;
- cash solvency and reserve identities after every transition;
- atom-precise opposite-claim/debt equality for active hLPs;
- identical curve semantics for Spot, leverage, lending, and liquidation;
- SBF verifier/default heap success and a representative complete swap at or
  below 100,000 CU.
