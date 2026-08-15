# Dusk protocol client tests

This suite validates Dusk through signed Solana transactions against the Surfpool fork. It does not invoke Rust handlers directly and does not mutate protocol state except through submitted transactions. Fork wallet funding is test setup; every protocol action is simulated, signed by an independent keypair wallet, submitted over RPC, and confirmed.

The catalog in `catalog.ts` is the intended behavioral surface. It includes happy paths, expected rejections, exact integer boundaries, multi-wallet state machines, security substitutions, and stress scenarios. The runner compares both catalog and executed instruction coverage with the current Anchor IDL, so newly added instructions remain visibly uncovered until a scenario executes them.

## Run

Start a fresh fork RPC, proxy, and API, then run:

```bash
npm run test-protocol-client
```

For an isolated run that builds Dusk plus `leverage_delegate` and the Surfpool
SDK controller, creates a new fork, starts every service, runs the suite, and
cleans up automatically:

```bash
npm run test-protocol-client:fresh
```

The isolated command refuses to reuse occupied service ports. It uses
`FORK_SDK_REMOTE_RPC_URL` (or `SURFPOOL_DATASOURCE_RPC_URL`) when set and falls
back to Solana's public mainnet RPC for local testing. Set
`PROTOCOL_TEST_SKIP_BUILD=true` only when both SBF artifacts and the compiled
SDK controller already exist, or set `PROTOCOL_TEST_KEEP_SERVICES=true` to
leave its clean stack running after the report is written.

Every harness API and Solana RPC response, including its body, is bounded by
`PROTOCOL_TEST_HTTP_TIMEOUT_MS` (150 seconds by default, longer than the API's
120-second route deadline). A scenario that still
does not settle within `PROTOCOL_TEST_SCENARIO_TIMEOUT_MS` (15 minutes by
default) is recorded as failed, aborts its active HTTP/RPC request, and stops
the entire run so late work cannot mutate later scenarios. The fresh runner
also terminates the test process after `PROTOCOL_TEST_RUN_TIMEOUT_MS` (three
hours by default) and then waits for each detached service process group to
exit before removing fork state.

Any POST transport timeout, aborted or unreadable POST response, API payload
marked `uncertainOutcome`, Solana `sendTransaction` transport failure, or
confirmation timeout after submission is fatal to the run. The harness records
the current scenario, finalizes the report, and does not start another
scenario against potentially late-arriving state. Definitive API errors that
do not mark the outcome uncertain remain ordinary scenario failures.

The fresh runner gives the Surfpool controller and read-only API separate
state directories. This mirrors Railway service isolation and proves that the
API reconstructs bootstrap evidence from confirmed fork history rather than
reading controller-local files.

Set `FORK_API_URL` to target another fork API. Reports are written incrementally to `.protocol-test-lab/runs/<run-id>/` and mirrored at `.protocol-test-lab/runs/latest.json`. A failed process still leaves transaction logs, assertions, and `issues.md` for reproduction.

Each report distinguishes:

- catalog coverage: an instruction has at least one designed scenario;
- execution coverage: a decoded instruction was present in a real submitted or expected-failure transaction;
- behavioral evidence: state assertions and exact expected rejection results;
- pending scope: catalog scenarios that do not yet have an RPC implementation.

Bootstrap coverage is sourced from confirmed fork history, not API-process
memory. The API discovers successful top-level Dusk transactions, accepts only
the expected futarchy authority, configured market, LP mint, metadata, and
transfer-hook accounts, and deduplicates signatures. The client then fetches
and decodes every reported transaction again before counting it. Expected
market and LP-metadata counts scale with `/api/v2/fork/config.markets`, so the
default META/USDC CPMM plus concentrated deployment requires two market and six
LP-metadata transactions. Surfpool-only raw authority seeding is disabled by
default, requires the explicit
`DUSK_ALLOW_SURFPOOL_AUTHORITY_ACCOUNT_SEED=true` escape hatch, and is never
represented as a transaction. The production-shaped fresh-stack scenario
requires one real `init_futarchy_authority` transaction, so both the escape
hatch and an unexplained preexisting authority fail that scenario.
