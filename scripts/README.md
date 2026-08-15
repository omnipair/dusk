# Omnipair V2 (Dusk) Scripts

This directory contains Omnipair V2 (Dusk) development and fork-lab scripts.

## Devnet

```bash
yarn v2:build-devnet
yarn v2:deploy-devnet
yarn v2:create-mock-tokens
yarn v2:mint-mock-tokens <wallet>
yarn v2:bootstrap-market
yarn v2:smoke-devnet
```

Local Dusk state and generated keypairs live under
`~/.config/omnipair/dusk-devnet` unless overridden by the environment variables
documented in `scripts/v2/README.md`.

## Fork Lab

```bash
yarn build:v2-fork-rpc-controller
# Requires FORK_SDK_REMOTE_RPC_URL and an explicit FORK_LAB_PAYER_KEYPAIR_*.
yarn v2-fork:surfpool
yarn v2-fork:surfpool:cli # legacy local CLI fallback
yarn v2-fork:rpc-proxy
yarn v2-fork:api
yarn test-surfpool-v2
yarn surfpool-v2-e2e
```

The fork lab runs `dusk` against a private Surfpool fork and exposes the
browser-facing Dusk fork API. The public proxy accepts HTTP RPC and same-domain
WebSocket upgrades; configure its private targets with `SURFPOOL_RPC_URL` and
`SURFPOOL_WS_URL`. See `scripts/v2-fork-lab/README.md`.

## Utilities

- `scripts/utils/address_vanity.ts`: local address-generation helper.
- `scripts/utils/deploy_tokens.ts`: mock token deployment helper.

Older pair-program scripts intentionally do not live in this repository.
