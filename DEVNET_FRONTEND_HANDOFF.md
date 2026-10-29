# Dusk Devnet Frontend Handoff

This is the current reusable Dusk deployment on Solana devnet.

## Network

- Cluster: `devnet`
- Default RPC: `https://api.devnet.solana.com`
- SPL token program: `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`
- Metaplex token metadata program: `metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s`

## Programs

| Program | Address | Anchor IDL account |
| --- | --- | --- |
| Dusk | `JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X` | `A78P9gNEWQsGXi8wRFY3ppewtKGDuoTgSUsx23n8Buri` |
| Leverage delegate | `AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv` | `2fBTnoqoHx7aXRHbPvpNH1NCQMRVLYMchYPK4XGeCNMW` |
| Mock-token faucet | `EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz` | `7PgCv82EoWCGanp2SSg1Q63wYBoJJ9VyCAHYfcZCJJf` |
| Faucet mint-authority PDA | `8q3DtsqnNaxZHLnJmoGNTKZCLv1ExaimJHCBGNJN7ij4` | — |

The three IDLs are published on devnet and can be fetched with `anchor idl fetch <PROGRAM_ID> --provider.cluster devnet`.

## Mock Tokens

| Role | Name | Symbol | Decimals | Mint | Metadata PDA |
| --- | --- | --- | ---: | --- | --- |
| Base | MetaDAO | META | 6 | `2G78R5YwVopkAxnDMmvNany3RdL98eAM2jcr3UafvL7W` | `3DbrG78sKmAE6a7YC6fCzaVcK4hsnh5rFNAmdYTharkX` |
| Quote | USD Coin | USDC | 6 | `G8XEiNtP7TZ2cZRbUdtcPoc3C19T2GgYxTJqBpWz5ZDV` | `8hwkgrvnA6eopUZ5j8m3fvZunV5qAStfAqvNr6QwHcbb` |

Both are devnet mock tokens. They are not the canonical mainnet META or USDC mints.

## Markets

The reset deployment intentionally has no markets. Create the first market
through the app so the complete permissionless launch flow is exercised. The
indexer and webapp accept an empty market list and discover the new market once
its creation transaction is indexed.

## Explorer

- [Dusk program](https://explorer.solana.com/address/JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X?cluster=devnet)
- [Leverage delegate](https://explorer.solana.com/address/AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv?cluster=devnet)
- [Faucet](https://explorer.solana.com/address/EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz?cluster=devnet)
- [META mint](https://explorer.solana.com/address/2G78R5YwVopkAxnDMmvNany3RdL98eAM2jcr3UafvL7W?cluster=devnet)
- [USDC mint](https://explorer.solana.com/address/G8XEiNtP7TZ2cZRbUdtcPoc3C19T2GgYxTJqBpWz5ZDV?cluster=devnet)

## Frontend Defaults

```ts
export const DUSK_DEVNET = {
  programId: "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X",
  leverageDelegateProgramId: "AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv",
  faucetProgramId: "EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz",
  baseMint: "2G78R5YwVopkAxnDMmvNany3RdL98eAM2jcr3UafvL7W",
  quoteMint: "G8XEiNtP7TZ2cZRbUdtcPoc3C19T2GgYxTJqBpWz5ZDV",
} as const;
```

Market and LP-mint addresses are outputs of market creation and must be read
from the indexer after the transaction confirms.
