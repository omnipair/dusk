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

## Market

| Account | Address |
| --- | --- |
| META/USDC market | `7Rjrf8i81hZihsuFdPfzTaP6SiQ3JEmQs7Hfg7YNjkNm` |
| yLP mint | `JBxhpnrdADegPpmB5jHagcPkRR5Nkqsgpc2pPmPAsjRg` |
| META hLP mint | `DzH431M9FMitSsWYMCkkvx24BxVqWZo8MteiB5HemRzT` |
| USDC hLP mint | `HaHJtXoacXNVuG2Q9NCsZu1rvhDwqBhDH6M5jSXz7Q5p` |
| yLP metadata | `7U9SFVtrirvy3ThKuTYcJU3LxtJKRJiBPAVjoTFyJZ4Z` |
| META hLP metadata | `GGcr38JBfDnZVHdk2Z5AHaJ6GRkcTLyXr8r2cvjpJPPc` |
| USDC hLP metadata | `GjTuuVVegphGjhQoNV7AdK6qv5kobMfEN57qwod8tXq9` |
| META reserve vault | `8RBbxBPHuecXXcHFdzowbhjLbR9VpTtX9Yuu2XgUfs8N` |
| USDC reserve vault | `8AuwR6EiQzg1sHDwpjf4gSizVG78xiw8bnnLqP4YwjGi` |
| META collateral vault | `BkJ5PDv7r6t3dPR53f2T1794L7YExegW6uC4QoqmHFkK` |
| USDC collateral vault | `CSETQwFsLbJ7zjRN1ZD9yjxMZBAVxixL8oEXvi33caue` |
| META insurance vault | `D8cDKxXf1QEZCPVhtvGkGMuLGdMZ3NbfXRVXyuJEvtbB` |
| USDC insurance vault | `9tKehifT35DUGD2bFNYCtWJgTe9NXhSG4X4kPqjLKHQ9` |
| META interest vault | `63tFqjbauvpU9ijBKFGwXZ5ksjzSAyCBdyyuczYXbEAv` |
| USDC interest vault | `7qzeSRWQAKsBhsPciGzR2DiW3BxTfKLrDmK2UjnR3Fcn` |
| META hLP/yLP custody | `EHbQTVJHWEoxX48m6M3bSY1hGbNbdC1tRgrwDum8L7Wh` |
| USDC hLP/yLP custody | `HdKkb3LRhyiDqaKP9ogJbuQ3rmUzu2JBgsP3o61QbPZY` |
| Event authority | `5wiTA28gz5WFdEYxNVeZu288WCdZWzaonA1pnApLWSuh` |

## Explorer

- [Dusk program](https://explorer.solana.com/address/JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X?cluster=devnet)
- [Leverage delegate](https://explorer.solana.com/address/AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv?cluster=devnet)
- [Faucet](https://explorer.solana.com/address/EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz?cluster=devnet)
- [META mint](https://explorer.solana.com/address/2G78R5YwVopkAxnDMmvNany3RdL98eAM2jcr3UafvL7W?cluster=devnet)
- [USDC mint](https://explorer.solana.com/address/G8XEiNtP7TZ2cZRbUdtcPoc3C19T2GgYxTJqBpWz5ZDV?cluster=devnet)
- [META/USDC market](https://explorer.solana.com/address/7Rjrf8i81hZihsuFdPfzTaP6SiQ3JEmQs7Hfg7YNjkNm?cluster=devnet)

## Frontend Defaults

```ts
export const DUSK_DEVNET = {
  programId: "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X",
  leverageDelegateProgramId: "AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv",
  faucetProgramId: "EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz",
  market: "7Rjrf8i81hZihsuFdPfzTaP6SiQ3JEmQs7Hfg7YNjkNm",
  baseMint: "2G78R5YwVopkAxnDMmvNany3RdL98eAM2jcr3UafvL7W",
  quoteMint: "G8XEiNtP7TZ2cZRbUdtcPoc3C19T2GgYxTJqBpWz5ZDV",
  ylpMint: "JBxhpnrdADegPpmB5jHagcPkRR5Nkqsgpc2pPmPAsjRg",
  baseHlpMint: "DzH431M9FMitSsWYMCkkvx24BxVqWZo8MteiB5HemRzT",
  quoteHlpMint: "HaHJtXoacXNVuG2Q9NCsZu1rvhDwqBhDH6M5jSXz7Q5p",
} as const;
```
