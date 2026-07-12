# Omnipair Dusk (v2) Scripts

This directory contains Omnipair Dusk (v2) development and fork-lab scripts.

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
yarn v2-fork:surfpool
yarn v2-fork:rpc-proxy
yarn v2-fork:api
yarn test-surfpool-v2
yarn surfpool-v2-e2e
```

The fork lab runs `dusk` against a private Surfpool fork and exposes the
browser-facing Dusk fork API. See `scripts/v2-fork-lab/README.md`.

## Utilities

- `scripts/utils/address_vanity.ts`: local address-generation helper.
- `scripts/utils/deploy_tokens.ts`: mock token deployment helper.

Older pair-program scripts intentionally do not live in this repository.
