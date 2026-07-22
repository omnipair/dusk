import { AccountMeta, PublicKey } from "@solana/web3.js";

const DEFAULT_PROGRAM_ID = "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv";
const MPL_TOKEN_METADATA_PROGRAM_ID = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";

function getProgramIdFromEnv(fallback: string): string {
  if (typeof process === "undefined" || !process.env) return fallback;
  return process.env.DUSK_PROGRAM_ID ?? fallback;
}

/**
 * Omnipair Dusk (v2) program ID.
 * Reads from env DUSK_PROGRAM_ID.
 */
export const DUSK_PROGRAM_ID = new PublicKey(getProgramIdFromEnv(DEFAULT_PROGRAM_ID));

export const PROGRAM_ID = DUSK_PROGRAM_ID;
export const TOKEN_METADATA_PROGRAM_ID = new PublicKey(MPL_TOKEN_METADATA_PROGRAM_ID);

/**
 * PDA seeds used by the program
 */
export const SEEDS = {
  MARKET_V2: Buffer.from("market_v2"),
  MARKET_RESERVE_VAULT: Buffer.from("market_reserve"),
  MARKET_COLLATERAL_VAULT: Buffer.from("market_collateral"),
  MARKET_FEE_VAULT: Buffer.from("market_fee"),
  MARKET_INTEREST_VAULT: Buffer.from("market_interest"),
  BORROW_POSITION: Buffer.from("borrow_position_v2"),
  LEVERAGE_POSITION: Buffer.from("leverage_position_v2"),
  YIELD_ACCOUNT: Buffer.from("yield"),
  HLP_YLP_VAULT: Buffer.from("hlp_ylp_vault"),
  INSURANCE: Buffer.from("insurance"),
  FUTARCHY_AUTHORITY: Buffer.from("futarchy_authority"),
  REFERRAL_PARTNER: Buffer.from("referral_partner"),
  REFERRAL_ACCRUAL: Buffer.from("referral_accrual"),
  METADATA: Buffer.from("metadata"),
} as const;

function normalizeParamsHash(paramsHash: Uint8Array | Buffer | number[]): Buffer {
  const hash = Buffer.from(paramsHash);
  if (hash.length !== 32) {
    throw new Error(`paramsHash must be 32 bytes, got ${hash.length}`);
  }
  return hash;
}

/**
 * Derive Futarchy Authority PDA address
 */
export function deriveFutarchyAuthorityAddress(): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([SEEDS.FUTARCHY_AUTHORITY], DUSK_PROGRAM_ID);
}

/** Derive the protocol-wide referral partner for a referrer authority. */
export function deriveReferralPartnerAddress(authority: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.REFERRAL_PARTNER, authority.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

/** Derive claimable referral interest for one partner, market, and debt mint. */
export function deriveReferralAccrualAddress(
  referralPartner: PublicKey,
  market: PublicKey,
  assetMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [
      SEEDS.REFERRAL_ACCRUAL,
      referralPartner.toBuffer(),
      market.toBuffer(),
      assetMint.toBuffer(),
    ],
    DUSK_PROGRAM_ID
  );
}

/**
 * Derive Dusk market PDA address
 */
export function deriveMarketAddress(
  baseMint: PublicKey,
  quoteMint: PublicKey,
  paramsHash: Uint8Array | Buffer | number[]
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [
      SEEDS.MARKET_V2,
      baseMint.toBuffer(),
      quoteMint.toBuffer(),
      normalizeParamsHash(paramsHash),
    ],
    DUSK_PROGRAM_ID
  );
}

/**
 * Derive a Metaplex Token Metadata PDA for a mint.
 */
export function deriveTokenMetadataAddress(mint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.METADATA, TOKEN_METADATA_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    TOKEN_METADATA_PROGRAM_ID
  );
}

/**
 * Derive market reserve vault PDA address
 */
export function deriveMarketReserveVaultAddress(
  market: PublicKey,
  reserveMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.MARKET_RESERVE_VAULT, market.toBuffer(), reserveMint.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

/**
 * Derive market collateral vault PDA address
 */
export function deriveMarketCollateralVaultAddress(
  market: PublicKey,
  collateralMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.MARKET_COLLATERAL_VAULT, market.toBuffer(), collateralMint.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

/**
 * Derive market fee vault PDA address
 */
export function deriveMarketFeeVaultAddress(
  market: PublicKey,
  feeMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.MARKET_FEE_VAULT, market.toBuffer(), feeMint.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

/**
 * Derive market interest vault PDA address
 */
export function deriveMarketInterestVaultAddress(
  market: PublicKey,
  interestMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.MARKET_INTEREST_VAULT, market.toBuffer(), interestMint.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

/**
 * Derive borrow position PDA address
 */
export function deriveBorrowPositionAddress(
  market: PublicKey,
  positionId: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.BORROW_POSITION, market.toBuffer(), positionId.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

/**
 * Derive leverage position PDA address
 */
export function deriveLeveragePositionAddress(
  market: PublicKey,
  positionId: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.LEVERAGE_POSITION, market.toBuffer(), positionId.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

export type YieldTokenKind = "ylp" | "hlp" | 0 | 1;

function yieldTokenKindCode(tokenKind: YieldTokenKind): number {
  if (tokenKind === "ylp" || tokenKind === 0) return 0;
  if (tokenKind === "hlp" || tokenKind === 1) return 1;
  throw new Error(`Unsupported yield token kind: ${tokenKind}`);
}

/**
 * Derive yield-account PDA address for yLP/hLP revenue checkpoints.
 */
export function deriveYieldAccountAddress(
  market: PublicKey,
  owner: PublicKey,
  assetMint: PublicKey,
  tokenKind: YieldTokenKind
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [
      SEEDS.YIELD_ACCOUNT,
      market.toBuffer(),
      owner.toBuffer(),
      assetMint.toBuffer(),
      Buffer.from([yieldTokenKindCode(tokenKind)]),
    ],
    DUSK_PROGRAM_ID
  );
}

export interface YieldTransferHookAccountsArgs {
  lpMint: PublicKey;
  market: PublicKey;
  sourceOwner: PublicKey;
  destinationOwner: PublicKey;
  assetMint: PublicKey;
  tokenKind: YieldTokenKind;
}

export const TRANSFER_HOOK_EXECUTE_DISCRIMINATOR = Buffer.from([
  105, 37, 101, 197, 75, 251, 102, 26,
]);

const TRANSFER_HOOK_EXTRA_ACCOUNT_META_SIZE = 35;
const TRANSFER_HOOK_TOKEN_ACCOUNT_OWNER_OFFSET = 32;
const TRANSFER_HOOK_SOURCE_ACCOUNT_INDEX = 0;
const TRANSFER_HOOK_DESTINATION_ACCOUNT_INDEX = 2;
const TRANSFER_HOOK_MARKET_ACCOUNT_INDEX = 5;
const TRANSFER_HOOK_ASSET_MINT_ACCOUNT_INDEX = 6;
const TRANSFER_HOOK_QUOTE_ASSET_MINT_ACCOUNT_INDEX = 7;

type TransferHookSeed =
  | { kind: "literal"; bytes: Uint8Array | Buffer | number[] }
  | { kind: "instructionData"; index: number; length: number }
  | { kind: "accountKey"; index: number }
  | { kind: "accountData"; accountIndex: number; dataIndex: number; length: number };

interface TransferHookValidationMeta {
  discriminator: number;
  addressConfig: Buffer;
  isSigner: boolean;
  isWritable: boolean;
}

export interface YieldTransferHookValidationArgs {
  market: PublicKey;
  assetMint: PublicKey;
  tokenKind: YieldTokenKind;
}

export interface YlpTransferHookValidationArgs {
  market: PublicKey;
  baseMint: PublicKey;
  quoteMint: PublicKey;
}

/**
 * Derive the standard Token-2022 transfer-hook validation PDA for a Dusk LP mint.
 */
export function deriveYieldTransferHookValidationAddress(lpMint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("extra-account-metas"), lpMint.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

function assertU8(value: number, label: string): void {
  if (!Number.isInteger(value) || value < 0 || value > 255) {
    throw new Error(`${label} must fit in a u8, got ${value}`);
  }
}

function packTransferHookSeedConfig(seeds: TransferHookSeed[]): Buffer {
  const config = Buffer.alloc(32);
  let offset = 0;
  const writeByte = (value: number, label: string) => {
    assertU8(value, label);
    if (offset >= config.length) {
      throw new Error("transfer-hook seed config exceeds 32 bytes");
    }
    config[offset] = value;
    offset += 1;
  };

  for (const seed of seeds) {
    switch (seed.kind) {
      case "literal": {
        const bytes = Buffer.from(seed.bytes);
        assertU8(bytes.length, "literal seed length");
        writeByte(1, "literal seed discriminator");
        writeByte(bytes.length, "literal seed length");
        if (offset + bytes.length > config.length) {
          throw new Error("transfer-hook seed config exceeds 32 bytes");
        }
        bytes.copy(config, offset);
        offset += bytes.length;
        break;
      }
      case "instructionData":
        writeByte(2, "instruction-data seed discriminator");
        writeByte(seed.index, "instruction-data seed index");
        writeByte(seed.length, "instruction-data seed length");
        break;
      case "accountKey":
        writeByte(3, "account-key seed discriminator");
        writeByte(seed.index, "account-key seed index");
        break;
      case "accountData":
        writeByte(4, "account-data seed discriminator");
        writeByte(seed.accountIndex, "account-data seed account index");
        writeByte(seed.dataIndex, "account-data seed data index");
        writeByte(seed.length, "account-data seed length");
        break;
    }
  }

  return config;
}

function staticTransferHookMeta(meta: AccountMeta): TransferHookValidationMeta {
  return {
    discriminator: 0,
    addressConfig: meta.pubkey.toBuffer(),
    isSigner: meta.isSigner,
    isWritable: meta.isWritable,
  };
}

function pdaTransferHookMeta(
  seeds: TransferHookSeed[],
  isWritable: boolean
): TransferHookValidationMeta {
  return {
    discriminator: 1,
    addressConfig: packTransferHookSeedConfig(seeds),
    isSigner: false,
    isWritable,
  };
}

function yieldAccountTransferHookSeeds(
  ownerTokenAccountIndex: number,
  tokenKind: YieldTokenKind,
  assetMintAccountIndex = TRANSFER_HOOK_ASSET_MINT_ACCOUNT_INDEX
): TransferHookSeed[] {
  return [
    { kind: "literal", bytes: SEEDS.YIELD_ACCOUNT },
    { kind: "accountKey", index: TRANSFER_HOOK_MARKET_ACCOUNT_INDEX },
    {
      kind: "accountData",
      accountIndex: ownerTokenAccountIndex,
      dataIndex: TRANSFER_HOOK_TOKEN_ACCOUNT_OWNER_OFFSET,
      length: 32,
    },
    { kind: "accountKey", index: assetMintAccountIndex },
    { kind: "literal", bytes: [yieldTokenKindCode(tokenKind)] },
  ];
}

function encodeTransferHookValidationAccountData(extraMetas: TransferHookValidationMeta[]): Buffer {
  const podSliceLength = 4 + extraMetas.length * TRANSFER_HOOK_EXTRA_ACCOUNT_META_SIZE;
  const data = Buffer.alloc(8 + 4 + podSliceLength);
  TRANSFER_HOOK_EXECUTE_DISCRIMINATOR.copy(data, 0);
  data.writeUInt32LE(podSliceLength, 8);
  data.writeUInt32LE(extraMetas.length, 12);

  let offset = 16;
  for (const meta of extraMetas) {
    data[offset] = meta.discriminator;
    offset += 1;
    meta.addressConfig.copy(data, offset);
    offset += 32;
    data[offset] = meta.isSigner ? 1 : 0;
    offset += 1;
    data[offset] = meta.isWritable ? 1 : 0;
    offset += 1;
  }
  return data;
}

/**
 * Encode a static Token-2022 transfer-hook validation account for Execute.
 *
 * The returned bytes use the SPL TLV shape expected by Token-2022:
 * execute discriminator, pod-slice length, count, then static account metas.
 */
export function buildYieldTransferHookValidationAccountData(extraMetas: AccountMeta[]): Buffer {
  return encodeTransferHookValidationAccountData(extraMetas.map(staticTransferHookMeta));
}

/**
 * Encode the production Token-2022 transfer-hook validation account for Dusk
 * yLP/hLP mints.
 *
 * The validation state resolves static market/asset-mint accounts and derives
 * source/destination YieldAccount PDAs from the source and destination token
 * account owners observed by Token-2022 during Execute.
 */
export function buildYieldTransferHookYieldValidationAccountData({
  market,
  assetMint,
  tokenKind,
}: YieldTransferHookValidationArgs): Buffer {
  return encodeTransferHookValidationAccountData([
    staticTransferHookMeta({ pubkey: market, isSigner: false, isWritable: false }),
    staticTransferHookMeta({ pubkey: assetMint, isSigner: false, isWritable: false }),
    pdaTransferHookMeta(
      yieldAccountTransferHookSeeds(TRANSFER_HOOK_SOURCE_ACCOUNT_INDEX, tokenKind),
      true
    ),
    pdaTransferHookMeta(
      yieldAccountTransferHookSeeds(TRANSFER_HOOK_DESTINATION_ACCOUNT_INDEX, tokenKind),
      true
    ),
  ]);
}

export function buildYlpTransferHookValidationAccountData({
  market,
  baseMint,
  quoteMint,
}: YlpTransferHookValidationArgs): Buffer {
  return encodeTransferHookValidationAccountData([
    staticTransferHookMeta({ pubkey: market, isSigner: false, isWritable: false }),
    staticTransferHookMeta({ pubkey: baseMint, isSigner: false, isWritable: false }),
    staticTransferHookMeta({ pubkey: quoteMint, isSigner: false, isWritable: false }),
    pdaTransferHookMeta(
      yieldAccountTransferHookSeeds(TRANSFER_HOOK_SOURCE_ACCOUNT_INDEX, "ylp"),
      true
    ),
    pdaTransferHookMeta(
      yieldAccountTransferHookSeeds(TRANSFER_HOOK_DESTINATION_ACCOUNT_INDEX, "ylp"),
      true
    ),
    pdaTransferHookMeta(
      yieldAccountTransferHookSeeds(
        TRANSFER_HOOK_SOURCE_ACCOUNT_INDEX,
        "ylp",
        TRANSFER_HOOK_QUOTE_ASSET_MINT_ACCOUNT_INDEX
      ),
      true
    ),
    pdaTransferHookMeta(
      yieldAccountTransferHookSeeds(
        TRANSFER_HOOK_DESTINATION_ACCOUNT_INDEX,
        "ylp",
        TRANSFER_HOOK_QUOTE_ASSET_MINT_ACCOUNT_INDEX
      ),
      true
    ),
  ]);
}

/**
 * Build Dusk yLP/hLP transfer-hook extra account metas.
 *
 * Token-2022 passes the source token account, LP mint, destination token
 * account, and transfer authority as the base hook accounts. Omnipair Dusk (v2) needs
 * the market, underlying asset mint, canonical source/destination yield
 * accounts, hook program, and standard validation PDA as extra metas so the
 * hook can checkpoint revenue before the balance move is finalized.
 */
export function buildYieldTransferHookAccountMetas({
  lpMint,
  market,
  sourceOwner,
  destinationOwner,
  assetMint,
  tokenKind,
}: YieldTransferHookAccountsArgs): AccountMeta[] {
  const sourceYieldAccount = deriveYieldAccountAddress(
    market,
    sourceOwner,
    assetMint,
    tokenKind
  )[0];
  const destinationYieldAccount = deriveYieldAccountAddress(
    market,
    destinationOwner,
    assetMint,
    tokenKind
  )[0];
  const validationAccount = deriveYieldTransferHookValidationAddress(lpMint)[0];

  return [
    { pubkey: market, isSigner: false, isWritable: false },
    { pubkey: assetMint, isSigner: false, isWritable: false },
    { pubkey: sourceYieldAccount, isSigner: false, isWritable: true },
    { pubkey: destinationYieldAccount, isSigner: false, isWritable: true },
    { pubkey: DUSK_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: validationAccount, isSigner: false, isWritable: false },
  ];
}

export function buildYlpTransferHookAccountMetas({
  lpMint,
  market,
  sourceOwner,
  destinationOwner,
  baseMint,
  quoteMint,
}: Omit<YieldTransferHookAccountsArgs, "assetMint" | "tokenKind"> & {
  baseMint: PublicKey;
  quoteMint: PublicKey;
}): AccountMeta[] {
  const sourceBaseYieldAccount = deriveYieldAccountAddress(
    market,
    sourceOwner,
    baseMint,
    "ylp"
  )[0];
  const destinationBaseYieldAccount = deriveYieldAccountAddress(
    market,
    destinationOwner,
    baseMint,
    "ylp"
  )[0];
  const sourceQuoteYieldAccount = deriveYieldAccountAddress(
    market,
    sourceOwner,
    quoteMint,
    "ylp"
  )[0];
  const destinationQuoteYieldAccount = deriveYieldAccountAddress(
    market,
    destinationOwner,
    quoteMint,
    "ylp"
  )[0];
  const validationAccount = deriveYieldTransferHookValidationAddress(lpMint)[0];

  return [
    { pubkey: market, isSigner: false, isWritable: false },
    { pubkey: baseMint, isSigner: false, isWritable: false },
    { pubkey: quoteMint, isSigner: false, isWritable: false },
    { pubkey: sourceBaseYieldAccount, isSigner: false, isWritable: true },
    { pubkey: destinationBaseYieldAccount, isSigner: false, isWritable: true },
    { pubkey: sourceQuoteYieldAccount, isSigner: false, isWritable: true },
    { pubkey: destinationQuoteYieldAccount, isSigner: false, isWritable: true },
    { pubkey: DUSK_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: validationAccount, isSigner: false, isWritable: false },
  ];
}

/**
 * Derive canonical aggregate hLP vault yLP token-account PDA.
 */
export function deriveHlpYlpVaultAddress(
  market: PublicKey,
  targetHlpMint: PublicKey,
  ylpMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [
      SEEDS.HLP_YLP_VAULT,
      market.toBuffer(),
      targetHlpMint.toBuffer(),
      ylpMint.toBuffer(),
    ],
    DUSK_PROGRAM_ID
  );
}

/**
 * Derive insurance PDA address
 */
export function deriveInsuranceAddress(
  market: PublicKey,
  assetMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.INSURANCE, market.toBuffer(), assetMint.toBuffer()],
    DUSK_PROGRAM_ID
  );
}
