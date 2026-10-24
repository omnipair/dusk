# Mutable Token-2022 transfer fees (#287160)

## Implemented mitigation and remaining assumption

Dusk supports assets whose issuer can change the transfer fee. **Revoking that
fee authority is not a required or agreed remediation.** The mitigation combines
fee-aware admission, early liquidation during the final notice epoch, actual
net-receipt settlement, and an explicit loss waterfall.

Early liquidation depends on a keeper finding an executable, worthwhile fill
and getting it confirmed before activation. This is a substantive mitigation,
not a guarantee against issuer confiscation. Scheduling a fee does not itself
execute a liquidation. The SDK and keeper monitoring support this policy; the
keeper deployment must use the matching Dusk interface and artifacts.

## Three different fee calculations

Example: current fee 3%, newly announced fee 100%, with no binding cap, scheduled
for epoch 102. A transfer debits 1,000 token atoms. Table amounts are net credit.

| Epoch | Actual transfer | New-risk admission | Existing-position liquidation health |
| --- | ---: | ---: | ---: |
| 100: first notice | 970 | 0 | 970 |
| 101: final notice | 970 | 0 | 0 |
| 102: fee active | 0 | 0 | 0 |

New-risk checks consider **all announced future schedules** immediately. Existing
positions' liquidation checks include the announced schedule only when it becomes
effective next epoch. A pending schedule can therefore make a position eligible
while its collateral can still be transferred under the current fee. Healthy
positions must still pass the program's eligibility checks; notice status alone
never authorizes liquidation.

For each transfer amount, Dusk computes the exact fee under each applicable
schedule, including ceiling rounding and that schedule's maximum fee, then uses
the smaller net credit. It does **not** combine the largest rate with the largest
cap. A pending fee reduction cannot relax conservative health before activation.
Actual transfers and payment gross-ups always use the effective fee.

Dusk reads the mint schedules during execution; they are not cached in `Market`.
Borrowing and liquidity-withdrawal checks consider both market mints. Stored
market and position contributions remain gross and are discounted at valuation.

## SDK and keeper contract

Use `fetchTransferFeeNotices(connection, mints)` for a coherent mint + Clock read,
or `liquidations.transferFeeNotices(...)` on a `DuskLiquidations` instance for a
position's debt/collateral pair.
`transferFeeCredit` exposes the effective, admission and liquidation calculations.
These are monitoring helpers, not substitutes for on-chain position previews.

1. Discover every supported market and both mints. Poll Clock and mint accounts;
   an epoch transition can change eligibility **without a mint account update**.
2. Re-evaluate affected borrowing and leverage positions when a schedule changes,
   when entering the final notice epoch, and at activation. Keep normal periodic
   scans so restarts or missed notifications cannot lose work.
3. Refresh direct RPC state and on-chain previews before simulation/signing.
   Rebuild if the epoch, fee schedule, authority, payment or route has changed.
   Do not retain a healthy result across a fee/epoch change. Cancellation or
   rescheduling must trigger the same recheck and can make a position healthy.
4. Use current fees for the collateral receipt and all debt-payment gross-ups.
   Confirm the transaction and re-read remaining debt. A preview, submitted
   signature, or observation/timer transaction alone is not successful cleanup.
5. Alert on failed scans, final-notice backlog, repeated route/economic failures,
   signer/confirmation failure, or activation with exposed debt still remaining.
   Never bypass liquidation health, reward limits or insurance limits to clear
   the alert. Epochs, rather than an assumed number of wall-clock hours, define
   the notice window.

The keeper repository runbook is
[`docs/transfer-fee-notice.md`](https://github.com/omnipair/dusk-keepers/blob/feat/mutable-fee-liquidation-callers/docs/transfer-fee-notice.md).
[Keeper PR #2](https://github.com/omnipair/dusk-keepers/pull/2) at `52c1cec`
implements #45's observe/flash/emergency interface and captures protocol artifacts
from `5a9abcb3`. Native, RPC-loop, receipt-recovery and container checks pass.
It supports internal Dusk settlement and bounded inventory purchases; automatic
Jupiter route discovery is not implemented. This candidate is not deployed.
An older auction caller does not gain this coverage from the documentation
update; live coverage requires a separately authorized, validated rollout. Re-pin artifacts if program/interface code changes.

## Residual risks and evidence

A fee authority can schedule and cancel a change after an early liquidation.
Borrowers of mutable-fee assets bear the resulting liquidation-penalty risk.
A fee can activate before cleanup because of unavailable/unprofitable routes,
insufficient keeper coverage, congestion or confirmation delays. At that point,
zero-credit settlement follows principal-first recovery, principal-only bounded
insurance, unpaid-interest cancellation and remaining principal write-off.
These rules account for a loss; they do not prevent the issuer from causing it.

Market-wide health applies a conservative fee rate to aggregate contributions.
It cannot reconstruct per-position fee caps from those aggregates. A 100% rate
capped at one atom can therefore restrict new borrowing and liquidity withdrawal
more than exact position valuation would. Position previews and actual transfers
use exact fee caps. This aggregate limitation remains separate from keeper notice
handling.

Evidence includes SDK/Rust/keeper-TypeScript shared schedule vectors (first notice,
final notice, activation, cancellation/rescheduling, reductions, caps and u64
bounds) and the program's LiteSVM regression:
`liquidates leverage in the final notice epoch before a confiscatory fee activates`.
The separate `socializes leverage debt when a mutable fee leaves zero liquidation
credit` test covers missed-window loss accounting. These demonstrate the paths;
they do not establish production keeper availability or universal cleanup.
