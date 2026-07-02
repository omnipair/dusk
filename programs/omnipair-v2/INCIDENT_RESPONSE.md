# Omnipair V2 Incident Response Runbook

This runbook is a release-readiness artifact for Dusk. It is not approved until
the `Incident response` and `Monitoring and alerting` rows in
`SIGNOFF_CHECKLIST.md` are marked `Approved` with owner evidence.

## Scope

Use this runbook for the standalone Dusk program:

- program: `programs/omnipair-v2`;
- global reduce-only instruction: `set_global_reduce_only`;
- market reduce-only instruction: `set_reduce_only`;
- emergency signer: the public key compiled as the reduce-only authority in
  `programs/omnipair-v2/src/constants.rs`;
- deployment authority: the configured Squads vault after production handoff.

Do not use this document as a substitute for a signed release checklist,
auditor guidance, or governance/Squads approval when those are required.

## Severity Levels

| Severity | Condition | Default action |
| --- | --- | --- |
| SEV0 | Confirmed exploit, private-key compromise, wrong binary, or loss-causing invariant break | Enter reduce-only immediately, page all owners, halt integrations, prepare Squads action |
| SEV1 | Plausible exploit path, abnormal reserve/debt accounting, broken liquidation, or unsafe oracle-less risk state | Enter market or global reduce-only unless clearly contained |
| SEV2 | Indexer outage, UI misrouting, stale SDK/IDL, delayed settlement, or degraded monitoring | Pause affected integrations, keep protocol live if on-chain state is healthy |
| SEV3 | Documentation, analytics, or non-critical integration issue | Track and fix through normal release process |

When unsure between two severities, start with the higher severity and downgrade
only after evidence rules out user-fund or protocol-solvency impact.

## Signals To Monitor

Record dashboards or alert links in the `Monitoring and alerting` signoff row.
At minimum, production monitoring should cover:

- deployed program data hash and upgrade authority;
- global and market reduce-only state;
- base/quote reserve cash, live reserves, cash-backed debt, and hLP live
  reserve;
- `R_live = R_cash + D_cash_backed + R_hLP_live` drift per market side;
- hLP NAV, hLP funding debt, pending rebalance, and settlement divergence;
- yLP supply versus principal reserve backing;
- fee and interest vault balances versus liabilities;
- borrow and leverage position health distribution;
- liquidation events, insurance draws, and LP socialization;
- risk EMA age, spot/K circuit-breaker trips, daily borrow limit usage;
- failed transactions by error code, especially reduce-only and risk-breaker
  errors;
- indexer lag and event decode failures for the Dusk program ID.

## First Response

1. Assign an incident lead and scribe.
2. Record the time, slot, program ID, market IDs, affected mints, and suspected
   transactions.
3. Snapshot on-chain state for affected markets before taking optional actions.
4. Decide whether to enter market reduce-only or global reduce-only.
5. Notify app, SDK, indexer, analytics, aggregator, deployment, and security
   owners.
6. Preserve logs, RPC responses, transaction signatures, and local commands.

## Reduce-Only Procedure

Use market reduce-only when the issue is isolated to one market. Use global
reduce-only when the issue may affect shared logic, account validation, token
handling, event decoding, release artifacts, or authority safety.

Before toggling:

- confirm the emergency signer matches `constants.rs`;
- confirm the target program ID and cluster;
- confirm whether a Squads proposal is also required;
- record the planned instruction arguments.

After toggling:

- verify the transaction confirmed on the target cluster;
- verify app and integrators see the new reduce-only state;
- verify risk-increasing paths reject and risk-reducing paths remain available;
- add transaction signatures to the incident log.

## Investigation Checklist

- Identify whether the incident is code, configuration, integration, authority,
  RPC/indexer, or market-parameter driven.
- Compare current source commit, IDL, types, and deployed binary evidence.
- Reproduce the issue against a local or forked environment when possible.
- Check whether the issue violates any invariant in
  `programs/omnipair-v2/README.md`.
- Check whether losses stop after reduce-only or require a Squads upgrade.
- Record affected users, positions, vaults, and market balances.

## Recovery Checklist

- Root cause is written down and reviewed by security and relevant owners.
- Fix PR includes regression tests or an explicit reason tests cannot reproduce
  the issue.
- Release checklist and signoff register are updated with new evidence.
- Verifiable build, IDL/types, and program-interface artifacts are regenerated
  and checked.
- Squads proposal, buffer address, verified build link, and final transaction
  signatures are recorded.
- Reduce-only is lifted only after owner approval and target-cluster smoke tests
  pass.

## Incident Log Template

```text
Incident ID:
Severity:
Lead:
Scribe:
Start time:
Start slot:
Cluster:
Program ID:
Markets:
Initial signal:
Suspected impact:
Reduce-only action:
Key transaction signatures:
State snapshots:
Root cause:
Fix PR:
Release artifacts:
Post-deploy smoke:
Resolution time:
Follow-ups:
```
