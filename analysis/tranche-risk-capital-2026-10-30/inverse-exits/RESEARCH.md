# Inverse withdrawal code and Dusk exit design

Research date: 31 October 2026. These are source findings and candidate design changes, not an implemented Dusk feature.

Subsequent design correction: reject the proposed combination of no claim expiry, continued junior income, and holder-discretionary finalization after maturity. It creates a permanent ready-to-exit option and rewards pre-queueing immediately after entry. The Inverse source findings below still stand; the no-expiry finalization candidate below is retained only as the rejected reasoning trail. Reconsider a bounded claim window or scheduled economic conversion with separate later receipt delivery. The latter needs a demonstrated valuation and settlement-ordering design, not an assumption that a timestamp executes a transaction.

## Sources checked

- [Verified withdrawal escrow source at the documented v2 deployment](https://etherscan.io/address/0x8554d8a6Bcc5B6D6Eb7bEA2189e6a8F8d24c7e45#code). Its extracted source matches the repository's withdrawal-pause revision after trimming terminal whitespace. SHA-256 of extracted source: `2cf9634d5a931929f377a495bf92f1de29ce2147c566a3fcc3e9af625c13f070`.
- [WithdrawalEscrow.sol, pinned revision](https://github.com/InverseFinance/JuniorDola/blob/727bb0877ae8067cbe49bdea8964dba67d2f1b40/src/WithdrawalEscrow.sol).
- [LinearInterpolationDelayModel.sol, same revision](https://github.com/InverseFinance/JuniorDola/blob/727bb0877ae8067cbe49bdea8964dba67d2f1b40/src/LinearInterpolationDelayModel.sol).
- [Withdrawal tests, same revision](https://github.com/InverseFinance/JuniorDola/blob/727bb0877ae8067cbe49bdea8964dba67d2f1b40/test/WithdrawalEscrow.t.sol).
- [Relaunch proposal and initial parameters](https://forum.inverse.finance/t/proposal-to-relaunch-jrdola-with-initial-dbr-reward-budget/666).

The repository main branch predates the withdrawal-pause change. The escrow source comparison is against the newer pause revision. Delay examples use the published relaunch configuration; live getters were not read. Tests were inspected, not executed.

## Actual queue behavior

`queueWithdrawal` passes total junior share supply and the escrow's current junior balance plus the new request to the delay model. It transfers junior shares to escrow, records the net shares after the queue fee, and stores start/end timestamps. The model linearly interpolates between minimum and maximum delay according to queued shares relative to the configured threshold. The escrow also clamps model output to its own 1-to-60-day bounds and uses 60 days if the model call fails.

With the relaunch's published 1-day minimum, 10-day maximum and 100% threshold, illustrative delays are 1.9 days for 10% queued, 5.5 days for 50%, and 10 days for 100%. The request itself is included in crowding. The delay is calculated before the request's fee is burned, so the examples deliberately omit fee-related supply changes.

This is a delayed-redemption mechanism, not a FIFO flow limiter. One request for half of supply can redeem its entire remaining balance once eligible. Splitting an order among accounts can let some pieces mature earlier; `delay-examples.json` reproduces the arithmetic. A later increase in crowding does not extend unrelated existing requests. Adding to the same account's request reschedules that account's aggregate shares, without moving an existing future start earlier.

## Expiry and cancellation corrections

Time passing does not transfer or redeem shares. They remain junior shares in escrow, with their vault exposure intact. `completeWithdraw` requires the current timestamp to be within the saved start/end window and redeems all that account's queued shares. After expiry it reverts.

The user can renew with `queueWithdrawal(0, ...)`, obtaining a new window and potentially another fee, or call `cancelWithdrawal` to return junior shares to their wallet. Cancellation is blocked before the saved window start, and allowed after that start, including after expiry. Contrary to the earlier conversational summary, there is no automatic senior conversion or automatic transfer back to the wallet. Expired shares still contribute to the escrow balance used for crowding until an operation moves them.

The pause revision blocks new queueing and completion while paused. It does not shift saved window timestamps. The test `test_completeWithdraw_windowExpiresDuringPause` explicitly expects an expired-window revert after the pause has outlasted a request's window. Governance can renew its bounded pause through further transactions.

## Dusk implications

Immediately removing all queued junior capital from the rate numerator while it still earns junior income creates a strategic incentive: a large junior holder can announce departure, induce higher rates if demand persists, collect its share during notice, and later cancel or cycle back into junior. A non-cancellable request makes the commitment stronger but does not prove profitability is impossible, since re-entry may remain cheap. Inverse's queue is not evidence that this extra feedback into Dusk borrow pricing is safe.

A simpler candidate prices actual, current loss-bearing junior value, including escrowed junior until conversion. Queue size affects notice scheduling and is displayed to users, but does not directly subtract from pricing coverage. Rates respond prospectively when conversion actually removes protection. This sacrifices advance automatic repricing and still needs tests for ordinary capital concentration and entry/exit cycling.

For token mechanics, escrow jrLP and record pending junior share units. Keep them in junior losses and income until finalization. Once eligible, a transaction settles accounting, burns the escrowed jrLP and issues the appropriate srLP value atomically. A permissionless finalizer can send only to the request's recorded beneficiary; users need a direct fallback, and keeper availability/funding must be specified. No expiring claim window is required for that design.

Burning jrLP at request time is also possible only if the pending record preserves the same junior units, losses and reward entitlement. A fixed srLP promise at request time would remove first-loss exposure prematurely. Escrow is easier to explain and avoids a separate shadow share supply.

Timeout means eligibility, not an autonomous transaction. In the simpler candidate, junior exposure ends at successful finalization, not merely at the timestamp. Exact economic conversion at a past deadline requires additional maturity accounting and defined valuation/ordering rules; it cannot be achieved just by minting later at the current price.

If the product must spread large departures rather than only delay them, add a global share/volume release schedule with partial conversions across batches. Per-wallet limits are insufficient. That is an additional policy and calibration choice, not a property provided by Inverse's linear-delay formula. It is not yet selected.
