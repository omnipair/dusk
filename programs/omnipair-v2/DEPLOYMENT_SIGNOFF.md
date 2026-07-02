# Omnipair V2 Deployment Signoff

This document defines the evidence required before the `Deployment/Squads`,
`Release rehearsal`, and `Post-deploy smoke tests` rows in
`SIGNOFF_CHECKLIST.md` can be marked `Approved`. It is a deployment-readiness
template, not approval to publish or deploy Dusk.

Use this document only for the exact release commit, release tag, program ID,
verifiable binary, IDL/types, and target cluster being approved.

## Release Artifact Evidence

| Artifact | Required evidence |
| --- | --- |
| Release commit | Final commit hash and PR link. |
| Release tag | GitHub release tag or release-candidate identifier. |
| Program ID | `declare_id!` value from `programs/omnipair-v2/src/lib.rs`. |
| Verifiable binary | `target/verifiable/omnipair_v2.so` hash and embedded `GIT_REV`/`GIT_RELEASE` evidence. |
| IDL | `target/idl/omnipair_v2.json` hash and matching committed `packages/program-interface/src/idl_v2.json`. |
| Types | `target/types/omnipair_v2.ts` hash and matching committed `packages/program-interface/src/types_v2.ts`. |
| Interface package | `npm run check-idl-current --prefix packages/program-interface` result. |
| Verification | `solana-verify` command, output, and registry submission link if available. |

## Release Window Gates

Record owner approval before toggling any repository variable.

| Gate | Required state before release window | Required state after release window |
| --- | --- | --- |
| `DUSK_RELEASES_ENABLED` | `false` or unset until owner signoff is complete | Returned to `false` or unset after release artifacts are published |
| `DUSK_MAINNET_BUFFER_DEPLOYS_ENABLED` | `false` or unset until signed mainnet buffer window | Returned to `false` or unset after buffer address and Squads transfer are recorded |
| `SQUADS_VAULT_ADDRESS` | Configured and reviewed before mainnet buffer deployment | Recorded in release evidence |
| `DEPLOYER_KEYPAIR` | Present only as a GitHub secret, never committed or printed | Rotated or re-confirmed according to owner policy |
| Mainnet buffer inputs | `source=release`, explicit `release_tag`, `transfer_to_squads=true` | Recorded with workflow run link |

## Rehearsal Requirements

Run rehearsal against the intended cluster class before production approval.
For mainnet, the rehearsal may use a release-candidate artifact, devnet, or a
controlled dry run, but skipped mainnet-only steps must be explicitly recorded.

| Rehearsal step | Evidence |
| --- | --- |
| `release-build` verify-only workflow | Workflow URL and successful run ID. |
| Release artifact generation | Artifact names, hashes, and retention location. |
| Blocked-release guard | Evidence that release publishing stays blocked while `DUSK_RELEASES_ENABLED` is not approved. |
| Blocked-mainnet-buffer guard | Evidence that mainnet buffer deploy stays blocked while `DUSK_MAINNET_BUFFER_DEPLOYS_ENABLED` is not approved. |
| Buffer deployment | Workflow URL, network, source, release tag, priority fee, and buffer address. |
| Squads transfer | Transaction signature transferring buffer authority to the configured Squads vault. |
| Squads proposal | Proposal link, approvers, execution transaction, and final program authority. |
| Rollback path | Owner-approved rollback or reduce-only plan and expected transaction path. |
| App/SDK/indexer cutover | Links to integration release or staging confirmation. |

## Post-Deploy Smoke Matrix

Record target-cluster transaction signatures, market IDs, mints, and expected
state deltas. If a smoke step is intentionally skipped, mark it `N/A` with an
owner-approved reason.

| Smoke step | Required evidence |
| --- | --- |
| Program binary verification | `solana-verify` output and program data hash. |
| Market initialization | Transaction signature, market PDA, LP mint metadata signatures. |
| Add liquidity | Transaction signature, yLP mint amount, reserve deltas. |
| Remove liquidity | Transaction signature, yLP burn amount, cash-constrained outputs. |
| Swap | Transaction signature, `min_asset_out`, reserve deltas, fee credit. |
| Deposit collateral | Transaction signature, position PDA, collateral vault delta. |
| Borrow | Transaction signature, debt side, daily borrow bucket, health after borrow. |
| Repay | Transaction signature, principal/interest split, reserve and interest vault deltas. |
| Withdraw idle collateral | Transaction signature and remaining health. |
| Healthy liquidation rejection | Failed transaction or simulation output with expected error. |
| Unhealthy liquidation | Controlled market transaction, close factor, collateral seizure, insurance/socialization deltas. |
| Insurance-backed liquidation | Transaction signature and insurance vault movement. |
| Claim yLP yield | Transaction signature, yield account checkpoint, recipient delta. |
| Claim hLP yield | Transaction signature, hLP vault checkpoint, recipient delta. |
| Claim manager fees | Transaction signature and fee/interest liability deltas. |
| Deposit single-sided liquidity | Transaction signature, hLP shares, funding debt, yLP vault delta. |
| Withdraw single-sided liquidity | Transaction signature, hLP burn, debt repayment, target-side output. |
| Market reduce-only | Toggle signature plus rejection for risk-increasing path and success for risk-reducing path. |
| Global reduce-only | Toggle signature plus app/SDK/indexer visibility confirmation. |

## Do Not Approve If

- the release tag, release commit, binary, IDL, or types do not all match;
- release publishing or mainnet buffer deployment variables are enabled before
  all required signoff rows are approved;
- a mainnet buffer deployment can run without `source=release`, explicit
  `release_tag`, `transfer_to_squads=true`, and configured
  `SQUADS_VAULT_ADDRESS`;
- buffer authority is not transferred to Squads before the upgrade proposal;
- `solana-verify` cannot verify the deployed binary with the recorded args;
- post-deploy smoke tests do not include target-cluster transaction signatures
  for all critical user and admin flows;
- app, SDK, indexer, analytics, or aggregator owners have not acknowledged the
  final program ID and IDL.

## Evidence Template

```text
Signoff area:
Owner:
Release commit:
Release tag:
Program ID:
Cluster:
Binary hash:
IDL/type hashes:
GitHub release:
Release workflow run:
Verify-only workflow run:
Buffer deploy workflow run:
Buffer address:
Squads vault:
Squads proposal:
solana-verify output:
Post-deploy smoke signatures:
Rollback/reduce-only plan:
Integration acknowledgements:
Open deployment risks:
Approval link:
```
