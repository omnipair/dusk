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

## LP token metadata

`scripts/devnet/create_named_market.ts` creates a market the way mainnet will:
LP mints at `yLP`/`hLP` addresses ground by the vanity server for the creator's
key, transfer-hook validation accounts for all three mints (Token-2022 refuses
to move a hooked token without one), names from the SDK's `lpTokenNaming`, and
per-mint images and JSON pinned to IPFS before `initialize_lp_metadata`. It needs `VANITY_API_URL` (defaults to
https://vanity.omnipair.fi) and either `PINATA_JWT` or
`PINATA_SIGNED_URL_ENDPOINT` (a running webapp's
`/api/pinata/create-signed-url`, which keeps the key out of the shell).
`LP_METADATA_DRY_RUN=1` grinds, renders and writes everything under
`target/lp-metadata/<market>` without touching the chain or Pinata.
`scripts/lp-metadata/render_preview.ts` renders the three images for any pair
from logo URLs or placeholders.
