# Omnipair V2 Security Review Signoff

This document defines the evidence required before the `Security review` row in
`SIGNOFF_CHECKLIST.md` can be marked `Approved`. It is a review template, not a
claim that Dusk has been audited or is safe to deploy.

Use this document only for the exact release commit, production feature set,
generated IDL/types, verifiable binary, and deployment workflow being approved.

## Review Scope

Record whether each scope item is included, excluded, or blocked.

| Scope item | Required evidence |
| --- | --- |
| Source commit | Final git commit, branch, PR, and tag or release candidate. |
| Program binary | Verifiable `omnipair_v2.so`, embedded `GIT_REV`, embedded `GIT_RELEASE`, and `solana-verify` plan. |
| IDL and types | `target/idl`, `target/types`, and committed `packages/program-interface` drift check results. |
| Dusk program | `programs/omnipair-v2` instruction handlers, state, math, tests, and feature flags. |
| Leverage delegate | `programs/leverage_delegate` callback validation and delegated-close behavior. |
| Workflows | CI, release-build, and buffer-deploy workflow gates. |
| Integrations | App, SDK, indexer, analytics, and aggregator assumptions that can affect user safety. |
| Deployment authority | Squads vault, buffer authority transfer, release window, deployer key handling, and rollback path. |

## Threat Model Checklist

Security review must explicitly cover:

- unauthorized account, mint, PDA, signer, manager, operator, futarchy,
  reduce-only, or upgrade-authority paths;
- incorrect Token or Token-2022 program routing;
- Token-2022 transfer-fee, transfer-hook, freeze-authority, mint-authority, and
  metadata edge cases;
- reserve accounting, hLP live reserve, debt shares, fee liabilities, insurance,
  and socialization state consistency;
- oracle-less price manipulation, same-slot manipulation, stale EMA windows,
  and liquidity-EMA borrow-limit manipulation;
- liquidation, bad debt, dust, rounding, and insurance exhaustion behavior;
- hLP NAV, stale settlement reference, pending rebalance, and cash-headroom
  failure modes;
- isolated leverage delegated-close callback spoofing, stale payloads, wrong
  recipient, wrong market, wrong position, wrong output mint, or residual
  mismatch;
- reduce-only liveness for risk-reducing paths during emergency operation;
- release artifact substitution, wrong binary deployment, wrong program ID,
  stale IDL/types, and workflow bypasses;
- committed secret, RPC key, private key, absolute developer path, or local
  artifact leakage.

## Required Review Inputs

Before approval, attach or link:

- `CORE_INVARIANT_SIGNOFF.md`;
- `RISK_PARAMETER_SIGNOFF.md`;
- `SIMULATION_SIGNOFF.md`;
- `INCIDENT_RESPONSE.md`;
- final local verification commands and outputs;
- latest GitHub CI run links for the release commit;
- real Metaplex metadata CPI smoke evidence;
- release-build verify-only evidence;
- mainnet buffer-deploy dry-run or rehearsal evidence, including blocked-mainnet
  guard behavior before approval variables are enabled.

## Workflow And Authority Gates

Reviewers must confirm:

- `DUSK_RELEASES_ENABLED` remains unset or `false` until the signed release
  window;
- `DUSK_MAINNET_BUFFER_DEPLOYS_ENABLED` remains unset or `false` until the
  signed deployment window;
- mainnet buffer deploys require `source=release`, explicit `release_tag`,
  `transfer_to_squads=true`, and configured `SQUADS_VAULT_ADDRESS`;
- deployment buffers transfer authority to the configured Squads vault;
- the emergency reduce-only authority in `constants.rs` is the intended signer;
- release and verify-only workflows install JavaScript dependencies before
  program-interface drift checks;
- `yarn check:hygiene` covers tracked env/key artifacts, high-confidence secret
  literals, and developer-machine absolute paths.

## Required Commands

Record exact commands, feature flags, commit, and pass/fail result. The release
review can add stricter commands, but should not omit these without an explicit
owner-approved reason.

```bash
cargo fmt --all -- --check
yarn check:hygiene
yarn check:clippy
cargo test -p omnipair-v2
cargo test -p omnipair-v2 --lib --features production
cargo test -p leverage_delegate
yarn typecheck
yarn test-litesvm
npm run check:program-interface
npm run check-idl-current --prefix packages/program-interface
```

For release-candidate artifacts, also record:

```bash
anchor build --verifiable -p omnipair-v2 -- --features production
solana-verify verify-from-repo <args recorded in RELEASE_CHECKLIST.md>
```

## Do Not Approve If

- any `SIGNOFF_CHECKLIST.md` row required for production remains `Pending` or
  unresolved `Blocked`;
- the reviewed commit differs from the deployed or released binary;
- the generated IDL/types differ from committed program-interface files;
- mainnet releases or buffer deploys can run without the documented repository
  variables and explicit release inputs;
- any manager/operator/futarchy/reduce-only/update authority can be spoofed or
  bypassed;
- any critical invariant in `CORE_INVARIANT_SIGNOFF.md` lacks review evidence;
- any economic assumption in `RISK_PARAMETER_SIGNOFF.md` lacks owner evidence;
- any required simulation scenario in `SIMULATION_SIGNOFF.md` is missing without
  an approved exclusion;
- incident response lacks reachable owners, reduce-only transaction evidence,
  or monitoring for reserve/debt drift and fee-liability backing.

## Evidence Template

```text
Signoff area:
Owner:
Reviewer:
Commit:
Release tag or candidate:
Program ID:
Binary hash:
IDL/type artifact hashes:
GitHub CI runs:
Local commands:
Metaplex CPI evidence:
Release verify-only evidence:
Buffer deploy rehearsal:
Authority review notes:
Threat model notes:
Open findings:
Resolved findings:
Approved exclusions:
Approval link:
```
