# Omnipair Dusk (v2) Surfpool Mainnet-Fork Lab

This stack runs `dusk` against a private Surfpool mainnet fork and exposes a small fork API for the Dusk webapp. It is intentionally separate from the Helius-backed indexer path because private Surfpool transactions are not visible to Helius Atlas.

## Services

- `v2-surfpool-rpc`: private Surfpool RPC. Builds `anchor build -p dusk -- --features "development"`, starts a mainnet fork, deploys `target/deploy/dusk.so`, and waits for the local program deployment log.
- `v2-surfpool-rpc-proxy`: public Solana RPC proxy for wallets. It forwards normal RPC calls and blocks unauthenticated `surfnet_*` cheatcodes. Set `FORK_ADMIN_TOKEN` for admin access.
- `v2-fork-api`: public Dusk fork API. It bootstraps a fork-only META/USDC market by default, funds wallets through bounded Surfpool cheatcodes, serves webapp-compatible Dusk read endpoints, and builds unsigned browser transactions.

## Local Commands

```sh
npm run v2-fork:surfpool
npm run v2-fork:rpc-proxy
npm run v2-fork:api
npm run test-surfpool-v2
npm run surfpool-v2-e2e
LEVERAGE_ONLY=true node scripts/v2-fork-lab/test_leverage.mjs
```

## Core Env

```sh
SURFPOOL_RPC_URL=http://127.0.0.1:8899
PUBLIC_SURFPOOL_RPC_URL=http://127.0.0.1:8898
DUSK_PROGRAM_ID=358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv
DUSK_BASE_MINT=METAwkXcqyXKy1AtsSgJ8JiUHwGCafnZL38n3vYmeta
DUSK_QUOTE_MINT=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
FORK_ADMIN_TOKEN=<shared-secret>
```

The fork API accepts `FORK_LAB_PAYER_KEYPAIR_JSON`, `FORK_LAB_PAYER_KEYPAIR_BASE64`, `FORK_LAB_PAYER_KEYPAIR`, or `ANCHOR_WALLET`. If none are set it creates a local `.v2-fork-lab/payer.json`.

## API Endpoints

- `GET /health`
- `GET /api/v2/fork/config`
- `POST /api/v2/fork/fund-wallet`
- `POST /api/v2/fork/tx/add-liquidity`
- `POST /api/v2/fork/tx/swap`
- `POST /api/v2/fork/tx/deposit-collateral`
- `POST /api/v2/fork/tx/borrow`
- `POST /api/v2/fork/tx/repay`
- `POST /api/v2/fork/tx/deposit-single-sided`
- `POST /api/v2/fork/tx/withdraw-single-sided`
- `POST /api/v2/fork/tx/open-leverage`
- `POST /api/v2/fork/tx/close-leverage`
- `POST /api/v2/fork/tx/deposit-leverage-collateral`
- `POST /api/v2/fork/tx/withdraw-leverage-collateral`
- `GET /api/v2/fork/leverage/positions?owner=:wallet`
- `GET /api/v2/markets`
- `GET /api/v2/markets/:marketAddress`
- `GET /api/v2/markets/:marketAddress/swaps`
- `GET /api/v2/users/:wallet/positions`
- `GET /api/v2/users/:wallet/activity`

Transaction endpoints return an unsigned base64 legacy transaction in `data.transaction`. The browser wallet signs and submits it to `data.rpcUrl`, which should be the public RPC proxy.

Leverage open/close accepts `marginMode`: `0` keeps the debt-funded path,
while `1` deposits and settles in the collateral token. Collateral-margin opens
require `maxDebtInRaw`; collateral-margin closes require
`maxCollateralInRaw` and accept `minResidualOutRaw`.
