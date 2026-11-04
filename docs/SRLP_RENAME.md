# Senior LP naming

The senior receipt is **srLP**, paired with **jrLP** for junior liquidity.
This is a terminology and interface rename; tranche economics are unchanged.

| Surface | Senior name |
| --- | --- |
| Token kind / SDK mint kind | `srlp` |
| Rust enum | `YieldTokenKind::Srlp` (code 0) |
| Rust account / event fields | `srlp_mint`, `srlp_supply`, `srlp_shares` |
| SDK / frontend fields | `srlpMint`, `srlpSupply`, `srlpShares` |
| Senior liquidity module | `instructions/liquidity/srlp` |
| Token name / symbol example | `META/USDC srLP` / `srME/US` |
| New senior mint vanity suffix | `srLP`, case-sensitive |

The generated Dusk and leverage-delegate IDLs, TypeScript types, SDK read/write
facades, governance, yield accounts, hLP custody, metadata generation, market
launch scripts, tests, protocol docs and both current whitepaper PDFs use the
new names. The shared SDK version is `2.17.0-next.20261104-srlp.1`.

## Consumer coverage

The coordinated `feat/rename-senior-lp` changes in `dusk-webapp` update the
indexer response contract, transaction builders, bootstrap vanity requests,
metadata upload/rendering, UI labels, in-app docs and compiled SDK/IDL pins.
`dusk-indexer` updates public projections and API names, adds a checksummed
projection migration and vendors the exact generated candidate IDLs and SDK.
`dusk-keepers` updates account consumers and regenerates its IDL-derived field
layout. The external `omnipair/docs` repository and protocol lab contain no
active Dusk yLP references.

## Stable bytes and historical evidence

Instruction/account/event discriminators, serialized field order, enum codes,
program IDs, arithmetic and the existing hLP custody PDA seed bytes are
preserved. The renamed `HLP_SRLP_VAULT_SEED_PREFIX` deliberately retains
`hlp_ylp_vault`: the seed is an address namespace, not a displayed token name.
It must match the program, SDK, scripts and indexer derivations.

Historical captures, archived IDLs, published release packages, audit/research
snapshots and the `paper/previous` bundle retain their original identities.
Indexer projections normalize the historical field spelling to srLP without
rewriting raw observations, hashes, revisions or ownership evidence. Keeper
layout aliases read the names from the selected pinned IDL.

The srLP candidate does not assert a deployment. Promote its indexer pin only
after matching program/delegate deployment verification and replay review;
activate the frontend with that matching deployment envelope. Merging source does not rewrite existing on-chain token metadata. New markets use
the srLP name, symbol and vanity suffix from initialization.
