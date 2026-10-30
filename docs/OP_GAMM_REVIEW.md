# Verified implementation review

This is author verification of the bounded OP-GAMM changes, not an independent security audit. No open vulnerability severity is assigned from an unverified hypothesis.

## Confirmed and corrected

- **Frozen hLP ownership reconstruction:** double-counted frozen inventory and inconsistent static-supply reconstruction could create repeatable native-cycle gains. Fixed in `transitions/amm/mod.rs` and `transitions/liquidity/hlp/engine.rs`; regression spans both directions, decimal scales, shapes and frozen masks.
- **Carried interest reconstruction:** endpoint net NAV and backing require distinct treatment. Added explicit carry cover and certification of the exact rounded endpoint; repeated carry/materialization and fee/frozen cycle tests pass.
- **OP credit admission:** movement across native invariant scales must not create new recognized credit depth. `checkpoint_oracle_risk_depth` enforces the ceiling, exercised by both-direction and split exit-loan tests.
- **Transaction feasibility:** duplicate quote computation and unreclaimed temporary allocations caused concrete SBF failures. The authenticated callback context, value-only output path, exact integer optimizations and SPL-equivalent allocation changes resolve the exercised full/partial and Token-2022 routes without increasing test budgets.

## Boundaries verified

Source binding is per invocation; deserialization drops authority. Pyth requires full verification and both assets. New-source governance validates the replacement while retaining claims and debt; the old dead source is not required. Stale direct debt repayment does not authorize new risk. Fixed callback account identities cannot change while an approved quote is pending; the pending context is cleared before settlement and rollback removes it on failure.

## Remaining design limitations

Wrong but authenticated reports can cause economic losses. λ/source changes can transfer value through repricing. A feed read does not rehedge an idle hLP. Fixed-point/nonlinear work limits can reject transactions. Recognized credit depth does not yet grow from compounded fees without new shares. These are disclosed in [implementation status](OP_GAMM_IMPLEMENTATION_STATUS.md); finite tests do not establish universal solvency or replace an independent audit.
