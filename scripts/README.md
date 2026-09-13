# Dusk devnet scripts

The active integration target is Solana devnet. Use `scripts/v2/` for token and
market setup and `scripts/devnet/` for the live SDK flows. Read each script's
wallet, market and amount settings before running it; use dedicated devnet
wallets and record transaction signatures as release evidence.

Build and deploy the existing programs with the `v2:build-*-devnet` and
`v2:deploy-*-devnet` package commands. Verify the configured devnet genesis,
program IDs, upgrade authority, binary hashes and SDK IDLs before publishing
new pins to the app, indexer or keepers.

The deterministic program suite is `yarn test-litesvm`. It runs in process and
requires no validator service. Full release validation is defined by `AGENTS.md`
and `.github/workflows/ci.yaml`.
