# Omnipair Dusk (v2) Devnet Helpers

These scripts create a disposable Omnipair Dusk (v2) devnet market and tester balances without committing keypairs.

Local state and generated keypairs live in `~/.config/omnipair/dusk-devnet` by default. Override with `DUSK_DEVNET_CONFIG_DIR` or `DUSK_DEVNET_STATE`.

```bash
export ANCHOR_PROVIDER_URL=https://api.devnet.solana.com
export ANCHOR_WALLET=~/.config/solana/id.json
export DUSK_PROGRAM_ID=oMNi2XGwWxDbEvhS2pWRQ6dtw8GkNBV42hfLZD6WmMF
export DUSK_PROGRAM_KEYPAIR=~/.config/omnipair/dusk-devnet/dusk-program-keypair.json

yarn v2:build-devnet
yarn v2:deploy-devnet
yarn v2:create-mock-tokens
yarn v2:mint-mock-tokens <tester-wallet>
yarn v2:bootstrap-market
yarn v2:smoke-devnet
```

Devnet currently has SBPFv3 deployment active and rejects SBPF v0/v1/v2
program deployments. Use `yarn v2:build-devnet` before deploying so the
artifact is built with `--arch v3`.

`yarn v2:deploy-devnet` needs the deployer wallet to hold enough devnet SOL for
program rent. The generated vanity program keypair stays outside git at
`~/.config/omnipair/dusk-devnet/dusk-program-keypair.json` unless
`DUSK_PROGRAM_KEYPAIR` points elsewhere.

Useful knobs:

- `DUSK_TOKEN_PROGRAM=token2022` creates Token-2022 mock mints.
- `DUSK_MOCK_DECIMALS=6` controls mock mint decimals.
- `DUSK_MINT_AMOUNT=1000000` controls tester faucet size in human units.
- `DUSK_BASE_LIQUIDITY=100000` and `DUSK_QUOTE_LIQUIDITY=100000` control bootstrap reserves.
- `DUSK_FORCE_SEED=1` adds more bootstrap liquidity to an existing market.
- `DUSK_SMOKE_HLP_DEPOSIT=0` skips the default smoke-test hLP deposit.
- `DUSK_SMOKE_HLP_DEPOSIT_AMOUNT=10` controls the smoke-test base hLP deposit amount.
- `DUSK_SMOKE_SWAP=0` fetches state without sending the smoke swap.
