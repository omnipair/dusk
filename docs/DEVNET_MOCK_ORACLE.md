# Synthetic META/USDC devnet feed

This is a centralized test fixture, not market data and not Horus. It uses the
existing provider-owned `DOPRV001` report format; Dusk's deployed instructions,
SDK ABI and existing program IDs are unchanged.

The public manifest is `scripts/devnet/oracles/meta-usdc.json`. It pins the
provider program, publisher, feed ID, report PDA and the exact ordered mock
mints. The indexer exposes this as a devnet-only catalog entry. The webapp
never fetches the report account directly.

## Publish

Build the fixture:

```sh
cargo build-sbf --manifest-path tests/fixtures/devnet_oracle/Cargo.toml --sbf-out-dir target/deploy
```

The standard LiteSVM build includes this fixture and its independent signed SBF
regressions.

Set `DUSK_RPC_URL` explicitly and `MOCK_ORACLE_KEYPAIR` to the dedicated test
publisher keypair path. The CLI verifies the devnet genesis and publisher key:

```sh
node scripts/devnet/mock_oracle.mjs publish 1.00
node scripts/devnet/mock_oracle.mjs watch
node scripts/devnet/mock_oracle.mjs invalidate
```

`watch` keeps the existing price and status while refreshing the report. A
publisher can set a new synthetic price with `publish`. `invalidate` marks it
invalid; the hosted worker does not repair that status. `publish 1.00` resumes
it. Stop the publisher to test real time/slot expiry. The dedicated publisher
was funded from the existing devnet balance; no faucet funds were requested.
Never install the Dusk deployment wallet in a backend service.

## Guarantees and limits

- PDA seeds bind a signer to a feed ID. Another signer cannot update that PDA.
- Base/quote mints and feed ID are immutable after initialization. The wire
  output is exactly 145 bytes, matching `NormalizedOracleV1`.
- The report uses the actual chain clock and a checked monotone sequence.
  Updates include an expected sequence: a delayed refresh cannot overwrite a
  newer manual change. Prefunding the report PDA does not block initialization.
- This proves the adapter path, freshness and rejection behavior, not the
  accuracy, independence, aggregation or economics of an external oracle.

The optional devnet prices worker holds only the restricted publisher key and
refreshes a valid report every 15s plus transaction latency. Invalid reports
remain invalid. Stopping it creates a real stale observation after either
configured age limit. The initial demo market uses 35% weight, 1.25× cap,
60s/150-slot age limits and 1% confidence width. These are test settings.

Live signatures and test outcomes are recorded under
`docs/evidence/devnet-oracle-2026-10-31/`.

## Publisher key scope

A new random publisher key was generated only for this test feed. Its entire
devnet transaction history was checked against this mock program and funding
transfers, with zero token accounts. It is distinct from the program/deployment
authority. Private key files remain mode 600 locally and are absent from the
repository and build upload. With explicit user approval, only this restricted
key is installed in the devnet prices service through stdin; the deployment
wallet is never exported. The publisher has a finite prefunded devnet balance
and cannot draw additional funds automatically. Public scope evidence is in
`docs/evidence/devnet-oracle-2026-10-31/publisher-scope.json`.
