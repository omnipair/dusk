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
| META/USDC market | `8bK5oiUguBgQDNvEFyvT4wJTn4mmruuKkLXBWZJp1apA` |
| yLP mint | `DDi4gvGrkatHCKFLKY1qBY6Jcruk9bXtxMXFk935C1A1` |
| META hLP mint | `FZAwSsgUdjk8bSL6fuYufNE8RXFmWw6jEtfTkvnyBCBP` |
| USDC hLP mint | `93AAXDXV61FefMwdq2ZeUpS4dWdVt1PurbFmKbaaKYdu` |
| yLP metadata | `7UNLvZo35UY79jX6LTvCD8WYyGrvXwiVtWbsjigBp7Cv` |
| META hLP metadata | `DwD9UwzLmEjs51vk1k9bnCkD9hDovNzKSv4vmeRXcKt3` |
| USDC hLP metadata | `GD1sGkNfU56MrtphkgRnX1fPuSQSoj4YuJtg1dL5ZPmz` |
| META reserve vault | `6vPbJ19aCN4SYvZeALXBASztt7uERaWAcgWy5LXcxkt` |
| USDC reserve vault | `A25iu53KX8XcTB1JmAiGqDtKYnbmrsgjGLNXikiDdCN7` |
| META collateral vault | `8wXRj8Pt9HX6QRctYVtHwbobmevTkABEHSZr23iYhWPL` |
| USDC collateral vault | `A9nyUHSoeK2rfQThHNo51TMYk6XzstsyCooFQeXLahDH` |
| META insurance vault | `5jwCqUiHL84WxD2yJWdqtfzkDaPwb6mPQF513H7M2kMk` |
| USDC insurance vault | `85rFpdDzpsQBaLEVDyYD51zBV17ZxwA8SVhZ1j39qGyx` |
| META interest vault | `B5kyXTroQvZ2R2FisKzK8hiRXN4pLL9vAmj2hU9pSee5` |
| USDC interest vault | `ED8da2YnwHCrYJn7C1ngYshF8uuV8mgCYkFcwQdoVJfu` |
| META hLP/yLP custody | `sk6mhf4Ye8g9XAco9QbWJsYkNWB18z9VejMrkiq1d1W` |
| USDC hLP/yLP custody | `CHkxuWrMDEr5Z4m3E76seoi2cHNEdFhtHf8G9dwzFcaK` |
| Event authority | `5wiTA28gz5WFdEYxNVeZu288WCdZWzaonA1pnApLWSuh` |

The fresh v6 market is seeded with 10,000 mock META and 10,000 mock USDC.

## Explorer

- [Dusk program](https://explorer.solana.com/address/JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X?cluster=devnet)
- [Leverage delegate](https://explorer.solana.com/address/AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv?cluster=devnet)
- [Faucet](https://explorer.solana.com/address/EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz?cluster=devnet)
- [META mint](https://explorer.solana.com/address/2G78R5YwVopkAxnDMmvNany3RdL98eAM2jcr3UafvL7W?cluster=devnet)
- [USDC mint](https://explorer.solana.com/address/G8XEiNtP7TZ2cZRbUdtcPoc3C19T2GgYxTJqBpWz5ZDV?cluster=devnet)
- [META/USDC market](https://explorer.solana.com/address/8bK5oiUguBgQDNvEFyvT4wJTn4mmruuKkLXBWZJp1apA?cluster=devnet)

## Frontend Defaults

```ts
export const DUSK_DEVNET = {
  programId: "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X",
  leverageDelegateProgramId: "AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv",
  faucetProgramId: "EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz",
  market: "8bK5oiUguBgQDNvEFyvT4wJTn4mmruuKkLXBWZJp1apA",
  baseMint: "2G78R5YwVopkAxnDMmvNany3RdL98eAM2jcr3UafvL7W",
  quoteMint: "G8XEiNtP7TZ2cZRbUdtcPoc3C19T2GgYxTJqBpWz5ZDV",
  ylpMint: "DDi4gvGrkatHCKFLKY1qBY6Jcruk9bXtxMXFk935C1A1",
  baseHlpMint: "FZAwSsgUdjk8bSL6fuYufNE8RXFmWw6jEtfTkvnyBCBP",
  quoteHlpMint: "93AAXDXV61FefMwdq2ZeUpS4dWdVt1PurbFmKbaaKYdu",
} as const;
```
