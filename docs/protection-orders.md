# Repayment donations and LP-funded protection

Anyone may repay an existing borrow or leverage position with tokens from an account they own. `repay.owner`, `repay_leverage.owner`, and `add_leverage_margin.owner` identify the **payer**. The position owner stays unchanged; events report the borrower as `owner` and the payer as `metadata.signer`. Repayment grants no withdrawal, delegation, or collateral rights. The supplied amount is the payer's maximum gross debit, including transfer fees. Dusk only transfers the amount needed for the debt shares it can safely burn; excess remains with the payer. Dust below a repayable share is rejected.

`donate_collateral` adds either supported market asset to an existing borrow position without changing its owner, referral binding, or stored liquidation terms. Creation still uses the owner-authorized `deposit_collateral` instruction. Donations are irrevocable. The owner can sweep both collateral balances and close a debt-free position in `withdraw_all_collateral`, including dust donated immediately before that transaction. The donor cannot force a position open after closure.

For leverage, `repay_leverage` (and the existing `add_leverage_margin`) is a debt repayment: the collateral quantity and market exposure remain in the position. It now allows partial rescue while still liquidatable and complete repayment. After complete repayment, only the position owner can use `withdraw_repaid_leverage` to receive the collateral and close the position. `repay_leverage` skips the collateral-sale quote and emits `closeout_value: 0` to indicate it is unquoted; `add_leverage_margin` also omits that quote after full repayment. Neither operation sells collateral. Selling collateral to reduce exposure continues to use the existing decrease/close instructions.

## Sponsor authorization

`preview_protection_order` returns the current health as a u64, using the same refreshed accounting as execution; it does not change state.

The leverage-delegate program provides `create_protection_order`, `fund_protection_order`, `execute_protection_order`, and `cancel_protection_order`. One order covers one existing position, one debt side, and one action:

| Action | Keeper supplies | Result |
| --- | --- | --- |
| 0: borrow repayment | Debt asset | Fewer fixed-debt shares |
| 1: borrow collateral | Opposite collateral asset | More collateral, same debt |
| 2: leverage repayment | Debt asset | Fewer isolated-debt shares, unchanged collateral |

The sponsor may differ from the borrower. The order binds the market, position address, and position owner. It explicitly authorizes **recurring protection, including later borrowing and reopening at that address by the same owner**, until expiry, cancellation, or budget exhaustion. It does not authorize a new owner at that address. Sponsors should use a separate order and budget for each beneficiary and choose an expiry accordingly.

The sponsor escrows yLP or hLP from the protected market. hLP must target the payment asset. The order includes the total funded LP budget, maximum LP burn and payment per execution, trigger and target health, keeper reward in basis points, and `min_payment_per_lp_nad`. That last limit is a minimum ratio of actual gross payment to LP burned, multiplied by 1e9, using the raw units of both tokens. It prevents excessive redemption or poor execution relative to the payment made. It must be nonzero. Keeper rewards are rounded down and capped by the sponsor's chosen fee (at most 10%).

LP transfers need the native Dusk transfer-hook accounts and an initialized LP transfer-hook validation account. Initialize the order's LP custody token account and both yield accounts before creating the order. The custody token account must have no external delegate or close authority; use a newly created associated token account. Create and fund transfer only the authorized amount. Unsolicited extra LP does not increase the executable budget; cancellation refunds the entire custody balance.

Both yield accounts designate the sponsor as recipient. Escrowed LP retains normal yield accrual. The sponsor can harvest via the existing `harvest` instruction using the order as LP owner and the sponsor as caller/recipient. Cancellation returns remaining LP and leaves the order and yield accounts available for claiming earned yield.

## Atomic keeper execution

1. Refresh market accounting in a read-only copy, bind the target, and check expiry, budget, and trigger.
2. Repay or add collateral using the keeper's own payment token account. Measure its actual gross debit.
3. Redeem the authorized LP through native Dusk withdrawal. All existing market-health, reserve, and hLP solvency checks remain active.
4. Reimburse the keeper's debit plus the authorized proportional reward, and pay the additional 10 bps protocol fee from sponsor redemption proceeds. Account for transfer fees and verify both credits. Redemption must cover both payments within the sponsor's existing LP limits.
5. Return unused redemption proceeds to the sponsor. For yLP, this includes the other asset; no swap or external route is used.
6. Reload the market and position and require final health to exceed its starting value and reach the sponsor's target. Deduct the LP burned from the budget.

All steps are one instruction. Failure, including unavailable withdrawal liquidity, a competing cancellation/liquidation, a changed target, or inadequate final health, rolls back repayment, redemption, reward, and protocol fee. The keeper needs working capital to advance the payment. The SDK supplies typed instruction builders; callers must simulate the complete transaction, include required transfer-hook accounts, and use address lookup tables or a requested heap frame when needed.

Borrow health is liquidation capacity divided by debt in basis points, using the **linear valuation used by borrow liquidation**, with 10000 as the boundary. Leverage health uses executable collateral closeout value and the maintenance buffer, with one additional basis point of conservatism for integer rounding. Both return the maximum u64 for zero debt. Trigger must exceed 10000 and target must exceed trigger. These checks use Dusk's market state; they do not guarantee execution before liquidation during a price jump or a liquidity shortage.

Ordinary donations have no order budget or keeper reward. Order limits control the sponsor's escrow authorization, not the public repayment capability.


## Protocol order fee

Every successful built-in voluntary order execution pays a hard-coded **10 bps (0.10%)** protocol service fee. This is an additional owner/sponsor expense; it does not reduce the pool LPs' revenue allocation or the executor's existing incentive. Native liquidations, hLP rescue, direct repayments/collateral donations, order creation/cancellation, funding, and yield harvesting do not incur this service fee. Ordinary native trading/interest fees still apply.

| Order | Fee value, denominated in the payment token | Owner/sponsor funding |
| --- | --- | --- |
| Leverage entry | Actual opened margin plus borrowed principal | `protocol_fee_deposit_amount` is an additional gross deposit into the funding vault, separate from `deposit_amount`. Execution preserves the recorded margin and executor bounty, then refunds unused fee funding. |
| Leverage take-profit / stop-loss | Executed collateral-sale output before debt repayment | Owner's close proceeds pay the fee in addition to the unchanged executor incentive. An insufficient residual reverts; native liquidation remains available. |
| hLP stop-loss / stop-rate | Actual withdrawal proceeds before executor incentive | Owner's withdrawal proceeds pay the fee; the existing minimum output check applies after both fees and token transfer costs. |
| Borrow/leverage protection | Actual net repayment or collateral donation, excluding token transfer fees | Sponsor LP redemption covers the payment, full keeper reimbursement/reward, and fee. Insufficient output or LP budget reverts atomically. |

The fee is rounded up to the smallest raw token unit; zero execution value has zero fee. Token-2022 fees are grossed up on the protocol transfer so the treasury receives the full quoted fee. Any additional gross debit is also the owner's/sponsor's cost. Transfer-hook accounts must support the additional treasury transfer.

Execution requires `protocol_fee.fee_recipient`, a token account owned by the canonical Dusk authority's `recipients.futarchy_treasury` and matching the payment mint. There is no administrator-settable order fee rate. `OrderProtocolFeePaid` reports the order, owner, mint, executed value, fee, gross debit, and actual treasury credit. Collection bypasses neither repayment accounting nor native market-solvency checks; this additional service revenue is sent directly to the configured futarchy treasury.

The SDK exports `ORDER_PROTOCOL_FEE_BPS`, `calculateOrderProtocolFee`, and `orderProtocolFeeAccounts`. Pass the latter's result as `protocolFee` in execution accounts, and initialize its treasury ATA if necessary. Quotes must include all native fees, token transfer fees, the keeper incentive, and this surcharge; for protection, choose the LP amount within the sponsor's stored limits.
