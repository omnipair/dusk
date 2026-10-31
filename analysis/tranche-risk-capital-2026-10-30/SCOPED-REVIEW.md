# Verified finding: hLP unpaid-income ownership

Scope: public-income issuance/clearance/collection, exposure checkpoints, debt-to-
receivable synchronization, native custody credits, hLP valuation and collection.
This is a scoped implementation review, not a full-program security audit.

**Confirmed and corrected in the working tree.** The old hLP valuation assigned
all unpaid public interest in proportion to common LP shares. The tranche ledger
assigned only half to senior, with hLP participating in that half in proportion to
senior shares. Valuation and actual income allocation therefore disagreed.

Native reproduction: 20,000 base tokens in hLP and 10,000 newly accrued base
interest gave hLP a 98.135426-token receivable. The old valuation added 196.078431.
`tranche_hlp_nav_includes_only_its_allocated_unpaid_interest` failed before the
change and now passes. The fix values funded common backing plus only the vault's
own checkpointed receivables.

Cross-function checks now cover issuance and allocation conservation, canceled
versus collected interest, paid-index checkpoints, funding-cost netting, partial
and final exits, newcomer exclusion, and rehedging across both assets and curve
amplifications. `tranche_hlp_exit_buys_only_departing_coupons_and_does_not_gift_next_depositor`
checks native quote/settlement agreement, no dilution of remaining capital, matched
public advances and no old coupon payout after re-entry. Opposite-asset conversion
uses the existing AMM quote; price impact can benefit remaining capital.

A negative funded component with positive total hLP NAV is handled by purchasing
only the needed target coupon into pool backing, then completing the same funding
and ownership settlement. No funding/hedging feature was disabled. The Solana
instruction is atomic; callers of lower-level Rust transitions must not retain
partially mutated scratch state after an error.

## Integration and operating limits

The final verification manifest records SBF execution, native compounded-fee and
liquidation checks. This review is not independent and does not establish
production readiness.

- Historical income archives preserve the earning holder's claim. Catch-up is
  bounded to eight epochs per instruction, but total history is unbounded. A
  rent-paying caller can create extra history by alternating settlement and rolls.
  This is a maintenance-cost/griefing surface; bounded batches do not eliminate it.
  Active-holder checkpointing and retryable SDK plans are required. A stronger
  archival policy or compressed history remains a security-review topic.
- Junior seed reinvestment requires a balanced asset pair. An unmatched asset
  remains in seed custody and is excluded from productive risk coverage.
- Queue crowding delays an individual request by up to three additional days; it
  does not impose a global conversion-per-day cap. Requests can become executable
  together. Coverage can fall when they convert, and the rate controller responds
  to the capital that remains.
- Uncapped APR is bounded by integer representation, not an economic ceiling.
  Market creators and governance can set an APR ceiling. The default does not
  guarantee borrowing demand, underwriter returns or withdrawal liquidity.
- The optional liquidation policy benchmark includes eighteen adverse price
  observations and five minutes of unchanged external-price observations for
  settlement. Some small positions clear only after another observation. The
  test preserves the no-critical-residual assertion at the end of that explicit
  period; it is not evidence of immediate or universally profitable liquidation.

No liquidation feature, hLP funding behavior or leverage mode was disabled to
pass these checks. Future review should exercise arbitrary operation ordering,
archive churn, queue concentration and same-transaction cross-program calls.
