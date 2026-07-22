# Dusk protocol client tests

This suite validates Dusk through signed Solana transactions against the Surfpool fork. It does not invoke Rust handlers directly and does not mutate protocol state except through submitted transactions. Fork wallet funding is test setup; every protocol action is simulated, signed by an independent keypair wallet, submitted over RPC, and confirmed.

The catalog in `catalog.ts` is the intended behavioral surface. It includes happy paths, expected rejections, exact integer boundaries, multi-wallet state machines, security substitutions, and stress scenarios. The runner compares both catalog and executed instruction coverage with the current Anchor IDL, so newly added instructions remain visibly uncovered until a scenario executes them.

## Run

Start a fresh fork RPC, proxy, and API, then run:

```bash
npm run test-protocol-client
```

For an isolated run that builds Dusk, creates a new fork, starts every service, runs the suite, and cleans up automatically:

```bash
npm run test-protocol-client:fresh
```

The isolated command refuses to reuse occupied service ports. Set `PROTOCOL_TEST_SKIP_BUILD=true` to reuse the current SBF artifact or `PROTOCOL_TEST_KEEP_SERVICES=true` to leave its clean stack running after the report is written.

Set `FORK_API_URL` to target another fork API. Reports are written incrementally to `.protocol-test-lab/runs/<run-id>/` and mirrored at `.protocol-test-lab/runs/latest.json`. A failed process still leaves transaction logs, assertions, and `issues.md` for reproduction.

Each report distinguishes:

- catalog coverage: an instruction has at least one designed scenario;
- execution coverage: a decoded instruction was present in a real submitted or expected-failure transaction;
- behavioral evidence: state assertions and exact expected rejection results;
- pending scope: catalog scenarios that do not yet have an RPC implementation.
