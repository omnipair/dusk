# Calibration and accounting experiment

31 October 2026. Freeze these assumptions before inspecting results. The purpose is
to propose parameters for user sign-off, not claim an empirically optimal market.
Dusk is not deployed; borrower/LP behavior is synthetic and varied explicitly.

## Latest user instructions

- Borrowing remains available with zero junior, subject to cash limits. Insurance,
  then senior, absorbs uncovered losses. Do not introduce a junior launch gate.
- Restore a real permanent junior seed as the primary bootstrap design. Use 0.1%
  of the initial pool as the proposed test seed, funded from creator liquidity.
  No unbacked units count as protection. The seed's income compounds into junior.
- Split net LP-directed interest (ordinary, leverage and hLP funding) and net swap
  revenue 50/50 between srLP and jrLP. This supersedes pro-rata swap fees.
- Redirect the liquidation amount currently credited to insurance entirely to
  junior. Preserve the liquidator reward and protocol cut. Insurance does not
  receive this revenue stream in the new model.
- Keep junior counted and slashable during the 7-10-day notice and two-day window.
  Preserve unused window time during an objective insolvency restriction.
- Compare zero and 5-bp exit-request/renewal fees. Fee destination also matters;
  do not treat either fee choice as approved.

## Experiments and limits

1. Exact accounting examples with two actual token balances, protected claims,
   collected versus unpaid revenue, seed compounding, exhaustion and recapitalization.
   Model generation separation explicitly; do not merely erase a live mint's supply.
2. Bounded processing prototype: cash settlement and principal loss are immediate;
   old conditional income rights settle in capped chunks without scanning all holders
   in a borrower operation. Storage growth and any residual scheduling dependencies
   must be reported, not described as constant-space production code.
3. Controller screening over targets 2%, 5%, 10%; rate doubling/halving times 2, 7,
   14 days; APR caps 50%, 100%, 200%; new-borrow utilization limits 70%, 80%, 90%.
   Initial APR 4%, floor 1%, relative band +/-10% in screening. Refine floor, initial
   rate, band width and time step around the finalists without hiding sensitivity.
4. Compare bootstrap, mature, senior cash exits, junior queued exits, inelastic
   borrowers, slow junior entry, revenue drought, and credit losses. Use the same
   pool/insurance budget and external events across candidates. A separate
   utilization-only benchmark isolates the signal; it is not a native Dusk replay.

No single weighted score defines "best". Report borrowing supported, borrower cost,
junior/senior loss, reserve use, cash withdrawal backlog, peak rates and oscillation.
First identify dominated choices and practical trade-offs. Do not declare success
merely because high rates eliminate borrowing and therefore losses.

Macro simulations use a synthetic balance sheet and specified revenue/demand
responses. They do not reproduce AMM trades, collateral auctions, MEV or empirical
LP risk preferences. The exact ledger and macro model have distinct roles.

## Accounting conventions

The permanent seed is a non-redeemable protocol-owned junior claim. Its received
income buys additional junior units at the then-current price; ordinary users keep
their income rights separately. This avoids gifting seed income to later entrants.
If no live junior remains, new junior-directed *collected* revenue creates funded
protocol junior in a fresh economic generation. Prior earned income still belongs
to its original holders. A later user mints at current value, never claims an
unowned historical rewards balance. Before collection, the amount is not cash or
funded coverage.

Both base and quote insurance balances are asset-specific and cannot fund a
shortfall in the other token without an actual exchange. Junior protection is one
shared value budget; it must not be duplicated across the two debt books.

The income-processing prototype may retain per-period evidence. Bounded work per
instruction does not imply bounded lifetime history or a finished Solana design.
