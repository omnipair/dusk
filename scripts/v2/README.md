# Omnipair V2 (Dusk) Devnet Helpers

These scripts create a disposable Omnipair V2 (Dusk) devnet market and tester balances without committing keypairs.

Local state and generated keypairs live in `~/.config/omnipair/dusk-devnet` by default. Override with `DUSK_DEVNET_CONFIG_DIR` or `DUSK_DEVNET_STATE`.

```bash
export ANCHOR_PROVIDER_URL=https://api.devnet.solana.com
export ANCHOR_WALLET=~/.config/solana/id.json
export DUSK_PROGRAM_ID=5EuU8artxWthfER6GTUY52TfL1aRsQH2TH8DsdGCa8cJ
export DUSK_PROGRAM_KEYPAIR=~/.config/omnipair/dusk-devnet/dusk-program-keypair.json
export DUSK_LEVERAGE_DELEGATE_PROGRAM_ID=3ChYvxNwJfr49PznzYKPmXLFUw5P2vUQZmbqqmyFJCYi
export DUSK_LEVERAGE_DELEGATE_PROGRAM_KEYPAIR=~/.config/omnipair/dusk-devnet/leverage-delegate-tranches-program-keypair.json
export DUSK_FAUCET_PROGRAM_ID=EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz
export DUSK_FAUCET_PROGRAM_KEYPAIR=~/.config/omnipair/dusk-devnet/faucet-program-keypair.json

yarn v2:build-faucet-devnet
yarn v2:deploy-faucet-devnet
yarn v2:build-devnet
yarn v2:deploy-devnet
yarn v2:build-leverage-delegate-devnet
yarn v2:deploy-leverage-delegate-devnet
npm run build --prefix packages/dusk-sdk
yarn v2:create-mock-tokens
yarn v2:initialize-mock-metadata
yarn v2:mint-mock-tokens <tester-wallet>
yarn v2:bootstrap-market
yarn v2:smoke-devnet
```

The devnet helper builds the standard program interface as SBPF v0, matching
the existing deployment and the binaries covered by the complete LiteSVM suite.
It preserves the emergency authority pinned by the SDK and the live program.
Cluster feature state can change, so re-check the target cluster before a
deployment instead of treating this note as a permanent network guarantee.
Use `yarn v2:build-devnet` so the artifact is built with `--arch v0`. Before
changing binary formats, update the test runtime and validate the resulting
deployment binaries through the complete release suite.

The deploy commands use `ANCHOR_WALLET` as fee payer and upgrade authority, so
the same funded deployer can later upgrade or close the programs. Do not deploy
with `--final`: a finalized program cannot be closed. `DUSK_PROGRAM_KEYPAIR` may
point to the source-declared Dusk keypair when doing a first deployment;
`DUSK_LEVERAGE_DELEGATE_PROGRAM_KEYPAIR` serves the same purpose for the
delegate. A public address is enough for an upgrade, but an unused address
requires its matching keypair for the initial deployment.

`yarn v2:deploy-devnet` ends with `yarn v2:ensure-futarchy-authority`. An
in-place upgrade keeps the program's futarchy authority, so the step only
reports it. A deploy to a new program ID starts with no accounts, and every
market launch fails until the authority exists, so the step creates it, signed
by `ANCHOR_WALLET` as upgrade authority. Set `DUSK_PREVIOUS_PROGRAM_ID` to the
replaced program to copy its authority, recipients and shares; otherwise the
payer receives every share.

All three program keypairs stay outside git under
`~/.config/omnipair/dusk-devnet`. The faucet PDA owns the mock mints, so the
faucet and mock tokens can remain available across later Dusk program and market
deployments. Deploy the faucet before creating mock tokens. The local
`devnet-state.json` records the deployed programs and upgrade authority, faucet,
mock mints, markets, and derived accounts for later inspection or cleanup.

Useful knobs:

- `DUSK_PREVIOUS_PROGRAM_ID` copies the replaced program's futarchy authority settings when a deploy moves to a new program ID.
- `DUSK_ORACLE_POLICY_JSON` optionally selects an oracle policy in the creation instruction; omit it for native pricing. Use the [SDK policy format](../../packages/dusk-sdk/README.md#bounded-oracle-markets). Positive-weight seeding and smoke trades require current authenticated feed accounts. Changes to an existing market use governance, not this environment variable.
- `DUSK_TOKEN_PROGRAM=token2022` creates Token-2022 mock mints.
- `DUSK_FAUCET_PROGRAM_ID` overrides the source-declared devnet faucet address.
- `DUSK_MOCK_DECIMALS=6` controls mock mint decimals.
- `DUSK_MOCK_BASE_NAME=MetaDAO` and `DUSK_MOCK_BASE_SYMBOL=META` control base metadata.
- `DUSK_MOCK_QUOTE_NAME=USD Coin` and `DUSK_MOCK_QUOTE_SYMBOL=USDC` control quote metadata.
- `DUSK_MOCK_BASE_URI` and `DUSK_MOCK_QUOTE_URI` optionally set token-list JSON URIs.
- `DUSK_MINT_AMOUNT=1000000` controls tester faucet size in human units.
- `DUSK_BASE_LIQUIDITY=100000` and `DUSK_QUOTE_LIQUIDITY=100000` control bootstrap reserves.
- `DUSK_FORCE_SEED=1` adds more bootstrap liquidity to an existing market.
- `DUSK_SMOKE_HLP_DEPOSIT=0` skips the default smoke-test hLP deposit.
- `DUSK_SMOKE_HLP_DEPOSIT_AMOUNT=10` controls the smoke-test base hLP deposit amount.
- `DUSK_SMOKE_SWAP=0` fetches state without sending the smoke swap.

## Publishing the SDK

Merging never publishes `@omnipair/dusk-sdk`; a deployment does. After both
programs are live, upload their IDLs from the same build, then tag the deployed
commit with the version in `packages/dusk-sdk/package.json`. From the checkout
that was deployed:

```bash
anchor idl upgrade --filepath target/idl/dusk.json --provider.cluster devnet --provider.wallet "${ANCHOR_WALLET:-deployer-keypair.json}" "$DUSK_PROGRAM_ID"
anchor idl upgrade --filepath target/idl/leverage_delegate.json --provider.cluster devnet --provider.wallet "${ANCHOR_WALLET:-deployer-keypair.json}" "$DUSK_LEVERAGE_DELEGATE_PROGRAM_ID"
VERSION="$(node -p "require('./packages/dusk-sdk/package.json').version")"
git tag -a "dusk-sdk-v$VERSION" -m "@omnipair/dusk-sdk $VERSION"
git push origin "dusk-sdk-v$VERSION"
```

A program deployed to a new ID takes `anchor idl init` instead of
`anchor idl upgrade`. The tag starts `.github/workflows/publish-sdk.yaml`. It
builds the package from the tagged commit, which must already be on `main`, and
publishes only when every IDL the package ships matches the IDL account on the
cluster. `-next.<n>` and `-devnet.<n>` versions describe devnet and publish
under the `next` and `devnet` dist-tags; a stable version describes mainnet and
becomes `latest`. A version already on npm is not published again. The workflow
logs in to npm through the package's trusted publisher (repository
`omnipair/dusk`, workflow `publish-sdk.yaml`, environment `npm`). A manual run
with `dry_run` checks a tag without publishing it: it runs the same build and
IDL checks and logs in to npm, so a broken trusted publisher fails the dry run.
