# Devnet E2E program repairs — 26 October 2026

Two instruction-level failures discovered during application testing are
reproduced and covered by regressions in `tests/v2-final-smoke.test.ts`.
These changes preserve the account layouts, instructions, SDK interface and
liquidation policy.

## Reseeding after the last external yLP exit

The last external withdrawal leaves the permanently locked minimum shares and
small cash reserves. It deliberately clears the concentrated AMM. Previously,
the next deposit's update tried to reconstruct the concentrated curve from
that dust before adding the new cash, and continuity validation rejected it.

An uninitialized market with only minimum shares now uses the existing
terminal checkpoint. That checkpoint still requires equal yLP supplies, no
public or isolated debt or principal, cash equal to live reserves, and no
active hLP vault. The new deposit then rebuilds the curve with its added
liquidity. The deposit's ordinary share calculation still accounts for the
locked shares and residual reserves.

The regression seeds a concentrated market, swaps, withdraws all external
shares, previews a new deposit, executes it at the previewed share minimum,
and swaps again. The original binary fails at the new-deposit preview; the
repaired binary completes the entire sequence.

## Bounded emergency recovery proof

Full emergency settlement must prove that no smaller internal settlement can
restore the configured recovery target. Repeated concentrated-curve boundary
calculations exhausted compute on a real devnet position.

The proof now caches exact canonical boundary crossings for a fixed start.
Subsequent interval quotes retain the same integer output with one
constant-product division per segment. Recovery searches visit the upper
interval first to find a real recovery witness without repeatedly rebuilding
the same quote.

For flat maintenance tiers, an additional conservative certificate bounds
every sale's repayment by the largest segment slope, with optimistic fees
and explicit rounding allowances. Its recovery objective is affine in the
amount sold, so checking both endpoints can rule out recovery across the
whole range. An inconclusive certificate falls back to the existing exact
interval proof. Failed quotes and exhausted proof budgets do not authorize
full closure. Ordinary flash liquidation retains its separate, stricter
useful-partial rule.

The transaction regression covers 0.25% and 3% base swap fees, with and without
both hLP vaults active. Low-fee cases retain the protected partial and reject
the full close with `LiquidationRepayTooLarge`; higher-fee cases quote and
execute full emergency settlement while equity is still positive. Observed
successful execution costs were 1,061,627 CU without hLP and 1,291,884 CU
with both vaults. The harness compute ceiling was not increased.

Native regressions compare cached and canonical integer quotes across curve
regions and exact small-reserve boundaries. A separate matrix enumerates all
permitted partial sizes across both debt assets, constant-product and
concentrated curves, fee-collection modes, capped transfer fees and fractional
interest indexes. A full-close certificate must never override a real
fee-bearing recovery witness.

These local regressions do not establish live application acceptance. The
dedicated devnet market, browser writes and deployed compatibility metadata
must be verified separately after the executable upgrade.
