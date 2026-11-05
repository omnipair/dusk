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

The deploy commands use `ANCHOR_WALLET` as fee payer, upgrade authority and IDL
authority, so the same funded deployer can later upgrade or close the programs. Do not deploy
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

A merge to `main` publishes `@omnipair/dusk-sdk` once its version is deployed,
so deploy first, then merge. Deploy the programs the branch changes with the
commands above, then merge the pull request that sets the new version in
`packages/dusk-sdk/package.json`. `yarn v2:deploy-devnet` and
`yarn v2:deploy-leverage-delegate-devnet` upload each program's IDL from the
same build right after deploying it, through `scripts/v2/upload_idl.sh`. The
script upgrades the program's IDL account, or creates it for a program deployed
to a new ID, and stops when `ANCHOR_WALLET` is not the account's authority. A
program deployed any other way needs the script before the merge, run from the
checkout that was deployed:

```bash
scripts/v2/upload_idl.sh target/idl/dusk.json
scripts/v2/upload_idl.sh target/idl/leverage_delegate.json
```

The merge starts `.github/workflows/publish-sdk.yaml`. It
publishes the version when it is not on npm yet and every IDL the package ships
matches the IDL account on the cluster. `-next.<n>` and `-devnet.<n>` versions
describe devnet and publish under the `next` and `devnet` dist-tags; a stable
version describes mainnet and becomes `latest`. npm never takes a version twice,
so a change to the package needs a new version, and a merge that keeps a
published version publishes nothing.

The check compares IDLs only. A program change that keeps the IDLs passes it
whether or not it is deployed, so deploy such a change before merging it.

A merge that lands before its deployment publishes nothing and leaves a warning
on its `publish-sdk` run. Once the deployment is live, re-run that run:

```bash
gh run list --repo omnipair/dusk --workflow publish-sdk.yaml --branch main --limit 5
gh run rerun <run-id> --repo omnipair/dusk
```

An organization owner can instead tag the deployed commit once it is on `main`.
The tag push runs the same checks, and fails instead of warning when the IDLs
differ:

```bash
VERSION="$(node -p "require('./packages/dusk-sdk/package.json').version")"
git tag -a "dusk-sdk-v$VERSION" -m "@omnipair/dusk-sdk $VERSION"
git push origin "dusk-sdk-v$VERSION"
```

The workflow logs in to npm through the package's trusted publisher (repository
`omnipair/dusk`, workflow `publish-sdk.yaml`, environment `npm`). A manual run
with `dry_run` checks a tag without publishing it: it runs the same build and
IDL checks and logs in to npm, so a broken trusted publisher fails the dry run.
npm does not check which branch a run comes from, so only runs from `main` or a
`dusk-sdk-v*` tag can use the `npm` environment (Settings, Environments). The
publish job of a run dispatched from another branch, a dry run included, is
rejected before it starts. A tag push runs the workflow file of the tagged
commit, so only organization owners can create, move or delete `dusk-sdk-v*`
tags (ruleset `protect-sdk-release-tags`).
