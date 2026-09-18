# Native hLP deposit admission preview

`preview_hlp_deposit_capacity` is a permissionless, read-only instruction. It returns the selected vault identity, bank slot/epoch, explicit admission status, and gross/net funding ceilings. SDK 2.9.0 exports `decodePreviewHlpDepositCapacityReturnData`.

## META diagnosis

An unsigned devnet snapshot of META/USDC market `7Rjrf8i81hZihsuFdPfzTaP6SiQ3JEmQs7Hfg7YNjkNm` at slot 500291305 reproduced the deployed admission guard. META had 10,000,000 live hLP shares and residual exposure -57293: its hedge was actionable and required rebalancing. Opposite-asset funding headroom was 55,791,833,567 raw USDC, so this was not insufficient wallet balance or funding capacity. The empty USDC vault was settled and accepted an ordinary native deposit.

The preview does not rebalance the vault, relax entry guards, change deposit/withdraw accounting, or execute a keeper operation. It makes the existing restriction available before the user requests a deposit quote.

## Semantics

- Every account is read-only. Accrual, partial-burn reconciliation, clock advancement and hLP checkpoints affect only deserialized memory.
- Authority PDAs, supported asset mints, hLP mint identity/authority/decimals, market version and live supply retain native validation.
- `Ready` includes settled and controller-granularity-limited entry states admitted by the existing program. Other statuses distinguish not-started, reduce-only, missing liquidity, actionable hedge, cash-constrained hedge, unhedgeable vault, settlement-price divergence, no funding, and transfer fees consuming the deposit.
- Funding headroom uses the native rounded debt-share boundary. The opposite curve reserve ratio converts it to the greatest permitted net target credit. Token-2022 fees at the same epoch convert that to the greatest gross wallet debit, including inverse-fee plateaus, fee caps and 100% fees.
- A blocked status returns zero effective admission limits. A funding ceiling is **not** an executable-amount quote: tiny amounts, post-entry settlement, reserve/mint overflow, custody, balances and slippage still require the real deposit instruction. The frontend must retain unsigned amount simulation and full write-time deployment validation.

## Validation and rollout

All required local CI gates passed: formatting, repository hygiene, code shape, Clippy, 393 Dusk tests in each default/production profile, production check, 15 delegate tests, one faucet test, TypeScript, all SBF builds, generated SDK/IDL checks, clean-build identity, and 74 LiteSVM tests with mandatory complete CU coverage and no pending tests. The new LiteSVM case executes the preview for both assets and confirms that the submitted instruction leaves market bytes unchanged. Native regression tests replay the public snapshot and verify the exact funding boundary and transfer-fee rounding.

No devnet upgrade or signed live transaction was performed for this change. Rollout requires a reviewed Dusk upgrade and matching API deployment envelope/IDL identity before the dependent frontend can enable this preview. The leverage delegate interface and account layouts are unchanged.
