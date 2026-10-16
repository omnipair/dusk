# PR #45 closeout

Date: 2026-10-16. Runtime source: `5a9abcb342aa3b1afa284b3a9613c4fa0afdebb0`.
This closeout adds a reproducible test, its CI check and updated documentation; program, SDK
runtime, instruction/account interfaces and deployed artifacts are unchanged.

## Current conclusion

**Implementation closeout is documented; an unqualified security/economic signoff
is not established.** #287652's original live-price dependency is mitigated, but
inventory-related trading extraction remains open for explicit disposition.
#287160 has the agreed fee-notice mitigation and keeper implementation; issuer
risk and timely successful keeper execution remain assumptions.

The [35-finding ledger](audit/V12_FINDINGS.md) has 31 remediated root causes,
2 superseded auction findings and those 2 retained-risk items. The export's
other 134 Invalid labels were not independently re-audited one by one.

## Agreed implementation coverage

Later decisions supersede earlier proposals in the conversation.

| Area | Current status and evidence |
| --- | --- |
| Margin model | Stored liquidity-scaled 7/12/17% marginal MM bands; +3-point IM buffer; wallet-independent incremental crowding. About 6x at 30% stored depth before costs/current crowding. No old 2% unwind cap or 50%-TVL hard cap. [Formulas](STORED_LEVERAGE_MARGINS.md). |
| Predictable entry terms | Stored bands/requirements do not passively change with unrelated activity; new risk rechecks them. Price, interest and the position's own size still affect health. |
| Position actions | Debt-asset opening/add-to funding; increase size, decrease size, repay debt as increase margin, proportional remove-from-position; lower leverage/1x; no standalone margin withdrawal. SDK sizing, builders and previews exist. UI zaps remain separate work. |
| Borrowing/leverage liquidation | Symmetric linear EMA eligibility; atomic begin/payment/settle; owner account safety; position locks and fresh shared accounting. Arbitrary external route or buyer funds; internal Dusk payment adapter. [Contract](FLASH_LIQUIDATION_CONTRACT.md). |
| Partial/full permission | Useful partials; MM+2 recovery cap; affordable 0.5%-to-ramp-ceiling discount; no minimum remaining debt or launch-time dust setting. Emergency at 70% MM; full at EMA insolvency or certified absence of a smaller internal settlement reaching recovery. Failed proof/search is not permission. |
| Incentives | Ordinary ceiling 0.5%→5% over 2 minutes; health can accelerate it. Emergency 0.3%→1%; bounded even when insolvent. No mandatory time unlock or fixed insurance cleanup bonus. |
| Fees/insurance/losses | 1% solvent charge on net debt repaid: 0.2% protocol and 0.8% insurance/LP, with target taper. Principal-first recovery; 75% principal-loss coverage bounded by a 50%-per-nominal-day fund budget; unpaid interest canceled; uncovered principal socialized. hLP target allowance retained. |
| Internal emergency settlement | EMA price and smoothed depth with configured concentration/full fees; real collateral and hLP accounting; cash-funded payouts. Depth withdrawal alone does not change healthy EMA eligibility. [Current economic evidence](calibration/EMA_SETTLEMENT_CLOSEOUT.md). |
| LP governance delegation | Reusable market balance, one delegate, creation/support rights, partial support withdrawal before queue for direct/delegated support, owner principal/yield control and revocation. Queued support remains locked. Governance Nemesis review and CI correction completed. |
| Original admission/identity remedies | Freeze authorities and external asset hooks rejected; mutable fees supported; owner-scoped multiple position IDs; canonical atomic LP metadata with ALT launch helpers; owner-owned delegated residuals. [Ledger](audit/V12_FINDINGS.md). |
| SDK / ABI / events | Margin, action, governance, flash/emergency and transfer-fee helpers; regenerated IDLs/types; accounting events documented. No new helper or ABI change is required for this tests/docs closeout. |

## Companion delivery

| PR | Candidate | Verified status | Remaining boundary |
| --- | --- | --- | --- |
| [Dusk keepers #2](https://github.com/omnipair/dusk-keepers/pull/2) | `52c1cec` | Flash/emergency migration complete; protocol capture `5a9abcb3`; repository, Rust and container CI green | Not deployed. Internal Dusk and bounded inventory mode; no automatic Jupiter route discovery. Captured artifacts must match authorized rollout. |
| [Web documentation #97](https://github.com/omnipair/dusk-webapp/pull/97) | `25b986ed` | Pending-#45 docs updated; lint/build/test/visual checks green | Docs adoption is not implementation of all UI flows or indexer/VOB rollout. No production deployment requested here. |

Keeper validation includes 123 Node tests, 39 Rust unit tests, real PostgreSQL
recovery testing, 13 real-program bank fixtures and four native RPC-loop cases.
It persists signed bytes before send, requires matching finalized receipts and
handles incomplete history conservatively. These do not prove universal route
availability or production uptime. Companion results were verified from their
existing evidence/green CI; this closeout does not rerun both repositories.

## Fresh scoped Nemesis review

The closeout reran the full Feynman and State pipelines **within the EMA emergency
change and its shared settlement dependencies**, followed by targeted Feynman
re-interrogation of inventory/hLP/recenter/resource coupling. No new verified
code defect survived that review. This is not a fresh exhaustive review of every
line in #45, nor independent external certification.

Reviewed paths: EMA curve construction; fee reference selection; emergency quote,
partial sizing and full-close proof; preparation binding and one-use application;
fixed/isolated debt removal; principal/insurance/write-off accounting; hLP yield
ownership and reconstruction; actual reserve checkpoint/risk observation; token
credit/payout/custody validation; SDK preview/build reference fields.

The suspected virtual-reserve overwrite, duplicate insurance use, fee ownership
shift, repayment manipulation with unchanged reference inputs, and loss counted
as recenter profit were checked against their complete called paths and tests.
The surviving economic concern is separately retained as #287652. The [tracked review report](audit/EMA_NEMESIS_CLOSEOUT.md) includes the dependency
map, rejected hypotheses and convergence result. Local logs remain in
`.audit/findings/pr45-closeout-20261016/`.

## Validation

The complete mandatory local CI sequence passed on these changes:

- Formatting, tracked-file hygiene, helper shape and required Clippy checks.
- 501 Dusk tests in each default/production profile and production library check;
  16 delegate tests and 1 faucet test.
- TypeScript, full Anchor/SBF program/fixture builds and clean build identity.
- 66 SDK tests and generated-interface consistency checks; vendored interfaces
  remain unchanged.
- 135 strict LiteSVM tests, no pending tests, 71/71 instruction coverage and the
  complete compute baseline required without increasing ceilings.

The full benchmark-feature suite also passes: **536 tests**.
The admitted-position regression reproduces 144 rows and asserts exact
reference/resource-delta invariance; it is now a focused CI step. The current
policy sweep covers 576 combinations. Hosted status for the prior runtime commit
was green; new-head hosted status is a separate check after push.

## Still open

1. **#287652 economic disposition:** accept the quantified residual inventory/
   trading risk, or choose another mitigation and validate it. No program policy
   is silently changed to obtain a closed label.
2. **Operational follow-through:** authorized program/keeper rollout, live target
   validation, and remaining UI/indexer/VOB consumers. Passing tests/docs PRs do
   not establish deployment or compatibility of every downstream consumer.
3. **Coverage limit:** ordinary-eligible positions above the emergency threshold
   can remain unfilled in finite sampling. The current sweep has no emergency-
   eligible residuals, but is not proof of universal or timely cleanup.

No merge, program upgrade, live keeper launch or package publication is performed
by this closeout. No production account migration is assumed.
