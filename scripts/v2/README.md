# Omnipair V2 (Dusk) Devnet Helpers

These scripts create a disposable Omnipair V2 (Dusk) devnet market and tester balances without committing keypairs.

Local state and generated keypairs live in `~/.config/omnipair/dusk-devnet` by default. Override with `DUSK_DEVNET_CONFIG_DIR` or `DUSK_DEVNET_STATE`.

```bash
export ANCHOR_PROVIDER_URL=https://api.devnet.solana.com
export ANCHOR_WALLET=~/.config/solana/id.json
export DUSK_PROGRAM_ID=JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X
export DUSK_PROGRAM_KEYPAIR=~/.config/omnipair/dusk-devnet/dusk-program-keypair.json
export DUSK_LEVERAGE_DELEGATE_PROGRAM_ID=AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv
export DUSK_LEVERAGE_DELEGATE_PROGRAM_KEYPAIR=~/.config/omnipair/dusk-devnet/leverage-delegate-program-keypair.json
export DUSK_FAUCET_PROGRAM_ID=EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz
export DUSK_FAUCET_PROGRAM_KEYPAIR=~/.config/omnipair/dusk-devnet/faucet-program-keypair.json

yarn v2:build-faucet-devnet
yarn v2:deploy-faucet-devnet
yarn v2:build-devnet
yarn v2:deploy-devnet
yarn v2:build-leverage-delegate-devnet
yarn v2:deploy-leverage-delegate-devnet
yarn v2:create-mock-tokens
yarn v2:initialize-mock-metadata
yarn v2:mint-mock-tokens <tester-wallet>
yarn v2:bootstrap-market
yarn v2:smoke-devnet
```

The devnet helper deliberately builds the `development` profile as SBPF v3.
Cluster feature state can change, so re-check the target cluster before a
deployment instead of treating this note as a permanent network guarantee.
Use `yarn v2:build-devnet` so the artifact is built with both
`--features development` and `--arch v3`.

The deploy commands use `ANCHOR_WALLET` as fee payer and upgrade authority, so
the same funded deployer can later upgrade or close the programs. Do not deploy
with `--final`: a finalized program cannot be closed. `DUSK_PROGRAM_KEYPAIR` may
point to the source-declared Dusk keypair when doing a first deployment;
`DUSK_LEVERAGE_DELEGATE_PROGRAM_KEYPAIR` serves the same purpose for the
delegate. A public address is enough for an upgrade, but an unused address
requires its matching keypair for the initial deployment.

All three program keypairs stay outside git under
`~/.config/omnipair/dusk-devnet`. The faucet PDA owns the mock mints, so the
faucet and mock tokens can remain available across later Dusk program and market
deployments. Deploy the faucet before creating mock tokens. The local
`devnet-state.json` records the deployed programs and upgrade authority, faucet,
mock mints, markets, and derived accounts for later inspection or cleanup.

Useful knobs:

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
