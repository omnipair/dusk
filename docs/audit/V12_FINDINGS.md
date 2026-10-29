# Original V12 findings: current disposition

Updated: 2026-10-17. Runtime: `5a9abcb342aa3b1afa284b3a9613c4fa0afdebb0`.

The original export contains **169 reports: 134 labeled Invalid and 35 labeled
Unreviewed**. Those were source labels, not 35 established vulnerabilities.
The prior review traced the 35 unresolved subjects; this closeout reconciles the
subsequent fixes and remaining risks. The 134 Invalid reports were inventoried,
**not independently re-audited one by one**.

Current disposition: **31 remediated, 2 superseded, 1 mitigated with retained
issuer/keeper risk (#287160), and 1 closed after remediation with accepted residual
economic risk (#287652)**. The user explicitly accepted the latter on 2026-10-17.
Remediated refers to the reported root cause, not proof of subsystem safety.
Historical source line numbers below are locators from the earlier review; use
function/test names when later edits move lines.

| ID | Original title | Current disposition | Current evidence / limits |
|---|---|---|---|
| #287137 | Default Owner Accepted in Replay | Remediated | Native replay rejects default owner/market at initialize, decode and checkpoint; `benchmark_api.rs`, test `borrow_position_replay_rejects_default_owner_and_market`. Off-chain API, not an on-chain signer exploit. |
| #287140 | Benchmark repay omits referral accrual account mutation | Remediated | `BenchmarkMarket::execute_repay` clones market, position and referral state; commits referral only after successful position execution (`benchmark_api.rs:3289`). |
| #287160 | Zero-credit collateral bypasses AMM repayment | Mitigated; issuer/keeper risk retained | New-risk admission uses adverse announced fees; the final notice epoch permits early liquidation using current transfer fees. SDK notice helpers and keeper PR #2 are implemented. Confirmation/route availability and hostile issuer risk remain; no deployment is claimed. |
| #287161 | Clearing Cache Leaves Market Permanently Unquotable | Remediated | Zero live reserve branch resets `amm.initialized = false` in `transitions/amm/mod.rs:352`; subsequent initialization does not reuse a zero curve cache. |
| #287171 | Delegated close can revert after Dusk settlement due to unbudgeted fees | Remediated | Delegated fee split is bounded inside Dusk and paid from realized residual. Common owner account validation plus net owner minimum; `instructions/leverage/close_leverage.rs` and delegated fee tests. |
| #287193 | Dust recovery overpays terminal caller | Remediated | hLP terminal caller reward uses floor of interest × bps, not a minimum/ceil that consumes all dust interest; `instructions/spot/close_insolvent_hlp.rs:192`. |
| #287199 | Swap fee growth uses reconstructed hLP ownership | Remediated | AMM applies fee growth using captured hLP ownership before reconstruction (`transitions/amm/swap.rs:226`); eligibility is captured in prepare. Concentrated hLP yield regressions cover ownership conservation. |
| #287243 | Freezable Collateral Vault Locks Positions | Remediated | Shared `require_supported_asset_mint` rejects any freeze authority (`instructions/accounts.rs:151`); legacy and Token-2022 SVM rejection tests. |
| #287245 | Overstated max amount blocks valid rescue repayments | Remediated | Removed the early wallet-versus-maximum check in `add_leverage_margin`; validate the exact gross debit. Both repayment aliases, insufficient funds and Token-2022 are covered by LiteSVM. |
| #287266 | Metadata Ignores Zero-Supply Lifecycle | Remediated | All three LP metadata records are created atomically in market initialization (`instructions/market/initialization.rs:443`). SDK launches use ALT preparation; caller-supplied symbols are intentional, as selected. |
| #287290 | Large borrow previews exceed bounded search | Remediated | Capacity searches have an explicit large-domain step bound and conservative lower-bound result (`transitions/lending/preview.rs:6`). High raw-unit preview SVM coverage exists; conservative sizing can underquote capacity. |
| #287309 | Protected recenter surcharge can strand if governance disables concentration | Remediated | Governance releases protected recenter reserves when the controller is disabled or curve becomes CPMM (`transitions/governance.rs:780`). |
| #287501 | Opening Premium Rounds Down to Zero | Superseded | Old opening-auction premium path is no longer an on-chain entrypoint. Immediate flash pricing uses current reference and explicit discount. Retained native benchmark auction adapters do not reactivate it. |
| #287573 | Leverage flows accept freezable or hook-controlled asset mints | Remediated | Duplicate asset-freeze/extension exposure: shared asset validator rejects freeze authorities and external asset hooks. LP yield hooks remain intentionally supported on LP tokens. |
| #287583 | Debt Transfer Fees Cut Liquidator Rewards | Remediated | New liquidation settlement separates net keeper credit from gross token debit; emergency reward and fixed flash payment gross-ups include token fees. See `amm_liquidation.rs`, `flash_liquidation.rs`, debt-transfer-fee SVM cases. |
| #287621 | Concentration governance updates bypass the documented ramp | Remediated | Program and SDK docs describe immediate concentration changes at validated governance execution; no nonexistent concentration ramp is promised. |
| #287623 | Untrusted asset transfer hooks can permanently freeze custody flows | Remediated | TransferHook removed from supported underlying asset extensions (`token.rs:258`), validated at admission. Does not remove protocol-owned LP hooks. |
| #287627 | Preview branch field leaks incompatible internal encoding | Remediated | Preview documents all five branch codes: tail/shoulder/inner/shoulder/tail (`instructions/preview.rs:352`). SDK quote interpretation includes shoulders. |
| #287630 | Token-2022 fees shortchange order payouts and refunds | Remediated | Entry cancellation, executor rewards and hLP exit orders enforce net receipt/gross fee budget; `leverage_delegate/instructions/fees.rs` and Token-2022 cancellation/entry/hLP SVM tests. |
| #287631 | Stale delegations and orders survive position recreation | Remediated | Orders and delegation bind `open_curve_revision`; recreated PDA must have a matching new lifecycle (`leverage_order.rs:35`, delegation identity/generation checks). |
| #287632 | Unreserved leverage position IDs can be front-run | Remediated | Leverage seeds include market + owner + namespace + position ID (`instructions/leverage/settlement.rs:101`). Owner-scoped multiple positions remain supported. |
| #287633 | Borrow position IDs can be squatted by front-running | Remediated | Borrow position seeds include signing owner plus market and position ID. SVM test `binds borrow positions to the signing owner while allowing multiple position IDs`. |
| #287643 | Empty markets cause the public preview to revert | Remediated | Empty/unseeded market price preview returns explicit zero; `unseeded_market_has_a_zero_spot_preview` regression. |
| #287644 | New-position borrow capacity omits debt-share rounding | Remediated | Borrow capacity uses executable share rounding and projected debt; `new_position_preview_uses_executable_debt_share_rounding` plus existing-position boundary/property tests. |
| #287648 | Liquidation auctions execute against an unbounded stale price snapshot | Superseded | Auction-opening reference is no longer reused for a later fill. Each atomic flash begin uses fresh current risk and binds a same-transaction settlement quote. |
| #287650 | Delegated leverage closes fail during the launch window | Remediated | Close common validation calls launch same-transaction guard; preparation discriminator is included in launch guard handling. Both owner and delegated paths pass through common validation. |
| #287652 | Backstop collateral swap has no execution-price bound | Closed; original dependency remediated, residual risk accepted | EMA price/smoothed depth replace live spot/depth settlement for borrowing, leverage and native terminal hLP recovery. Full configured fees and concentration remain. The admitted-position regressions preserve reference sizing across same-slot spot changes. The user accepted remaining inventory/arbitrage risk on 2026-10-17; see the economic closeout for scope, loss allocation and V1 comparison limits. |
| #287654 | Uncapped divergence fees can be reduced by splitting swaps | Remediated | Current divergence charge is a Huberized additive potential with marginal cap, including production build. Native split-path/telescoping and marginal-cap tests pass; `transitions/amm/fees.rs`. |
| #287656 | Delegated closes can redirect residual payouts | Remediated | `CloseLeverage::validate_common` requires residual token-account owner = position owner (`close_leverage.rs:233`); Dusk pays bounded treasury/executor fees then owner directly. Delegated order custody is not residual authority. |
| #287657 | TP/SL trigger price omits mint-decimal normalization | Remediated | Delegated closeout trigger explicitly normalizes debt/collateral decimals (`leverage_delegate/instructions/leverage/before_close.rs:3`). Shared entry limit similarly normalizes quote/base decimals. |
| #287659 | Extreme prices can block hLP checkpoints | Remediated | hLP terminal valuation uses the forward reserve ratio in wide arithmetic rather than a truncated reciprocal (`liquidity/hlp/engine.rs:569`); extreme reciprocal regression passes. |
| #287660 | Frequent checkpoints permanently discard borrow interest | Remediated | Borrow-index fractional numerator carry persists between accruals (`transitions/lending/mod.rs:1166`), separately for each asset; small frequent accrual no longer discards all fractional growth. |
| #287661 | Stale accrual permanently discards interest beyond one year | Remediated | Accrual uses the full elapsed slot-derived interval, with checked arithmetic; `long_slot_gap_charges_the_full_elapsed_interval` passes. This does not promise success at physically unreachable u64 time horizons. |
| #287667 | Transfer-fee sold mints underdeliver auctioned revenue | Remediated | Protocol revenue auction prices actual net sold-token delivery and verifies observed receiver credit (`instructions/futarchy/settle_protocol_auction.rs:208`). Gross custody/liability retired separately. |
| #287670 | Delegated close callbacks forward fixed-account signer privileges | Remediated | Every delegated callback account meta has `is_signer: false`, with protected write restrictions and duplicate rejection (`instructions/leverage/settlement.rs:309`); signer-stripping regression passes. |


## Closure boundaries

- [#287652 current economic evidence](../calibration/EMA_SETTLEMENT_CLOSEOUT.md):
  the original live-price dependency is removed; post-settlement inventory and
  arbitrage risk was explicitly accepted on 2026-10-17. This is a scoped closure,
  not a claim of zero loss or identical V1 economics.
- [#287160 current fee/keeper policy](../mutable-transfer-fees.md): implemented
  prevention and accounting do not remove mutable-mint authority risk. No new
  fee-authority revocation requirement has been adopted.
- NM-001 through NM-005 from the later review are remediated; see
  [feasibility remediation](../calibration/LIQUIDATION_FEASIBILITY_REMEDIATION.md).
- [Current closeout](../AUDIT_CLOSEOUT.md) records scope, companion PRs and the
  remaining integration/rollout work. No original historical PoC was executed.
