import * as fs from "fs";
import * as path from "path";
import { fileURLToPath } from "url";
import anchor from "@coral-xyz/anchor";
import {
  ACCOUNT_SIZE,
  AccountLayout,
  AccountState,
  createBurnCheckedInstruction,
  createAccount,
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeAccount3Instruction,
  createInitializeMintInstruction,
  createInitializeTransferFeeConfigInstruction,
  createMint,
  createTransferCheckedWithTransferHookInstruction,
  ExtensionType,
  getAccount,
  getAssociatedTokenAddressSync,
  getExtraAccountMetaAddress,
  getMint,
  getMintLen,
  getTransferHook,
  mintTo,
  NATIVE_MINT,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createInitializeTransferHookInstruction,
  createUpdateTransferHookInstruction,
} from "@solana/spl-token";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Transaction,
} from "@solana/web3.js";
import { expect } from "chai";
import { ComputeBudget, FeatureSet, LiteSVM } from "litesvm";
import {
  buildLpTransferHookAccountMetas,
  buildYieldTransferHookValidationAccountData,
  deriveFutarchyAuthorityAddress,
  deriveHlpYlpVaultAddress,
  deriveInsuranceAddress,
  deriveMarketAddress,
  deriveMarketCollateralVaultAddress,
  deriveMarketInterestVaultAddress,
  deriveMarketReserveVaultAddress,
  deriveReferralAccrualAddress,
  deriveReferralPartnerAddress,
  deriveBorrowPositionAddress,
  deriveLeveragePositionAddress,
  deriveYieldAccountAddress,
  deriveYieldTransferHookValidationAddress,
  deriveTokenMetadataAddress,
  TOKEN_METADATA_PROGRAM_ID,
  TRANSFER_HOOK_EXECUTE_DISCRIMINATOR,
} from "../packages/dusk-sdk/src/constants.js";
import {
  decodePreviewAddLiquidityReturnData,
  decodePreviewBorrowCapacityReturnData,
  decodePreviewBorrowPositionReturnData,
  decodePreviewMarketReturnData,
  decodePreviewSwapReturnData,
} from "../packages/dusk-sdk/src/preview.js";
import { resolveTransferHookAccountMetas } from "../packages/dusk-sdk/src/referral.js";
import { DuskWrite } from "../packages/dusk-sdk/src/write.js";
import { LiteSVMConnection } from "./utils/litesvm-connection.js";
import {
  assertRequiredSwapComputeScenarios,
  getCoverageReport,
  LITESVM_COMPUTE_UNIT_LIMIT,
  recordExternalTransferHookComputeUnits,
  recordSwapComputeScenario,
  recordTransactionComputeUnits,
  trackV2Instruction,
} from "./utils/instruction-coverage.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const { AnchorProvider, BN, Program, Wallet } = anchor;
const DUSK_PROGRAM_ID = new PublicKey("358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv");
const LEVERAGE_DELEGATE_PROGRAM_ID = new PublicKey(
  "EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp"
);
const REFERRAL_TRANSFER_HOOK_PROGRAM_ID = new PublicKey(
  "noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV"
);
const idl = JSON.parse(
  fs.readFileSync(path.join(__dirname, "../target/idl/dusk.json"), "utf-8")
);
const leverageDelegateIdl = JSON.parse(
  fs.readFileSync(path.join(__dirname, "../target/idl/leverage_delegate.json"), "utf-8")
);
const accountCoder = new anchor.BorshAccountsCoder(idl);
const REDUCE_ONLY_EMERGENCY_AUTHORITY = new PublicKey(
  "3YL87sTCrHMB6DYKorE9CCN4dL45kZPahoREcMLDY6QV"
);
const BPF_LOADER_UPGRADEABLE_PROGRAM_ID = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111"
);
const LEVERAGE_COLLATERAL_VAULT_SEED = Buffer.from("leverage_collateral");
const LEVERAGE_DELEGATION_SEED = Buffer.from("leverage_delegation_v2");
const LEVERAGE_ORDER_SEED = Buffer.from("leverage_order");
const LEVERAGE_DELEGATE_CLOSE = 1;
const ORDER_KIND_TAKE_PROFIT = 1;
const FEATURE_PROGRAM_ID = new PublicKey(
  "Feature111111111111111111111111111111111111"
);
// LiteSVM issues #396 / PR #352: stricter ABI/runtime validation narrows the
// native JIT corruption surface but does not eliminate it for Dusk's x86-64
// path. CI therefore executes this suite on ARM64, where LiteSVM uses the SBF
// interpreter. Preserve LiteSVM's default feature snapshot instead of enabling
// unrelated future features, then rebuild the runtime around this addition.
const STRICT_RUNTIME_FEATURE = new PublicKey(
  "Eoh7e1sDqtyPtuiWAhBNSJinvtJWTTDgeUMRi3RF8zWS"
).toBytes();

function createLiteSvm(computeBudget: ComputeBudget): LiteSVM {
  const svm = new LiteSVM();
  const featureSet = new FeatureSet();
  let defaultFeatureCount = 0;
  for (const featureId of featureSet.getInactiveFeatures()) {
    const featureAccount = svm.getAccount(new PublicKey(featureId));
    if (
      featureAccount?.owner.equals(FEATURE_PROGRAM_ID) &&
      featureAccount.data.length === 9 &&
      featureAccount.data[0] === 1
    ) {
      featureSet.activate(
        featureId,
        Buffer.from(featureAccount.data).readBigUInt64LE(1)
      );
      defaultFeatureCount += 1;
    }
  }
  featureSet.activate(STRICT_RUNTIME_FEATURE, 0n);
  if (
    defaultFeatureCount !== 219 ||
    featureSet.getActiveFeaturesCount() !== 220 ||
    !featureSet.isActive(STRICT_RUNTIME_FEATURE) ||
    featureSet.activatedSlot(STRICT_RUNTIME_FEATURE) !== 0n
  ) {
    throw new Error(
      "LiteSVM stricter ABI/runtime constraints must be active for deterministic tests"
    );
  }
  return svm
    .withFeatureSet(featureSet)
    .withComputeBudget(computeBudget)
    .withBuiltins()
    .withDefaultPrograms()
    .withPrecompiles();
}

function deriveLeverageCollateralVaultAddress(
  market: PublicKey,
  collateralMint: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [LEVERAGE_COLLATERAL_VAULT_SEED, market.toBuffer(), collateralMint.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

function seedLegacyTokenAccount(svm: LiteSVM, address: PublicKey, mint: PublicKey, owner: PublicKey) {
  const data = Buffer.alloc(AccountLayout.span);
  AccountLayout.encode(
    {
      mint,
      owner,
      amount: 0n,
      delegateOption: 0,
      delegate: PublicKey.default,
      state: AccountState.Initialized,
      isNativeOption: 0,
      isNative: 0n,
      delegatedAmount: 0n,
      closeAuthorityOption: 0,
      closeAuthority: PublicKey.default,
    },
    data
  );
  svm.setAccount(address, {
    lamports: Number(svm.minimumBalanceForRentExemption(BigInt(data.length))),
    data: new Uint8Array(data),
    owner: TOKEN_PROGRAM_ID,
    executable: false,
    rentEpoch: 0,
  });
}

function deriveLeverageDelegationAddress(position: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [LEVERAGE_DELEGATION_SEED, position.toBuffer()],
    DUSK_PROGRAM_ID
  );
}

function deriveLeverageOrderAddress(
  position: PublicKey,
  owner: PublicKey,
  orderId: anchor.BN
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [
      LEVERAGE_ORDER_SEED,
      position.toBuffer(),
      owner.toBuffer(),
      orderId.toArrayLike(Buffer, "le", 8),
    ],
    LEVERAGE_DELEGATE_PROGRAM_ID
  );
}

function leverageDelegateProgramPath() {
  const programPath = path.join(__dirname, "../target/deploy/leverage_delegate.so");
  if (!fs.existsSync(programPath)) {
    throw new Error(
      `Leverage delegate program file not found at ${programPath}. Run anchor build -p leverage_delegate.`
    );
  }
  return programPath;
}

function marketConfig() {
  return {
    swapFeeBps: 30,
    divergenceFeeShareCapBps: 0,
    volatilityFeeShareCapBps: 0,
    targetHlpLeverageBps: 20_000,
    settlementDivergenceBps: 500,
    emaHalfLifeMs: new BN(60_000),
    directionalEmaHalfLifeMs: new BN(60_000),
    qEmaHalfLifeMs: new BN(60_000),
    maxDailyBorrowBps: 2_000,
    globalHealthContributionCapBps: 15_000,
    borrowMarketHealthFloorBps: 11_000,
    amm: {
      rangeWidthNad: new BN(0),
      concentratedLiquidityShareNad: new BN(0),
      centerEmaHalfLifeMs: new BN(60_000),
      volatilityHalfLifeMs: new BN(60_000),
      adjustmentThresholdNad: new BN(0),
      adjustmentStepNad: new BN(0),
      minAdjustmentIntervalSlots: new BN(0),
      volatilityShockCapNad: new BN(0),
      volatilityCapNad: new BN(0),
      divergenceFeeCoefficientNad: new BN(0),
      volatilityFeeCoefficientNad: new BN(0),
      swapFeeCollectMode: 0,
      compoundingFeeBps: 0,
      launchFeeStartBps: 0,
      launchFeeDurationSeconds: new BN(0),
      launchFeeDecayMode: 0,
      launchMarketPriceStepBps: 0,
      launchMarketNumberOfPeriods: 0,
      launchMarketReductionFactorBps: 0,
      launchRateLimitAsset: 0,
      launchRateLimitReferenceNad: new BN(0),
      launchRateLimitIncrementBps: 0,
      launchRateLimitMaxFeeBps: 0,
      launchRateLimitDurationSeconds: new BN(0),
      reserved: [],
    },
    irm: {
      targetUtilizationBps: 7_000,
      curveSteepnessNad: new BN(4_000_000_000),
      adjustmentSpeedPerYear: new BN(20),
    },
    startTime: new BN(0),
  };
}

describe("Omnipair V2 (Dusk) final model smoke", () => {
  let svm: LiteSVM;
  let connection: LiteSVMConnection;
  let payer: Keypair;
  let program: any;
  let leverageDelegateProgram: any;
  let teamTreasury: PublicKey;
  let teamTreasuryWsolAccount: PublicKey;
  let futarchyAuthority: PublicKey;

  before(async () => {
    const computeBudget = new ComputeBudget();
    computeBudget.computeUnitLimit = LITESVM_COMPUTE_UNIT_LIMIT;
    // Keep LiteSVM's cluster-default 32 KiB heap. A path that needs a larger
    // frame must request it in its transaction so the request and its CU are
    // visible in the named measurement instead of being granted globally.
    svm = createLiteSvm(computeBudget);
    svm.warpToSlot(1n);
    const programPath = path.join(__dirname, "../target/deploy/dusk.so");
    if (!fs.existsSync(programPath)) {
      throw new Error(`Program file not found at ${programPath}`);
    }
    svm.addProgramFromFile(DUSK_PROGRAM_ID, programPath);
    svm.addProgramFromFile(LEVERAGE_DELEGATE_PROGRAM_ID, leverageDelegateProgramPath());
    svm.addProgramFromFile(
      REFERRAL_TRANSFER_HOOK_PROGRAM_ID,
      path.join(__dirname, "../target/deploy/referral_transfer_hook.so")
    );
    svm.addProgramFromFile(
      TOKEN_METADATA_PROGRAM_ID,
      path.join(__dirname, "../target/deploy/token_metadata_fixture.so")
    );
    connection = new LiteSVMConnection(svm);

    payer = Keypair.generate();
    await connection.requestAirdrop(payer.publicKey, 20 * LAMPORTS_PER_SOL);
    const provider = new AnchorProvider(connection as any, new Wallet(payer) as any, {});
    program = new Program({ ...idl, accounts: [] } as any, provider as any);
    leverageDelegateProgram = new Program(
      { ...leverageDelegateIdl, accounts: [] } as any,
      provider as any
    );

    teamTreasury = Keypair.generate().publicKey;
    const teamTreasuryWsol = Keypair.generate();
    teamTreasuryWsolAccount = teamTreasuryWsol.publicKey;
    await connection.sendTransaction(
      new Transaction().add(
        SystemProgram.createAccount({
          fromPubkey: payer.publicKey,
          newAccountPubkey: teamTreasuryWsolAccount,
          lamports: await connection.getMinimumBalanceForRentExemption(ACCOUNT_SIZE),
          space: ACCOUNT_SIZE,
          programId: TOKEN_PROGRAM_ID,
        }),
        createInitializeAccount3Instruction(
          teamTreasuryWsolAccount,
          NATIVE_MINT,
          teamTreasury,
          TOKEN_PROGRAM_ID
        )
      ),
      [payer, teamTreasuryWsol]
    );

    await seedFutarchyAuthority();
  });

  after(() => {
    getCoverageReport();
    if (process.env.DUSK_REQUIRE_COMPLETE_CU_BASELINE === "1") {
      assertRequiredSwapComputeScenarios();
    }
  });

  beforeEach(async () => {
    svm.expireBlockhash();
    await resetFutarchyDefaults();
  });

  async function seedFutarchyAuthority() {
    const [authority, bump] = deriveFutarchyAuthorityAddress();
    futarchyAuthority = authority;
    const auctionRecipients = {
      treasury: payer.publicKey,
      staking_vault: payer.publicKey,
      treasury_bps: 10_000,
      staking_vault_bps: 0,
    };
    const auctionParams = {
      start_multiplier_bps: 12_000,
      floor_multiplier_bps: 8_000,
      duration_slots: new BN(216_000),
      max_reference_age_slots: new BN(21_600),
    };
    const auctionConfig = {
      accepted_mint: NATIVE_MINT,
      recipients: auctionRecipients,
      params: auctionParams,
    };
    const data = await accountCoder.encode("FutarchyAuthority", {
      version: 1,
      authority: payer.publicKey,
      recipients: {
        futarchy_treasury: payer.publicKey,
        buybacks_vault: payer.publicKey,
        team_treasury: teamTreasury,
      },
      revenue_share: {
        swap_bps: 0,
        interest_bps: 0,
      },
      max_referral_interest_share_bps: 5_000,
      revenue_distribution: {
        futarchy_treasury_bps: 0,
        buybacks_vault_bps: 0,
        team_treasury_bps: 10_000,
      },
      protocol_auction_split: {
        fee_auction_bps: 10_000,
        buyback_auction_bps: 0,
      },
      fee_auction: auctionConfig,
      buyback_auction: auctionConfig,
      global_reduce_only: false,
      bump,
    });
    svm.setAccount(futarchyAuthority, {
      lamports: Number(svm.minimumBalanceForRentExemption(BigInt(data.length))),
      data: new Uint8Array(data),
      owner: DUSK_PROGRAM_ID,
      executable: false,
      rentEpoch: 0,
    });
  }

  async function resetFutarchyDefaults() {
    await seedFutarchyAuthority();
  }

  async function initializeYieldAccounts(
    fixture: {
      market: PublicKey;
      ylpMint: PublicKey;
      baseHlpMint: PublicKey;
      quoteHlpMint: PublicKey;
      baseMint: PublicKey;
      quoteMint: PublicKey;
    },
    owner: PublicKey,
    lpMint: PublicKey,
    tokenKind: "ylp" | "hlp",
    forceProgramReentry = false,
  ) {
    const baseYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      owner,
      lpMint,
      fixture.baseMint,
      tokenKind
    )[0];
    const quoteYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      owner,
      lpMint,
      fixture.quoteMint,
      tokenKind
    )[0];
    const yieldAccountDefinition = idl.accounts.find(
      (account: { name: string }) => account.name === "YieldAccount"
    );
    expect(yieldAccountDefinition).to.not.equal(undefined);
    const expectedSize = accountCoder.size("YieldAccount");
    const discriminator = Buffer.from(yieldAccountDefinition.discriminator);
    const yieldAccountsReady = [baseYieldAccount, quoteYieldAccount].every((address) => {
      const account = svm.getAccount(address);
      return (
        account !== null &&
        account.owner.equals(DUSK_PROGRAM_ID) &&
        account.data.length === expectedSize &&
        Buffer.from(account.data.subarray(0, discriminator.length)).equals(discriminator)
      );
    });
    if (yieldAccountsReady && !forceProgramReentry) {
      return { baseYieldAccount, quoteYieldAccount };
    }
    const tx = await program.methods
      .initializeYieldAccounts({
        owner,
        tokenKind: tokenKind === "ylp" ? { ylp: {} } : { hlp: {} },
      })
      .accounts({
        payer: payer.publicKey,
        market: fixture.market,
        lpMint,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        baseYieldAccount,
        quoteYieldAccount,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    trackV2Instruction("initializeYieldAccounts");
    return { baseYieldAccount, quoteYieldAccount };
  }

  async function initializeLpTransferHook(
    fixture: { market: PublicKey },
    lpMint: PublicKey,
  ) {
    const validationAccount = deriveYieldTransferHookValidationAddress(lpMint)[0];
    const tx = await program.methods
      .initializeLpTransferHook()
      .accounts({
        payer: payer.publicKey,
        market: fixture.market,
        lpMint,
        validationAccount,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    trackV2Instruction("initializeLpTransferHook");
    return validationAccount;
  }

  async function createHookedLpMint(
    authority: PublicKey,
    decimals = 6,
    mint = Keypair.generate()
  ) {
    const mintLen = getMintLen([ExtensionType.TransferHook]);
    await connection.sendTransaction(
      new Transaction().add(
        SystemProgram.createAccount({
          fromPubkey: payer.publicKey,
          newAccountPubkey: mint.publicKey,
          lamports: await connection.getMinimumBalanceForRentExemption(mintLen),
          space: mintLen,
          programId: TOKEN_2022_PROGRAM_ID,
        }),
        createInitializeTransferHookInstruction(
          mint.publicKey,
          PublicKey.default,
          DUSK_PROGRAM_ID,
          TOKEN_2022_PROGRAM_ID
        ),
        createInitializeMintInstruction(
          mint.publicKey,
          decimals,
          authority,
          null,
          TOKEN_2022_PROGRAM_ID
        )
      ),
      [payer, mint]
    );
    return mint.publicKey;
  }

  async function createDeferredHookMint(authority: PublicKey, decimals = 6) {
    const mint = Keypair.generate();
    const mintLen = getMintLen([ExtensionType.TransferHook]);
    await connection.sendTransaction(
      new Transaction().add(
        SystemProgram.createAccount({
          fromPubkey: payer.publicKey,
          newAccountPubkey: mint.publicKey,
          lamports: await connection.getMinimumBalanceForRentExemption(mintLen),
          space: mintLen,
          programId: TOKEN_2022_PROGRAM_ID,
        }),
        createInitializeTransferHookInstruction(
          mint.publicKey,
          payer.publicKey,
          PublicKey.default,
          TOKEN_2022_PROGRAM_ID
        ),
        createInitializeMintInstruction(
          mint.publicKey,
          decimals,
          authority,
          null,
          TOKEN_2022_PROGRAM_ID
        )
      ),
      [payer, mint]
    );
    return mint.publicKey;
  }

  async function createTransferFeeMint(
    authority: PublicKey,
    decimals = 6,
    transferFeeBasisPoints = 100,
    maximumFee = 10_000n
  ) {
    const mint = Keypair.generate();
    const mintLen = getMintLen([ExtensionType.TransferFeeConfig]);
    await connection.sendTransaction(
      new Transaction().add(
        SystemProgram.createAccount({
          fromPubkey: payer.publicKey,
          newAccountPubkey: mint.publicKey,
          lamports: await connection.getMinimumBalanceForRentExemption(mintLen),
          space: mintLen,
          programId: TOKEN_2022_PROGRAM_ID,
        }),
        createInitializeTransferFeeConfigInstruction(
          mint.publicKey,
          payer.publicKey,
          payer.publicKey,
          transferFeeBasisPoints,
          maximumFee,
          TOKEN_2022_PROGRAM_ID
        ),
        createInitializeMintInstruction(
          mint.publicKey,
          decimals,
          authority,
          null,
          TOKEN_2022_PROGRAM_ID
        )
      ),
      [payer, mint]
    );
    return mint.publicKey;
  }

  function eventAuthority() {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("__event_authority")],
      DUSK_PROGRAM_ID
    )[0];
  }

  function cpiEvents(transaction: Transaction): Array<{ name: string; data: any }> {
    if (!transaction.signature) throw new Error("Transaction has no signature");
    const result = svm.getTransaction(transaction.signature);
    if (!result) throw new Error("LiteSVM transaction metadata was not retained");
    if (typeof (result as any).err === "function") {
      throw new Error(`Cannot decode events from failed transaction: ${(result as any).err()}`);
    }

    const eventTag = Buffer.alloc(8);
    eventTag.writeBigUInt64LE(0x1d9acb512ea545e4n);
    const accountKeys = transaction.compileMessage().accountKeys;
    const events: Array<{ name: string; data: any }> = [];
    for (const group of (result as any).innerInstructions()) {
      for (const inner of group) {
        const instruction = inner.instruction();
        const invokedProgram = accountKeys[instruction.programIdIndex()];
        const data = Buffer.from(instruction.data());
        if (
          invokedProgram?.equals(DUSK_PROGRAM_ID) &&
          data.length > eventTag.length &&
          data.subarray(0, eventTag.length).equals(eventTag)
        ) {
          const event = program.coder.events.decode(
            data.subarray(eventTag.length).toString("base64")
          );
          if (!event) throw new Error("Dusk emitted an undecodable CPI event");
          events.push(event);
        }
      }
    }
    return events;
  }

  function cpiEvent(transaction: Transaction, name: string): any {
    const matches = cpiEvents(transaction).filter((event) => event.name === name);
    expect(matches, `${name} CPI event count`).to.have.length(1);
    return matches[0].data;
  }

  async function sendTransactionWithUncheckedSigners(
    transaction: Transaction,
    signers: Keypair[],
    uncheckedSigners: PublicKey[]
  ) {
    const { blockhash } = await connection.getLatestBlockhash();
    transaction.recentBlockhash = blockhash;
    transaction.feePayer = payer.publicKey;
    transaction.sign(...signers);
    for (const signer of uncheckedSigners) {
      transaction.addSignature(signer, Buffer.alloc(64));
    }

    svm.withSigverify(false);
    try {
      const result = svm.sendTransaction(transaction as any);
      if (result && typeof (result as any).err === "function") {
        const err = (result as any).err();
        if (err) {
          const meta = (result as any).meta?.();
          const prettyLogs = meta?.prettyLogs?.();
          throw new Error(`Transaction failed: ${err.toString?.() ?? err}\n${prettyLogs ?? ""}`);
        }
      }
      if (result && "err" in result && (result as any).err) {
        throw new Error(`Transaction failed: ${JSON.stringify((result as any).err)}`);
      }
      const computeUnits = (result as any)?.computeUnitsConsumed?.();
      if (computeUnits === undefined) {
        throw new Error("LiteSVM did not expose compute units for the submitted transaction");
      }
      recordTransactionComputeUnits(transaction, BigInt(computeUnits));
    } finally {
      svm.withSigverify(true);
    }
  }

  async function simulateReturnData(transaction: Transaction): Promise<Buffer> {
    const { blockhash } = await connection.getLatestBlockhash();
    transaction.recentBlockhash = blockhash;
    transaction.feePayer = payer.publicKey;
    transaction.sign(payer);

    const result = svm.simulateTransaction(transaction as any) as any;
    if (result && typeof result.err === "function") {
      const err = result.err();
      const prettyLogs = result.meta?.()?.prettyLogs?.() ?? result.prettyLogs?.();
      throw new Error(`Simulation failed: ${err?.toString?.() ?? err}\n${prettyLogs ?? ""}`);
    }
    const meta = result?.meta?.();
    const computeUnits = meta?.computeUnitsConsumed?.();
    if (computeUnits === undefined) {
      throw new Error("LiteSVM did not expose compute units for the simulated transaction");
    }
    recordTransactionComputeUnits(transaction, BigInt(computeUnits));
    const returnData = meta?.returnData?.();
    if (!returnData) {
      throw new Error(`Simulation did not return data\n${meta?.prettyLogs?.() ?? ""}`);
    }
    const programId = new PublicKey(returnData.programId());
    expect(programId.toString()).to.equal(DUSK_PROGRAM_ID.toString());
    return Buffer.from(returnData.data());
  }

  function upgradeableProgramData(authority: PublicKey) {
    const data = Buffer.alloc(45);
    data.writeUInt32LE(3, 0);
    data.writeBigUInt64LE(0n, 4);
    data[12] = 1;
    authority.toBuffer().copy(data, 13);
    return data;
  }

  async function createIsolatedProgram() {
    const isolatedSvm = createLiteSvm(new ComputeBudget());
    const programPath = path.join(__dirname, "../target/deploy/dusk.so");
    isolatedSvm.addProgramFromFile(DUSK_PROGRAM_ID, programPath);
    isolatedSvm.addProgramFromFile(
      TOKEN_METADATA_PROGRAM_ID,
      path.join(__dirname, "../target/deploy/token_metadata_fixture.so")
    );
    const isolatedConnection = new LiteSVMConnection(isolatedSvm);
    const isolatedPayer = Keypair.generate();
    await isolatedConnection.requestAirdrop(isolatedPayer.publicKey, 10 * LAMPORTS_PER_SOL);
    const isolatedProvider = new AnchorProvider(
      isolatedConnection as any,
      new Wallet(isolatedPayer) as any,
      {}
    );
    const isolatedProgram = new Program({ ...idl, accounts: [] } as any, isolatedProvider as any);
    return {
      isolatedSvm,
      isolatedConnection,
      isolatedPayer,
      isolatedProgram,
    };
  }

  async function initializeFinalMarket(paramsSeed: number, config = marketConfig()) {
    const baseMint = await createMint(connection as any, payer, payer.publicKey, null, 6);
    const quoteMint = await createMint(connection as any, payer, payer.publicKey, null, 6);
    if (process.env.DUSK_EXPECT_PRODUCTION_MINT_SUFFIXES === "1") {
      const paramsHash = Buffer.alloc(32, paramsSeed);
      const [market] = deriveMarketAddress(baseMint, quoteMint, paramsHash);
      // Public test-only keys satisfying production's exact yLP/hLP suffixes.
      const ylp = Keypair.fromSecretKey(
        Buffer.from("eVTnT13X34qGGAitZluS8ntUjqyI68RMUTB39YSJVE0IIgiaaycPLv/XpDFF2Hl7Iu1LxQhtESDsHxlf/0Z3tA==", "base64")
      );
      const baseHlp = Keypair.fromSecretKey(
        Buffer.from("mcqSo7BbkVpVF14Stzsj62CcpMdDYV30bPeHBB8XDrkJIUjEkHjWHig2YGwzxgyi/Y+Mdr/fIQR2Wzi/3SvArA==", "base64")
      );
      const quoteHlp = Keypair.fromSecretKey(
        Buffer.from("W1Quna5vQMVJqK+Ah0r6NBQGKcFZnGpN+Wgjj6DCrqMH9fJyiOBna+rsbArFZY2n2Tu5tzuh065P6+95avwDDA==", "base64")
      );
      await createHookedLpMint(market, 6, ylp);
      await createHookedLpMint(market, 6, baseHlp);
      await createHookedLpMint(market, 6, quoteHlp);
      return initializeFinalMarketWithMints(paramsSeed, baseMint, quoteMint, config, 6, {
        ylpMint: ylp.publicKey,
        baseHlpMint: baseHlp.publicKey,
        quoteHlpMint: quoteHlp.publicKey,
      });
    }
    return initializeFinalMarketWithMints(paramsSeed, baseMint, quoteMint, config);
  }

  async function initializeFinalMarketWithMints(
    paramsSeed: number,
    baseMint: PublicKey,
    quoteMint: PublicKey,
    config = marketConfig(),
    lpDecimals = 6,
    lpMints: Partial<{
      ylpMint: PublicKey;
      baseHlpMint: PublicKey;
      quoteHlpMint: PublicKey;
    }> = {}
  ) {
    const paramsHash = Buffer.alloc(32, paramsSeed);
    const [market] = deriveMarketAddress(baseMint, quoteMint, paramsHash);
    const ylpMint = lpMints.ylpMint ?? (await createHookedLpMint(market, lpDecimals));
    const baseHlpMint = lpMints.baseHlpMint ?? (await createHookedLpMint(market, lpDecimals));
    const quoteHlpMint = lpMints.quoteHlpMint ?? (await createHookedLpMint(market, lpDecimals));
    const ylpTokenMetadata = deriveTokenMetadataAddress(ylpMint)[0];
    const baseHlpTokenMetadata = deriveTokenMetadataAddress(baseHlpMint)[0];
    const quoteHlpTokenMetadata = deriveTokenMetadataAddress(quoteHlpMint)[0];
    const baseHlpYlpVault = deriveHlpYlpVaultAddress(market, baseHlpMint, ylpMint)[0];
    const quoteHlpYlpVault = deriveHlpYlpVaultAddress(market, quoteHlpMint, ylpMint)[0];
    const baseReserveVault = deriveMarketReserveVaultAddress(market, baseMint)[0];
    const quoteReserveVault = deriveMarketReserveVaultAddress(market, quoteMint)[0];
    const baseCollateralVault = deriveMarketCollateralVaultAddress(market, baseMint)[0];
    const quoteCollateralVault = deriveMarketCollateralVaultAddress(market, quoteMint)[0];
    const baseInsuranceVault = deriveInsuranceAddress(market, baseMint)[0];
    const quoteInsuranceVault = deriveInsuranceAddress(market, quoteMint)[0];
    const baseInterestVault = deriveMarketInterestVaultAddress(market, baseMint)[0];
    const quoteInterestVault = deriveMarketInterestVaultAddress(market, quoteMint)[0];

    const tx = await program.methods
      .initializeMarket({
        config,
        paramsHash: [...paramsHash],
        bootstrapPriceNad: new anchor.BN(0),
        launchFeeProgressOffset: 0,
      })
      .accounts({
        payer: payer.publicKey,
        baseMint,
        quoteMint,
        market,
        futarchyAuthority,
        ylpMint,
        baseHlpMint,
        quoteHlpMint,
        baseReserveVault,
        quoteReserveVault,
        baseCollateralVault,
        quoteCollateralVault,
        baseInsuranceVault,
        quoteInsuranceVault,
        baseInterestVault,
        quoteInterestVault,
        teamTreasury,
        teamTreasuryWsolAccount,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);

    await initializeLpMetadata({
      market,
      lpMint: ylpMint,
      lpTokenMetadata: ylpTokenMetadata,
      name: "Omnipair V2 (Dusk) yLP",
      symbol: "yLP",
      uri: "https://omnipair.fi/metadata/dusk/ylp.json",
    });
    await initializeLpMetadata({
      market,
      lpMint: baseHlpMint,
      lpTokenMetadata: baseHlpTokenMetadata,
      name: "Omnipair V2 (Dusk) Base hLP",
      symbol: "hLP",
      uri: "https://omnipair.fi/metadata/dusk/base-hlp.json",
    });
    await initializeLpMetadata({
      market,
      lpMint: quoteHlpMint,
      lpTokenMetadata: quoteHlpTokenMetadata,
      name: "Omnipair V2 (Dusk) Quote hLP",
      symbol: "hLP",
      uri: "https://omnipair.fi/metadata/dusk/quote-hlp.json",
    });
    return {
      baseMint,
      quoteMint,
      paramsHash,
      market,
      ylpMint,
      baseHlpMint,
      quoteHlpMint,
      ylpTokenMetadata,
      baseHlpTokenMetadata,
      quoteHlpTokenMetadata,
      baseHlpYlpVault,
      quoteHlpYlpVault,
      baseReserveVault,
      quoteReserveVault,
      baseCollateralVault,
      quoteCollateralVault,
      baseInsuranceVault,
      quoteInsuranceVault,
      baseInterestVault,
      quoteInterestVault,
    };
  }

  async function initializeLpMetadata(params: {
    market: PublicKey;
    lpMint: PublicKey;
    lpTokenMetadata: PublicKey;
    name: string;
    symbol: string;
    uri: string;
  }) {
    const tx = await program.methods
      .initializeLpMetadata({
        name: params.name,
        symbol: params.symbol,
        uri: params.uri,
      })
      .accounts({
        payer: payer.publicKey,
        market: params.market,
        lpMint: params.lpMint,
        lpTokenMetadata: params.lpTokenMetadata,
        systemProgram: SystemProgram.programId,
        sysvarInstructions: SYSVAR_INSTRUCTIONS_PUBKEY,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        tokenMetadataProgram: TOKEN_METADATA_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    trackV2Instruction("initializeLpMetadata", "Token Metadata CreateV1-compatible CPI smoke");
  }

  async function createOwnerAssetAccounts(
    fixture: Awaited<ReturnType<typeof initializeFinalMarket>>,
    baseMintAmount: number | bigint = 1_000_000,
    quoteMintAmount: number | bigint = 2_000_000
  ) {
    const ownerBaseAccount = await createAccount(
      connection as any,
      payer,
      fixture.baseMint,
      payer.publicKey
    );
    const ownerQuoteAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      payer.publicKey
    );
    const ownerYlpAccount = await createToken2022Ata(fixture.ylpMint, payer.publicKey);
    await mintTo(connection as any, payer, fixture.baseMint, ownerBaseAccount, payer, baseMintAmount);
    await mintTo(connection as any, payer, fixture.quoteMint, ownerQuoteAccount, payer, quoteMintAmount);
    return {
      ownerBaseAccount,
      ownerQuoteAccount,
      ownerYlpAccount,
    };
  }

  async function createRecipientAssetAccounts(
    fixture: Awaited<ReturnType<typeof initializeFinalMarket>>,
    owner: PublicKey
  ) {
    const baseAccount = await createAccount(connection as any, payer, fixture.baseMint, owner);
    const quoteAccount = await createAccount(connection as any, payer, fixture.quoteMint, owner);
    return { baseAccount, quoteAccount };
  }

  async function createToken2022Ata(mint: PublicKey, owner: PublicKey) {
    const ata = getAssociatedTokenAddressSync(mint, owner, true, TOKEN_2022_PROGRAM_ID);
    await connection.sendTransaction(
      new Transaction().add(
        createAssociatedTokenAccountIdempotentInstruction(
          payer.publicKey,
          ata,
          owner,
          mint,
          TOKEN_2022_PROGRAM_ID
        )
      ),
      [payer]
    );
    return ata;
  }

  async function addBalancedLiquidity(
    paramsSeed: number,
    config = marketConfig(),
    amounts: {
      baseDeposit: number | bigint;
      quoteDeposit: number | bigint;
      minYlp: number | bigint;
      baseMint: number | bigint;
      quoteMint: number | bigint;
    } = {
      baseDeposit: 100_000,
      quoteDeposit: 200_000,
      minYlp: 100_000,
      baseMint: 1_000_000,
      quoteMint: 2_000_000,
    },
    mintDecimals = 6
  ) {
    const fixture = mintDecimals === 6
      ? await initializeFinalMarket(paramsSeed, config)
      : await initializeFinalMarketWithMints(
          paramsSeed,
          await createMint(connection as any, payer, payer.publicKey, null, mintDecimals),
          await createMint(connection as any, payer, payer.publicKey, null, mintDecimals),
          config,
          mintDecimals
        );
    const ownerAccounts = await createOwnerAssetAccounts(fixture, amounts.baseMint, amounts.quoteMint);

    const tx = await program.methods
      .addLiquidity({
        baseDepositAmount: new BN(amounts.baseDeposit.toString()),
        quoteDepositAmount: new BN(amounts.quoteDeposit.toString()),
        minYlpAmount: new BN(amounts.minYlp.toString()),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        ownerBaseAccount: ownerAccounts.ownerBaseAccount,
        ownerQuoteAccount: ownerAccounts.ownerQuoteAccount,
        ownerYlpAccount: ownerAccounts.ownerYlpAccount,
        baseYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.baseMint,
          "ylp"
        )[0],
        quoteYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.quoteMint,
          "ylp"
        )[0],
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);

    return {
      ...fixture,
      ...ownerAccounts,
    };
  }

  async function openBaseHedge(
    fixture: Awaited<ReturnType<typeof addBalancedLiquidity>>,
    depositAmount = 10_000,
    existingOwnerBaseHlpAccount?: PublicKey
  ) {
    const ownerBaseHlpAccount =
      existingOwnerBaseHlpAccount ??
      (await createToken2022Ata(fixture.baseHlpMint, payer.publicKey));
    const hlpYlpAccount = deriveHlpYlpVaultAddress(
      fixture.market,
      fixture.baseHlpMint,
      fixture.ylpMint
    )[0];
    const baseYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.baseHlpMint,
      fixture.baseMint,
      "hlp"
    )[0];
    const quoteYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.baseHlpMint,
      fixture.quoteMint,
      "hlp"
    )[0];
    await initializeYieldAccounts(fixture, payer.publicKey, fixture.baseHlpMint, "hlp");

    const tx = await program.methods
      .depositSingleSided({
        depositAmount: new BN(depositAmount),
        minHlpAmount: new BN(1),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        targetHlpMint: fixture.baseHlpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        ownerTargetAccount: fixture.ownerBaseAccount,
        ownerHlpAccount: ownerBaseHlpAccount,
        hlpYlpAccount,
        baseYieldAccount,
        quoteYieldAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);

    return {
      ownerBaseHlpAccount,
      hlpYlpAccount,
      baseYieldAccount,
      quoteYieldAccount,
    };
  }

  async function openQuoteHedge(
    fixture: Awaited<ReturnType<typeof addBalancedLiquidity>>,
    depositAmount = 20_000,
    existingOwnerQuoteHlpAccount?: PublicKey
  ) {
    const ownerQuoteHlpAccount =
      existingOwnerQuoteHlpAccount ??
      (await createToken2022Ata(fixture.quoteHlpMint, payer.publicKey));
    const hlpYlpAccount = deriveHlpYlpVaultAddress(
      fixture.market,
      fixture.quoteHlpMint,
      fixture.ylpMint
    )[0];
    const baseYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.quoteHlpMint,
      fixture.baseMint,
      "hlp"
    )[0];
    const quoteYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.quoteHlpMint,
      fixture.quoteMint,
      "hlp"
    )[0];
    await initializeYieldAccounts(fixture, payer.publicKey, fixture.quoteHlpMint, "hlp");

    const tx = await program.methods
      .depositSingleSided({
        depositAmount: new BN(depositAmount),
        minHlpAmount: new BN(1),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        targetHlpMint: fixture.quoteHlpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        ownerTargetAccount: fixture.ownerQuoteAccount,
        ownerHlpAccount: ownerQuoteHlpAccount,
        hlpYlpAccount,
        baseYieldAccount,
        quoteYieldAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);

    return {
      ownerQuoteHlpAccount,
      hlpYlpAccount,
      baseYieldAccount,
      quoteYieldAccount,
    };
  }

  async function burnHlpDirectly(lpMint: PublicKey, ownerLpAccount: PublicKey, amount: bigint) {
    const burnIx = createBurnCheckedInstruction(
      ownerLpAccount,
      lpMint,
      payer.publicKey,
      amount,
      6,
      [],
      TOKEN_2022_PROGRAM_ID
    );
    await connection.sendTransaction(new Transaction().add(burnIx), [payer]);
  }

  function hlpSwapAccounts(fixture: Awaited<ReturnType<typeof addBalancedLiquidity>>) {
    return [
      {
        pubkey: fixture.ylpMint,
        isWritable: true,
        isSigner: false,
      },
      {
        pubkey: fixture.baseHlpYlpVault,
        isWritable: true,
        isSigner: false,
      },
      {
        pubkey: fixture.quoteHlpYlpVault,
        isWritable: true,
        isSigner: false,
      },
      {
        pubkey: fixture.baseInterestVault,
        isWritable: true,
        isSigner: false,
      },
      {
        pubkey: fixture.quoteInterestVault,
        isWritable: true,
        isSigner: false,
      },
    ];
  }

  async function swapBaseForQuote(
    fixture: Awaited<ReturnType<typeof addBalancedLiquidity>>,
    remainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] = [],
    exactAssetIn: number | bigint = 1_000,
    minAssetOut: number | bigint = 1_900
  ) {
    let builder = program.methods
      .swap({
        exactAssetIn: new BN(exactAssetIn.toString()),
        minAssetOut: new BN(minAssetOut.toString()),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        trader: payer.publicKey,
        assetInMint: fixture.baseMint,
        assetOutMint: fixture.quoteMint,
        reserveInVault: fixture.baseReserveVault,
        reserveOutVault: fixture.quoteReserveVault,
        traderAssetInAccount: fixture.ownerBaseAccount,
        traderAssetOutAccount: fixture.ownerQuoteAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      });
    if (remainingAccounts.length > 0) {
      builder = builder.remainingAccounts(remainingAccounts);
    }
    const tx = await builder.transaction();
    return connection.sendTransactionMeasured(tx, [payer]);
  }

  async function swapQuoteForBase(
    fixture: Awaited<ReturnType<typeof addBalancedLiquidity>>,
    remainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] = [],
    exactAssetIn = 2_000,
    minAssetOut = 900
  ) {
    let builder = program.methods
      .swap({
        exactAssetIn: new BN(exactAssetIn),
        minAssetOut: new BN(minAssetOut),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        trader: payer.publicKey,
        assetInMint: fixture.quoteMint,
        assetOutMint: fixture.baseMint,
        reserveInVault: fixture.quoteReserveVault,
        reserveOutVault: fixture.baseReserveVault,
        traderAssetInAccount: fixture.ownerQuoteAccount,
        traderAssetOutAccount: fixture.ownerBaseAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      });
    if (remainingAccounts.length > 0) {
      builder = builder.remainingAccounts(remainingAccounts);
    }
    const tx = await builder.transaction();
    return connection.sendTransactionMeasured(tx, [payer]);
  }

  function currentConcentrationDeltaNad(
    fixture: Awaited<ReturnType<typeof addBalancedLiquidity>>
  ) {
    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const market = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    const base = BigInt(market.base_side.reserves.live_reserve.toString());
    const quoteAtCenter =
      (BigInt(market.quote_side.reserves.live_reserve.toString()) * 1_000_000_000n) /
      BigInt(market.amm.center_price_nad.toString());
    const sum = base + quoteAtCenter;
    const denominator = sum * sum;
    const qNumerator = 4n * base * quoteAtCenter;
    return ((denominator - qNumerator) * 1_000_000_000n) / denominator;
  }

  async function openQuoteDebtLeverage(
    fixture: Awaited<ReturnType<typeof addBalancedLiquidity>>,
    marginAmount = 1_000,
    remainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] = []
  ) {
    const positionId = Keypair.generate().publicKey;
    const leveragePosition = deriveLeveragePositionAddress(fixture.market, positionId)[0];
    const leverageCollateralVault = deriveLeverageCollateralVaultAddress(
      fixture.market,
      fixture.baseMint
    )[0];

    let builder = program.methods
      .openLeverage({
        positionId,
        debtAsset: 1,
        marginAmount: new BN(marginAmount),
        multiplierBps: new BN(20_000),
        minCollateralOut: new BN(1),
        referrer: null,
        positionOwner: null,
        limitPriceNad: new BN(0),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        payer: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        leverageCollateralVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      });
    if (remainingAccounts.length > 0) {
      builder = builder.remainingAccounts(remainingAccounts);
    }
    const tx = await builder.transaction();
    const measurement = await connection.sendTransactionMeasured(tx, [payer]);

    return {
      positionId,
      leveragePosition,
      leverageCollateralVault,
      measurement,
    };
  }

  async function configureReferralPartner(
    referrer: PublicKey,
    interestShareBps = 7_500,
    active = true
  ) {
    const referralPartner = deriveReferralPartnerAddress(referrer)[0];
    const tx = await program.methods
      .configureReferralPartner({ referrer, interestShareBps, active })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        referralPartner,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    return referralPartner;
  }

  async function initializeReferralAccrual(
    referrer: PublicKey,
    market: PublicKey,
    assetMint: PublicKey
  ) {
    const referralPartner = deriveReferralPartnerAddress(referrer)[0];
    const referralAccrual = deriveReferralAccrualAddress(
      referralPartner,
      market,
      assetMint
    )[0];
    const tx = await program.methods
      .initializeReferralAccrual()
      .accounts({
        payer: payer.publicKey,
        referralPartner,
        market,
        assetMint,
        referralAccrual,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    return referralAccrual;
  }

  async function updateInterestRevenue(
    interestBps: number | null,
    maxReferralInterestShareBps: number | null = null
  ) {
    const tx = await program.methods
      .updateProtocolRevenue({
        swapBps: null,
        interestBps,
        maxReferralInterestShareBps,
        revenueDistribution: null,
        protocolAuctionSplit: null,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
  }

  function advanceClockByYear() {
    const clock = svm.getClock();
    clock.slot += 78_840_000n;
    clock.unixTimestamp += 31_536_000n;
    svm.setClock(clock);
    svm.expireBlockhash();
  }

  it("initializes a final yLP/hLP market with hooked Token-2022 LP mints", async function () {
    const fixture = await initializeFinalMarket(42);
    trackV2Instruction("initializeMarket", this.test?.title);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_side.asset_mint.toString()).to.equal(fixture.baseMint.toString());
    expect(decoded.quote_side.asset_mint.toString()).to.equal(fixture.quoteMint.toString());
    expect(decoded.ylp_mint.toString()).to.equal(fixture.ylpMint.toString());
    expect(decoded.base_side.hlp_mint.toString()).to.equal(fixture.baseHlpMint.toString());
    expect(decoded.quote_side.hlp_mint.toString()).to.equal(fixture.quoteHlpMint.toString());
    expect(decoded.base_hlp_vault.ylp_vault.toString()).to.equal(
      fixture.baseHlpYlpVault.toString()
    );
    expect(decoded.quote_hlp_vault.ylp_vault.toString()).to.equal(
      fixture.quoteHlpYlpVault.toString()
    );
    expect(svm.getAccount(fixture.ylpTokenMetadata)).to.not.equal(null);
    expect(svm.getAccount(fixture.baseHlpTokenMetadata)).to.not.equal(null);
    expect(svm.getAccount(fixture.quoteHlpTokenMetadata)).to.not.equal(null);
  });

  it("makes the Dusk transfer hook immutable on production LP mints", async function () {
    const fixture = await initializeFinalMarket(94);
    const mint = await getMint(connection as any, fixture.ylpMint, undefined, TOKEN_2022_PROGRAM_ID);
    const hook = getTransferHook(mint);
    expect(hook).to.not.equal(null);
    // Token-2022 stores `OptionalNonZeroPubkey::None` as 32 zero bytes. The
    // spl-token 0.4.x JS decoder exposes those bytes as PublicKey.default
    // rather than mapping them back to `null`.
    expect(hook!.authority.equals(PublicKey.default)).to.equal(true);
    expect(hook!.programId.equals(DUSK_PROGRAM_ID)).to.equal(true);

    let rejection: unknown;
    try {
      await connection.sendTransaction(
        new Transaction().add(
          createUpdateTransferHookInstruction(
            fixture.ylpMint,
            payer.publicKey,
            REFERRAL_TRANSFER_HOOK_PROGRAM_ID,
            [],
            TOKEN_2022_PROGRAM_ID
          )
        ),
        [payer]
      );
    } catch (error) {
      rejection = error;
    }
    expect(rejection).to.not.equal(undefined);

    const unchangedMint = await getMint(connection as any, fixture.ylpMint, undefined, TOKEN_2022_PROGRAM_ID);
    const unchangedHook = getTransferHook(unchangedMint);
    expect(unchangedHook).to.not.equal(null);
    expect(unchangedHook!.authority.equals(PublicKey.default)).to.equal(true);
    expect(unchangedHook!.programId.equals(DUSK_PROGRAM_ID)).to.equal(true);
  });

  it("rejects duplicate hLP mints before their vault and yield namespaces can become market state", async function () {
    const baseMint = await createMint(connection as any, payer, payer.publicKey, null, 6);
    const quoteMint = await createMint(connection as any, payer, payer.publicKey, null, 6);
    const paramsSeed = 43;
    const paramsHash = Buffer.alloc(32, paramsSeed);
    const [market] = deriveMarketAddress(baseMint, quoteMint, paramsHash);
    const sharedHlpMint = await createHookedLpMint(market, 6);
    const ylpMint = await createHookedLpMint(market, 6);

    const baseHlpYlpVault = deriveHlpYlpVaultAddress(market, sharedHlpMint, ylpMint)[0];
    const quoteHlpYlpVault = deriveHlpYlpVaultAddress(market, sharedHlpMint, ylpMint)[0];
    expect(baseHlpYlpVault.equals(quoteHlpYlpVault)).to.equal(true);

    const baseClassBaseYield = deriveYieldAccountAddress(
      market,
      payer.publicKey,
      sharedHlpMint,
      baseMint,
      "hlp"
    )[0];
    const quoteClassBaseYield = deriveYieldAccountAddress(
      market,
      payer.publicKey,
      sharedHlpMint,
      baseMint,
      "hlp"
    )[0];
    expect(baseClassBaseYield.equals(quoteClassBaseYield)).to.equal(true);

    let rejection: unknown;
    try {
      await initializeFinalMarketWithMints(paramsSeed, baseMint, quoteMint, marketConfig(), 6, {
        ylpMint,
        baseHlpMint: sharedHlpMint,
        quoteHlpMint: sharedHlpMint,
      });
    } catch (error) {
      rejection = error;
    }

    expect(rejection).to.not.equal(undefined);
    expect(String(rejection)).to.include("InvalidLpMintKey");
    expect(svm.getAccount(market)).to.equal(null);
    expect(svm.getAccount(baseHlpYlpVault)).to.equal(null);
    expect(svm.getAccount(baseClassBaseYield)).to.equal(null);
  });

  it("initializes the Dusk futarchy authority from upgradeable ProgramData", async function () {
    const { isolatedSvm, isolatedConnection, isolatedPayer, isolatedProgram } =
      await createIsolatedProgram();
    const [isolatedFutarchyAuthority] = deriveFutarchyAuthorityAddress();
    const [programData] = PublicKey.findProgramAddressSync(
      [DUSK_PROGRAM_ID.toBuffer()],
      BPF_LOADER_UPGRADEABLE_PROGRAM_ID
    );
    const programDataBytes = upgradeableProgramData(isolatedPayer.publicKey);
    isolatedSvm.setAccount(programData, {
      lamports: Number(isolatedSvm.minimumBalanceForRentExemption(BigInt(programDataBytes.length))),
      data: new Uint8Array(programDataBytes),
      owner: BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
      executable: false,
      rentEpoch: 0,
    });

    const tx = await isolatedProgram.methods
      .initFutarchyAuthority({
        authority: isolatedPayer.publicKey,
        swapBps: 125,
        interestBps: 250,
        maxReferralInterestShareBps: 2_500,
        futarchyTreasury: isolatedPayer.publicKey,
        futarchyTreasuryBps: 5_000,
        buybacksVault: isolatedPayer.publicKey,
        buybacksVaultBps: 2_000,
        teamTreasury: isolatedPayer.publicKey,
        teamTreasuryBps: 3_000,
        stakingVault: isolatedPayer.publicKey,
        feeAuctionAcceptedMint: NATIVE_MINT,
        buybackAuctionAcceptedMint: NATIVE_MINT,
      })
      .accounts({
        deployer: isolatedPayer.publicKey,
        futarchyAuthority: isolatedFutarchyAuthority,
        programData,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    await isolatedConnection.sendTransaction(tx, [isolatedPayer]);
    trackV2Instruction("initFutarchyAuthority", this.test?.title);

    const authorityAccount = isolatedSvm.getAccount(isolatedFutarchyAuthority);
    expect(authorityAccount).to.not.equal(null);
    const authority = accountCoder.decode(
      "FutarchyAuthority",
      Buffer.from(authorityAccount!.data)
    ) as any;
    expect(authority.authority.toString()).to.equal(isolatedPayer.publicKey.toString());
    expect(authority.revenue_share.swap_bps).to.equal(125);
    expect(authority.revenue_share.interest_bps).to.equal(250);
    expect(authority.revenue_distribution.futarchy_treasury_bps).to.equal(5_000);
    expect(authority.revenue_distribution.buybacks_vault_bps).to.equal(2_000);
    expect(authority.revenue_distribution.team_treasury_bps).to.equal(3_000);
  });

  it("adds balanced liquidity and mints floating yLP shares", async function () {
    const fixture = await addBalancedLiquidity(43);
    trackV2Instruction("addLiquidity", this.test?.title);

    const ylpAccount = await getAccount(
      connection as any,
      fixture.ownerYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ylpAccount.amount).to.equal(140_421n);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_side.reserves.live_reserve.toNumber()).to.equal(100_000);
    expect(decoded.quote_side.reserves.live_reserve.toNumber()).to.equal(200_000);
    expect(decoded.base_side.shares.ylp_supply.toNumber()).to.equal(141_421);
    expect(decoded.quote_side.shares.ylp_supply.toNumber()).to.equal(141_421);
  });

  it("returns typed preview data for market state and swap quotes", async function () {
    const fixture = await addBalancedLiquidity(60);

    const marketPreview = decodePreviewMarketReturnData(
      await simulateReturnData(
        await program.methods
          .previewMarket()
          .accounts({
            market: fixture.market,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewMarket", this.test?.title);

    expect(marketPreview.base.liveReserve.toNumber()).to.equal(100_000);
    expect(marketPreview.quote.liveReserve.toNumber()).to.equal(200_000);
    expect(marketPreview.base.ylpSupply.toNumber()).to.equal(141_421);
    expect(marketPreview.quote.ylpSupply.toNumber()).to.equal(141_421);
    expect(marketPreview.base.spotPriceNad.toNumber()).to.equal(2_000_000_000);
    expect(marketPreview.quote.spotPriceNad.toNumber()).to.equal(500_000_000);

    const addLiquidityPreview = decodePreviewAddLiquidityReturnData(
      await simulateReturnData(
        await program.methods
          .previewAddLiquidity({
            baseDepositAmount: new BN(10_000),
            quoteDepositAmount: new BN(50_000),
          })
          .accounts({
            market: fixture.market,
            baseMint: fixture.baseMint,
            quoteMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewAddLiquidity", this.test?.title);

    expect(addLiquidityPreview.requestedBaseAmount.toNumber()).to.equal(10_000);
    expect(addLiquidityPreview.requestedQuoteAmount.toNumber()).to.equal(50_000);
    expect(addLiquidityPreview.baseTransferAmount.toNumber()).to.equal(10_000);
    expect(addLiquidityPreview.quoteTransferAmount.toNumber()).to.equal(20_000);
    expect(addLiquidityPreview.baseReserveCredit.toNumber()).to.equal(10_000);
    expect(addLiquidityPreview.quoteReserveCredit.toNumber()).to.equal(20_000);
    expect(addLiquidityPreview.unusedQuoteAmount.toNumber()).to.equal(30_000);
    expect(addLiquidityPreview.ylpAmount.toNumber()).to.equal(14_142);

    const swapPreview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({
            exactAssetIn: new BN(1_000),
          })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.baseMint,
            assetOutMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewSwap", this.test?.title);

    expect(swapPreview.assetIn).to.deep.equal({ base: {} });
    expect(swapPreview.assetOut).to.deep.equal({ quote: {} });
    expect(swapPreview.reserveCredit.toNumber()).to.equal(1_000);
    expect(swapPreview.baseFeeDebit.toNumber()).to.equal(3);
    expect(swapPreview.claimableFeeCredit.toNumber()).to.equal(3);
    expect(swapPreview.amountInForQuote.toNumber()).to.equal(997);
    expect(swapPreview.amountOut.toNumber()).to.equal(1_974);
    expect(swapPreview.reserveInLiveReserve.toNumber()).to.equal(100_997);
    expect(swapPreview.reserveOutLiveReserve.toNumber()).to.equal(198_026);
  });

  it("adds V1-style limiting-side liquidity with Token-2022 transfer-fee assets", async function () {
    const baseMint = await createTransferFeeMint(payer.publicKey, 6, 100, 10_000n);
    const quoteMint = await createTransferFeeMint(payer.publicKey, 6, 50, 10_000n);
    const fixture = await initializeFinalMarketWithMints(61, baseMint, quoteMint);
    const ownerBaseAccount = await createAccount(
      connection as any,
      payer,
      fixture.baseMint,
      payer.publicKey,
      Keypair.generate(),
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const ownerQuoteAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      payer.publicKey,
      Keypair.generate(),
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const ownerYlpAccount = await createToken2022Ata(fixture.ylpMint, payer.publicKey);
    await mintTo(
      connection as any,
      payer,
      fixture.baseMint,
      ownerBaseAccount,
      payer,
      1_000_000,
      [],
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    await mintTo(
      connection as any,
      payer,
      fixture.quoteMint,
      ownerQuoteAccount,
      payer,
      1_000_000,
      [],
      undefined,
      TOKEN_2022_PROGRAM_ID
    );

    const previewAddLiquidity = async (baseDepositAmount: number, quoteDepositAmount: number) =>
      decodePreviewAddLiquidityReturnData(
        await simulateReturnData(
          await program.methods
            .previewAddLiquidity({
              baseDepositAmount: new BN(baseDepositAmount),
              quoteDepositAmount: new BN(quoteDepositAmount),
            })
            .accounts({
              market: fixture.market,
              baseMint: fixture.baseMint,
              quoteMint: fixture.quoteMint,
            })
            .transaction()
        )
      ) as any;

    const sendAddLiquidity = async (
      baseDepositAmount: number,
      quoteDepositAmount: number,
      minYlpAmount: anchor.BN | number
    ) => {
      const tx = await program.methods
        .addLiquidity({
          baseDepositAmount: new BN(baseDepositAmount),
          quoteDepositAmount: new BN(quoteDepositAmount),
          minYlpAmount: BN.isBN(minYlpAmount) ? minYlpAmount : new BN(minYlpAmount),
        })
        .accounts({
          market: fixture.market,
          futarchyAuthority,
          owner: payer.publicKey,
          baseMint: fixture.baseMint,
          quoteMint: fixture.quoteMint,
          ylpMint: fixture.ylpMint,
          baseReserveVault: fixture.baseReserveVault,
          quoteReserveVault: fixture.quoteReserveVault,
          ownerBaseAccount,
          ownerQuoteAccount,
          ownerYlpAccount,
        baseYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.baseMint,
            "ylp"
          )[0],
        quoteYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.quoteMint,
            "ylp"
          )[0],
          tokenProgram: TOKEN_PROGRAM_ID,
          token2022Program: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          eventAuthority: eventAuthority(),
          program: DUSK_PROGRAM_ID,
        })
        .transaction();
      await connection.sendTransaction(tx, [payer]);
    };

    const firstPreview = await previewAddLiquidity(101_000, 202_000);
    expect(firstPreview.baseTransferFee.toNumber()).to.be.greaterThan(0);
    expect(firstPreview.quoteTransferFee.toNumber()).to.be.greaterThan(0);

    await sendAddLiquidity(101_000, 202_000, 1);
    trackV2Instruction("addLiquidity", this.test?.title);

    let marketAccount = svm.getAccount(fixture.market);
    expect(marketAccount).to.not.equal(null);
    let decoded = accountCoder.decode("Market", Buffer.from(marketAccount!.data)) as any;
    expect(decoded.base_side.reserves.live_reserve.toNumber()).to.equal(
      firstPreview.baseReserveCredit.toNumber()
    );
    expect(decoded.quote_side.reserves.live_reserve.toNumber()).to.equal(
      firstPreview.quoteReserveCredit.toNumber()
    );

    const baseOwnerBefore = await getAccount(
      connection as any,
      ownerBaseAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const quoteOwnerBefore = await getAccount(
      connection as any,
      ownerQuoteAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const secondPreview = await previewAddLiquidity(10_000, 50_000);
    trackV2Instruction("previewAddLiquidity", this.test?.title);

    expect(secondPreview.baseTransferFee.toNumber()).to.be.greaterThan(0);
    expect(secondPreview.quoteTransferFee.toNumber()).to.be.greaterThan(0);
    expect(secondPreview.unusedQuoteAmount.toNumber()).to.be.greaterThan(0);

    await sendAddLiquidity(10_000, 50_000, secondPreview.ylpAmount);
    trackV2Instruction("addLiquidity", this.test?.title);

    const baseOwnerAfter = await getAccount(
      connection as any,
      ownerBaseAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const quoteOwnerAfter = await getAccount(
      connection as any,
      ownerQuoteAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(baseOwnerBefore.amount - baseOwnerAfter.amount).to.equal(
      BigInt(secondPreview.baseTransferAmount.toNumber())
    );
    expect(quoteOwnerBefore.amount - quoteOwnerAfter.amount).to.equal(
      BigInt(secondPreview.quoteTransferAmount.toNumber())
    );

    marketAccount = svm.getAccount(fixture.market);
    expect(marketAccount).to.not.equal(null);
    decoded = accountCoder.decode("Market", Buffer.from(marketAccount!.data)) as any;
    expect(decoded.base_side.reserves.live_reserve.toNumber()).to.equal(
      firstPreview.baseReserveCredit
        .add(secondPreview.baseReserveCredit)
        .toNumber()
    );
    expect(decoded.quote_side.reserves.live_reserve.toNumber()).to.equal(
      firstPreview.quoteReserveCredit
        .add(secondPreview.quoteReserveCredit)
        .toNumber()
    );
    expect(decoded.base_side.shares.ylp_supply.toNumber()).to.equal(
      secondPreview.ylpSupply.toNumber()
    );
    expect(decoded.quote_side.shares.ylp_supply.toNumber()).to.equal(
      secondPreview.ylpSupply.toNumber()
    );

    const token2022SwapMeasurement = await swapBaseForQuote(
      {
        ...fixture,
        ownerBaseAccount,
        ownerQuoteAccount,
        ownerYlpAccount,
      },
      [],
      10_000,
      1
    );
    recordSwapComputeScenario("token_2022_swap", token2022SwapMeasurement);
  });

  it("accrues and claims permissioned referral interest for Token-2022 assets", async function () {
    const baseMint = await createTransferFeeMint(payer.publicKey, 6, 100, 10_000n);
    const quoteMint = await createTransferFeeMint(payer.publicKey, 6, 50, 10_000n);
    const fixture = await initializeFinalMarketWithMints(71, baseMint, quoteMint);
    const ownerBaseAccount = await createAccount(
      connection as any,
      payer,
      fixture.baseMint,
      payer.publicKey,
      Keypair.generate(),
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const ownerQuoteAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      payer.publicKey,
      Keypair.generate(),
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const ownerYlpAccount = await createToken2022Ata(fixture.ylpMint, payer.publicKey);
    await mintTo(
      connection as any,
      payer,
      fixture.baseMint,
      ownerBaseAccount,
      payer,
      1_000_000,
      [],
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    await mintTo(
      connection as any,
      payer,
      fixture.quoteMint,
      ownerQuoteAccount,
      payer,
      1_000_000,
      [],
      undefined,
      TOKEN_2022_PROGRAM_ID
    );

    const addLiquidityTx = await program.methods
      .addLiquidity({
        baseDepositAmount: new BN(101_000),
        quoteDepositAmount: new BN(202_000),
        minYlpAmount: new BN(1),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        ownerBaseAccount,
        ownerQuoteAccount,
        ownerYlpAccount,
        baseYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.baseMint,
          "ylp"
        )[0],
        quoteYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.quoteMint,
          "ylp"
        )[0],
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(addLiquidityTx, [payer]);

    await updateInterestRevenue(10_000, 5_000);
    const referralPartner = await configureReferralPartner(payer.publicKey, 7_500);
    const referralAccrual = await initializeReferralAccrual(
      payer.publicKey,
      fixture.market,
      fixture.quoteMint
    );
    trackV2Instruction("configureReferralPartner", this.test?.title);
    trackV2Instruction("initializeReferralAccrual", this.test?.title);

    const borrowPositionId = Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPositionAddress(fixture.market, borrowPositionId)[0];
    const depositTx = await program.methods
      .depositCollateral({
        positionId: borrowPositionId,
        depositAmount: new BN(20_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: ownerBaseAccount,
        borrowPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(depositTx, [payer]);

    const preview = decodePreviewBorrowCapacityReturnData(
      await simulateReturnData(
        await program.methods
          .previewBorrowCapacity({
            collateralAmount: new BN(19_800),
            projectedBorrowAmount: new BN(10_000),
          })
          .accounts({
            market: fixture.market,
            collateralAssetMint: fixture.baseMint,
            debtAssetMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    expect(preview.projectedDebtAmount.toNumber()).to.equal(10_000);

    const ownerQuoteBefore = await getAccount(
      connection as any,
      ownerQuoteAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const borrowTx = await program.methods
      .borrow({
        borrowAmount: new BN(10_000),
        minDebtAmountOut: new BN(9_950),
        minLiquidationCfBps: 8_500,
        referrer: payer.publicKey,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: ownerQuoteAccount,
        borrowPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(borrowTx, [payer]);
    trackV2Instruction("borrow", this.test?.title);

    const ownerQuoteAfter = await getAccount(
      connection as any,
      ownerQuoteAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ownerQuoteAfter.amount - ownerQuoteBefore.amount).to.equal(9_950n);
    let position = accountCoder.decode(
      "BorrowPosition",
      Buffer.from(svm.getAccount(borrowPosition)!.data)
    ) as any;
    expect(position.fixed_quote_shares.toNumber()).to.equal(10_000);
    expect(position.quote_referral_partner.toString()).to.equal(referralPartner.toString());
    expect(position.quote_referral_interest_share_bps).to.equal(5_000);
    let accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    expect(accrual.amount.toNumber()).to.equal(0);

    await configureReferralPartner(payer.publicKey, 1_000, false);
    advanceClockByYear();

    const repayTx = await program.methods
      .repay({ repayAmount: new BN(5_000) })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        reserveVault: fixture.quoteReserveVault,
        interestVault: fixture.quoteInterestVault,
        ownerDebtAccount: ownerQuoteAccount,
        borrowPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(repayTx, [payer]);
    trackV2Instruction("repay", this.test?.title);

    accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    const claimable = BigInt(accrual.amount.toString());
    expect(claimable > 0n).to.equal(true);
    const market = accountCoder.decode(
      "Market",
      Buffer.from(svm.getAccount(fixture.market)!.data)
    ) as any;
    expect(BigInt(market.quote_side.fees.referral_interest_liability.toString())).to.equal(
      claimable
    );
    const interestVaultBefore = await getAccount(
      connection as any,
      fixture.quoteInterestVault,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(interestVaultBefore.amount >= claimable).to.equal(true);

    const recipientTokenAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      payer.publicKey,
      Keypair.generate(),
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const claimTx = await program.methods
      .claimReferralInterest()
      .accounts({
        market: fixture.market,
        authority: payer.publicKey,
        referralPartner,
        assetMint: fixture.quoteMint,
        referralAccrual,
        interestVault: fixture.quoteInterestVault,
        recipientTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(claimTx, [payer]);
    trackV2Instruction("claimReferralInterest", this.test?.title);

    accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    expect(accrual.amount.toNumber()).to.equal(0);
    const recipient = await getAccount(
      connection as any,
      recipientTokenAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(recipient.amount > 0n && recipient.amount <= claimable).to.equal(true);
  });
  it("resolves Token-2022 hooks across liquidity, lending, and claims", async function () {
    const baseMint = await createMint(connection as any, payer, payer.publicKey, null, 6);
    const quoteMint = await createDeferredHookMint(payer.publicKey, 6);
    const fixture = await initializeFinalMarketWithMints(72, baseMint, quoteMint);
    const ownerBaseAccount = await createAccount(
      connection as any,
      payer,
      fixture.baseMint,
      payer.publicKey
    );
    const ownerQuoteAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      payer.publicKey,
      Keypair.generate(),
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const ownerYlpAccount = await createToken2022Ata(fixture.ylpMint, payer.publicKey);
    await mintTo(connection as any, payer, fixture.baseMint, ownerBaseAccount, payer, 1_000_000);
    await mintTo(
      connection as any,
      payer,
      fixture.quoteMint,
      ownerQuoteAccount,
      payer,
      1_000_000,
      [],
      undefined,
      TOKEN_2022_PROGRAM_ID
    );

    await connection.sendTransaction(
      new Transaction().add(
        createUpdateTransferHookInstruction(
          fixture.quoteMint,
          payer.publicKey,
          REFERRAL_TRANSFER_HOOK_PROGRAM_ID,
          [],
          TOKEN_2022_PROGRAM_ID
        )
      ),
      [payer]
    );
    const hookValidationAccount = getExtraAccountMetaAddress(
      fixture.quoteMint,
      REFERRAL_TRANSFER_HOOK_PROGRAM_ID
    );
    const hookValidationData = buildYieldTransferHookValidationAccountData([]);
    svm.setAccount(hookValidationAccount, {
      lamports: Number(svm.minimumBalanceForRentExemption(BigInt(hookValidationData.length))),
      data: new Uint8Array(hookValidationData),
      owner: REFERRAL_TRANSFER_HOOK_PROGRAM_ID,
      executable: false,
      rentEpoch: 0,
    });
    const addLiquidityHookAccounts = await resolveTransferHookAccountMetas(
      connection as any,
      [{
        source: ownerQuoteAccount,
        mint: fixture.quoteMint,
        destination: fixture.quoteReserveVault,
        authority: payer.publicKey,
        amount: 200_000,
        decimals: 6,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      }]
    );

    const addLiquidityTx = await program.methods
      .addLiquidity({
        baseDepositAmount: new BN(100_000),
        quoteDepositAmount: new BN(200_000),
        minYlpAmount: new BN(100_000),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        ownerBaseAccount,
        ownerQuoteAccount,
        ownerYlpAccount,
        baseYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.baseMint,
          "ylp"
        )[0],
        quoteYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.quoteMint,
          "ylp"
        )[0],
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .remainingAccounts(addLiquidityHookAccounts)
      .transaction();
    await connection.sendTransaction(addLiquidityTx, [payer]);

    const referrer = Keypair.generate();
    await connection.requestAirdrop(referrer.publicKey, LAMPORTS_PER_SOL);
    await updateInterestRevenue(10_000, 5_000);
    const referralPartner = await configureReferralPartner(referrer.publicKey, 5_000);
    const referralAccrual = await initializeReferralAccrual(
      referrer.publicKey,
      fixture.market,
      fixture.quoteMint
    );
    const setRecipientTx = await program.methods
      .setReferralRecipient({ recipient: payer.publicKey })
      .accounts({
        authority: referrer.publicKey,
        referralPartner,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(setRecipientTx, [referrer]);
    trackV2Instruction("setReferralRecipient", this.test?.title);

    const borrowPositionId = Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPositionAddress(fixture.market, borrowPositionId)[0];
    const depositTx = await program.methods
      .depositCollateral({
        positionId: borrowPositionId,
        depositAmount: new BN(10_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: ownerBaseAccount,
        borrowPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(depositTx, [payer]);

    const borrowHookAccounts = await resolveTransferHookAccountMetas(
      connection as any,
      [{
        source: fixture.quoteReserveVault,
        mint: fixture.quoteMint,
        destination: ownerQuoteAccount,
        authority: fixture.market,
        amount: 5_000,
        decimals: 6,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      }]
    );
    const borrowTx = await program.methods
      .borrow({
        borrowAmount: new BN(5_000),
        minDebtAmountOut: new BN(5_000),
        minLiquidationCfBps: 8_500,
        referrer: referrer.publicKey,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: ownerQuoteAccount,
        borrowPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .remainingAccounts(borrowHookAccounts)
      .transaction();
    await connection.sendTransaction(borrowTx, [payer]);

    advanceClockByYear();
    const repayHookAccounts = await resolveTransferHookAccountMetas(
      connection as any,
      [
        {
          source: ownerQuoteAccount,
          mint: fixture.quoteMint,
          destination: fixture.quoteReserveVault,
          authority: payer.publicKey,
          amount: 2_500,
          decimals: 6,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        },
        {
          source: fixture.quoteReserveVault,
          mint: fixture.quoteMint,
          destination: fixture.quoteInterestVault,
          authority: fixture.market,
          amount: 1,
          decimals: 6,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        },
      ]
    );
    const repayTx = await program.methods
      .repay({ repayAmount: new BN(2_500) })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        reserveVault: fixture.quoteReserveVault,
        interestVault: fixture.quoteInterestVault,
        ownerDebtAccount: ownerQuoteAccount,
        borrowPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .remainingAccounts(repayHookAccounts)
      .transaction();
    await connection.sendTransaction(repayTx, [payer]);

    const accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    const claimable = BigInt(accrual.amount.toString());
    expect(claimable > 0n).to.equal(true);

    const claimHookAccounts = await resolveTransferHookAccountMetas(
      connection as any,
      [{
        source: fixture.quoteInterestVault,
        mint: fixture.quoteMint,
        destination: ownerQuoteAccount,
        authority: fixture.market,
        amount: claimable,
        decimals: 6,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      }]
    );
    const ownerQuoteBeforeClaim = await getAccount(
      connection as any,
      ownerQuoteAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const claimTx = await program.methods
      .claimReferralInterest()
      .accounts({
        market: fixture.market,
        authority: referrer.publicKey,
        referralPartner,
        assetMint: fixture.quoteMint,
        referralAccrual,
        interestVault: fixture.quoteInterestVault,
        recipientTokenAccount: ownerQuoteAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .remainingAccounts(claimHookAccounts)
      .transaction();
    await connection.sendTransaction(claimTx, [referrer]);

    const ownerQuoteAfterClaim = await getAccount(
      connection as any,
      ownerQuoteAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ownerQuoteAfterClaim.amount - ownerQuoteBeforeClaim.amount).to.equal(claimable);
  });
  it("supports the hLP launch profile for base hLP entry", async function () {
    const fixture = await addBalancedLiquidity(44);
    const ownerBaseBefore = await getAccount(connection as any, fixture.ownerBaseAccount);
    const hedge = await openBaseHedge(fixture);
    trackV2Instruction("depositSingleSided", this.test?.title);

    const ownerBaseAfter = await getAccount(connection as any, fixture.ownerBaseAccount);
    expect(ownerBaseAfter.amount).to.equal(ownerBaseBefore.amount - 10_000n);

    const ownerHlp = await getAccount(
      connection as any,
      hedge.ownerBaseHlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const vaultYlp = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ownerHlp.amount).to.equal(10_000n);
    expect(vaultYlp.amount).to.equal(14_142n);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_side.reserves.live_reserve.toNumber()).to.equal(110_000);
    expect(decoded.quote_side.reserves.live_reserve.toNumber()).to.equal(220_000);
    expect(decoded.base_hlp_vault.ylp_shares.toNumber()).to.equal(14_142);
    expect(decoded.base_hlp_vault.hlp_supply.toNumber()).to.equal(10_000);
    expect(decoded.base_hlp_vault.debt_shares.toNumber()).to.be.greaterThan(0);
  });

  it("SDK hLP builder repairs prefunded System-owned yield and yLP vault PDAs in the action transaction", async function () {
    const fixture = await addBalancedLiquidity(94);
    const ownerHlpAccount = await createToken2022Ata(fixture.baseHlpMint, payer.publicKey);
    const hlpYlpAccount = deriveHlpYlpVaultAddress(
      fixture.market,
      fixture.baseHlpMint,
      fixture.ylpMint
    )[0];
    const baseYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.baseHlpMint,
      fixture.baseMint,
      "hlp"
    )[0];
    const quoteYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.baseHlpMint,
      fixture.quoteMint,
      "hlp"
    )[0];
    const prefundLamports = Number(svm.minimumBalanceForRentExemption(0n));
    await connection.sendTransaction(
      new Transaction().add(
        SystemProgram.transfer({
          fromPubkey: payer.publicKey,
          toPubkey: baseYieldAccount,
          lamports: prefundLamports,
        }),
        SystemProgram.transfer({
          fromPubkey: payer.publicKey,
          toPubkey: quoteYieldAccount,
          lamports: prefundLamports,
        }),
        SystemProgram.transfer({
          fromPubkey: payer.publicKey,
          toPubkey: hlpYlpAccount,
          lamports: prefundLamports,
        })
      ),
      [payer]
    );
    expect(svm.getAccount(baseYieldAccount)?.owner.toString()).to.equal(
      SystemProgram.programId.toString()
    );
    expect(svm.getAccount(quoteYieldAccount)?.owner.toString()).to.equal(
      SystemProgram.programId.toString()
    );
    expect(svm.getAccount(hlpYlpAccount)?.owner.toString()).to.equal(
      SystemProgram.programId.toString()
    );

    const yieldAccountDefinition = idl.accounts.find(
      (account: { name: string }) => account.name === "YieldAccount"
    );
    expect(yieldAccountDefinition).to.not.equal(undefined);
    const sdkWrite = new DuskWrite({
      provider: program.provider,
      programId: program.programId,
      methods: program.methods,
      idl: {
        instructions: program.idl.instructions,
        accounts: [{ ...yieldAccountDefinition, name: "yieldAccount" }],
      },
      coder: {
        accounts: {
          size: () => accountCoder.size("YieldAccount"),
        },
      },
    } as any);
    const depositArgs = { depositAmount: new BN(1_000), minHlpAmount: new BN(1) };
    const depositOptions = {
      payer: payer.publicKey,
      owner: payer.publicKey,
      market: fixture.market,
      targetHlpMint: fixture.baseHlpMint,
      baseMint: fixture.baseMint,
      quoteMint: fixture.quoteMint,
      accounts: {
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        targetHlpMint: fixture.baseHlpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        ownerTargetAccount: fixture.ownerBaseAccount,
        ownerHlpAccount,
        hlpYlpAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: payer.publicKey,
        program: SystemProgram.programId,
      },
    };
    const build = await sdkWrite.depositSingleSided(depositArgs, depositOptions);
    expect(build.setupInstructions).to.have.length(1);
    const depositInstruction = program.idl.instructions.find(
      (instruction: { name: string }) => instruction.name === "depositSingleSided"
    );
    const eventAuthorityIndex = depositInstruction?.accounts.findIndex(
      (account: { name: string }) => account.name === "eventAuthority"
    );
    const eventProgramIndex = depositInstruction?.accounts.findIndex(
      (account: { name: string }) => account.name === "program"
    );
    expect(eventAuthorityIndex).to.be.a("number").and.greaterThanOrEqual(0);
    expect(eventProgramIndex).to.be.a("number").and.greaterThanOrEqual(0);
    expect(build.actionInstruction.keys[eventAuthorityIndex!].pubkey.equals(eventAuthority())).to.equal(true);
    expect(build.actionInstruction.keys[eventProgramIndex!].pubkey.equals(DUSK_PROGRAM_ID)).to.equal(true);
    await connection.sendTransaction(build.transaction, [payer]);
    const openedEvent = cpiEvent(build.transaction, "hlpOpened");
    expect(openedEvent.market.toString()).to.equal(fixture.market.toString());
    expect(openedEvent.owner.toString()).to.equal(payer.publicKey.toString());
    expect(openedEvent.assetSide).to.equal(0);
    expect(openedEvent.depositAmount.toString()).to.equal("1000");
    expect(BigInt(openedEvent.borrowedAmount.toString()) > 0n).to.equal(true);
    expect(BigInt(openedEvent.ylpAmount.toString()) > 0n).to.equal(true);
    expect(BigInt(openedEvent.hlpAmount.toString()) > 0n).to.equal(true);

    const repeatedBuild = await sdkWrite.depositSingleSided(depositArgs, depositOptions);
    expect(repeatedBuild.setupInstructions).to.have.length(0);

    expect(svm.getAccount(baseYieldAccount)?.owner.toString()).to.equal(DUSK_PROGRAM_ID.toString());
    expect(svm.getAccount(quoteYieldAccount)?.owner.toString()).to.equal(DUSK_PROGRAM_ID.toString());
    expect(svm.getAccount(hlpYlpAccount)?.owner.toString()).to.equal(TOKEN_2022_PROGRAM_ID.toString());
    const ownerHlp = await getAccount(
      connection as any,
      ownerHlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ownerHlp.amount > 0n).to.equal(true);
  });

  it("aggregates repeated base hLP opens into canonical vault yLP accounts", async function () {
    const fixture = await addBalancedLiquidity(50);
    const first = await openBaseHedge(fixture, 5_000);
    await openBaseHedge(fixture, 6_000, first.ownerBaseHlpAccount);

    const ownerHlp = await getAccount(
      connection as any,
      first.ownerBaseHlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const vaultYlp = await getAccount(
      connection as any,
      first.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ownerHlp.amount).to.equal(11_001n);
    expect(vaultYlp.amount).to.equal(15_556n);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_hlp_vault.ylp_shares.toNumber()).to.equal(15_556);
    expect(decoded.base_hlp_vault.hlp_supply.toNumber()).to.equal(11_001);
  });

  it("reconciles a partial direct hLP burn before pricing the next deposit", async function () {
    const fixture = await addBalancedLiquidity(92);
    const first = await openBaseHedge(fixture, 10_000);
    await burnHlpDirectly(fixture.baseHlpMint, first.ownerBaseHlpAccount, 4_000n);

    const liveAfterBurn = await getAccount(
      connection as any,
      first.ownerBaseHlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(liveAfterBurn.amount).to.equal(6_000n);

    await openBaseHedge(fixture, 6_000, first.ownerBaseHlpAccount);
    const liveAfterDeposit = await getAccount(
      connection as any,
      first.ownerBaseHlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_hlp_vault.hlp_supply.toString()).to.equal(liveAfterDeposit.amount.toString());
    expect(liveAfterDeposit.amount > liveAfterBurn.amount).to.equal(true);
  });

  it("lets the final legitimate hLP atom exit after every other atom was burned directly", async function () {
    const fixture = await addBalancedLiquidity(93);
    const hedge = await openBaseHedge(fixture, 10_000);
    await burnHlpDirectly(fixture.baseHlpMint, hedge.ownerBaseHlpAccount, 9_999n);

    const tx = await program.methods
      .withdrawSingleSided({
        hlpAmount: new BN(1),
        minTargetAmountOut: new BN(1),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        targetHlpMint: fixture.baseHlpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        borrowedInterestVault: fixture.quoteInterestVault,
        ownerTargetAccount: fixture.ownerBaseAccount,
        ownerHlpAccount: hedge.ownerBaseHlpAccount,
        hlpYlpAccount: hedge.hlpYlpAccount,
        baseYieldAccount: hedge.baseYieldAccount,
        quoteYieldAccount: hedge.quoteYieldAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);

    const liveAfterExit = await getAccount(
      connection as any,
      hedge.ownerBaseHlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const vaultYlpAfterExit = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(liveAfterExit.amount).to.equal(0n);
    expect(vaultYlpAfterExit.amount).to.equal(0n);
    expect(decoded.base_hlp_vault.hlp_supply.toNumber()).to.equal(0);
    expect(decoded.base_hlp_vault.ylp_shares.toNumber()).to.equal(0);
  });

  it("closes base hLP by burning vault yLP, repaying quote debt, and returning base", async function () {
    const fixture = await addBalancedLiquidity(45);
    const ownerBaseBeforeOpen = await getAccount(connection as any, fixture.ownerBaseAccount);
    const hedge = await openBaseHedge(fixture);
    const ownerBaseBeforeClose = await getAccount(connection as any, fixture.ownerBaseAccount);

    const tx = await program.methods
      .withdrawSingleSided({
        hlpAmount: new BN(10_000),
        minTargetAmountOut: new BN(9_998),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        targetHlpMint: fixture.baseHlpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        borrowedInterestVault: fixture.quoteInterestVault,
        ownerTargetAccount: fixture.ownerBaseAccount,
        ownerHlpAccount: hedge.ownerBaseHlpAccount,
        hlpYlpAccount: hedge.hlpYlpAccount,
        baseYieldAccount: hedge.baseYieldAccount,
        quoteYieldAccount: hedge.quoteYieldAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    trackV2Instruction("withdrawSingleSided", this.test?.title);

    const ownerBaseAfterClose = await getAccount(connection as any, fixture.ownerBaseAccount);
    const closedEvent = cpiEvent(tx, "hlpClosed");
    expect(closedEvent.market.toString()).to.equal(fixture.market.toString());
    expect(closedEvent.owner.toString()).to.equal(payer.publicKey.toString());
    expect(closedEvent.assetSide).to.equal(0);
    expect(closedEvent.hlpAmount.toString()).to.equal("10000");
    expect(closedEvent.amountOut.toString()).to.equal(
      (ownerBaseAfterClose.amount - ownerBaseBeforeClose.amount).toString()
    );
    expect(BigInt(closedEvent.ylpAmount.toString()) > 0n).to.equal(true);
    expect(BigInt(closedEvent.debtRepaid.toString()) > 0n).to.equal(true);
    expect(ownerBaseAfterClose.amount).to.equal(ownerBaseBeforeOpen.amount - 2n);

    const ownerHlp = await getAccount(
      connection as any,
      hedge.ownerBaseHlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const vaultYlp = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ownerHlp.amount).to.equal(0n);
    expect(vaultYlp.amount).to.equal(0n);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_side.reserves.live_reserve.toNumber()).to.equal(100_002);
    expect(decoded.quote_side.reserves.live_reserve.toNumber()).to.equal(200_000);
    expect(decoded.base_side.shares.ylp_supply.toNumber()).to.equal(141_421);
    expect(decoded.quote_side.shares.ylp_supply.toNumber()).to.equal(141_421);
    expect(decoded.base_hlp_vault.ylp_shares.toNumber()).to.equal(0);
    expect(decoded.base_hlp_vault.hlp_supply.toNumber()).to.equal(0);
    expect(decoded.base_hlp_vault.debt_shares.toNumber()).to.equal(0);
  });

  it("opens and closes quote hLP by borrowing base and returning quote", async function () {
    const fixture = await addBalancedLiquidity(54);
    const ownerQuoteBeforeOpen = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const hedge = await openQuoteHedge(fixture);
    trackV2Instruction("depositSingleSided", this.test?.title);

    const ownerQuoteAfterOpen = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerQuoteAfterOpen.amount).to.equal(ownerQuoteBeforeOpen.amount - 20_000n);

    const ownerHlp = await getAccount(
      connection as any,
      hedge.ownerQuoteHlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const vaultYlp = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ownerHlp.amount).to.equal(20_000n);
    expect(vaultYlp.amount).to.equal(14_142n);

    let account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    let decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.quote_hlp_vault.ylp_shares.toNumber()).to.equal(14_142);
    expect(decoded.quote_hlp_vault.hlp_supply.toNumber()).to.equal(20_000);
    expect(decoded.quote_hlp_vault.debt_shares.toNumber()).to.be.greaterThan(0);

    const tx = await program.methods
      .withdrawSingleSided({
        hlpAmount: new BN(20_000),
        minTargetAmountOut: new BN(19_996),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        targetHlpMint: fixture.quoteHlpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        borrowedInterestVault: fixture.baseInterestVault,
        ownerTargetAccount: fixture.ownerQuoteAccount,
        ownerHlpAccount: hedge.ownerQuoteHlpAccount,
        hlpYlpAccount: hedge.hlpYlpAccount,
        baseYieldAccount: hedge.baseYieldAccount,
        quoteYieldAccount: hedge.quoteYieldAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    trackV2Instruction("withdrawSingleSided", this.test?.title);

    const ownerQuoteAfterClose = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerQuoteAfterClose.amount).to.equal(ownerQuoteBeforeOpen.amount - 4n);

    account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_side.reserves.live_reserve.toNumber()).to.equal(100_000);
    expect(decoded.quote_side.reserves.live_reserve.toNumber()).to.equal(200_004);
    expect(decoded.quote_hlp_vault.ylp_shares.toNumber()).to.equal(0);
    expect(decoded.quote_hlp_vault.hlp_supply.toNumber()).to.equal(0);
    expect(decoded.quote_hlp_vault.debt_shares.toNumber()).to.equal(0);
  });

  it("removes matched yLP liquidity and returns pro-rata reserves", async function () {
    const fixture = await addBalancedLiquidity(46);
    const ownerBaseBefore = await getAccount(connection as any, fixture.ownerBaseAccount);
    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);

    const tx = await program.methods
      .removeLiquidity({
        ylpAmount: new BN(1_000),
        minBaseAmountOut: new BN(707),
        minQuoteAmountOut: new BN(1_414),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        ownerBaseAccount: fixture.ownerBaseAccount,
        ownerQuoteAccount: fixture.ownerQuoteAccount,
        ownerYlpAccount: fixture.ownerYlpAccount,
        baseYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.baseMint,
          "ylp"
        )[0],
        quoteYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.quoteMint,
          "ylp"
        )[0],
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    trackV2Instruction("removeLiquidity", this.test?.title);

    const ownerBaseAfter = await getAccount(connection as any, fixture.ownerBaseAccount);
    const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerBaseAfter.amount).to.equal(ownerBaseBefore.amount + 707n);
    expect(ownerQuoteAfter.amount).to.equal(ownerQuoteBefore.amount + 1_414n);

    const ylpAccount = await getAccount(
      connection as any,
      fixture.ownerYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ylpAccount.amount).to.equal(139_421n);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_side.reserves.live_reserve.toNumber()).to.equal(99_293);
    expect(decoded.quote_side.reserves.live_reserve.toNumber()).to.equal(198_586);
    expect(decoded.base_side.shares.ylp_supply.toNumber()).to.equal(140_421);
    expect(decoded.quote_side.shares.ylp_supply.toNumber()).to.equal(140_421);
    const baseReserveVault = await getAccount(connection as any, fixture.baseReserveVault);
    const quoteReserveVault = await getAccount(connection as any, fixture.quoteReserveVault);
    expect(baseReserveVault.amount).to.equal(
      BigInt(decoded.base_side.reserves.cash_reserve.toString()) +
        BigInt(decoded.base_side.fees.swap_fee_custody_balance.toString())
    );
    expect(quoteReserveVault.amount).to.equal(
      BigInt(decoded.quote_side.reserves.cash_reserve.toString()) +
        BigInt(decoded.quote_side.fees.swap_fee_custody_balance.toString())
    );
  });

  it("allows yLP exits through the normal liquidity path", async function () {
    const fixture = await addBalancedLiquidity(59);
    const ownerBaseBefore = await getAccount(connection as any, fixture.ownerBaseAccount);
    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);

    const tx = await program.methods
      .removeLiquidity({
        ylpAmount: new BN(20_000),
        minBaseAmountOut: new BN(14_000),
        minQuoteAmountOut: new BN(28_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        baseMint: fixture.baseMint,
        quoteMint: fixture.quoteMint,
        ylpMint: fixture.ylpMint,
        baseReserveVault: fixture.baseReserveVault,
        quoteReserveVault: fixture.quoteReserveVault,
        ownerBaseAccount: fixture.ownerBaseAccount,
        ownerQuoteAccount: fixture.ownerQuoteAccount,
        ownerYlpAccount: fixture.ownerYlpAccount,
        baseYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.baseMint,
          "ylp"
        )[0],
        quoteYieldAccount: deriveYieldAccountAddress(
          fixture.market,
          payer.publicKey,
          fixture.ylpMint,
          fixture.quoteMint,
          "ylp"
        )[0],
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(tx, [payer]);
    trackV2Instruction("removeLiquidity", this.test?.title);

    const ownerBaseAfter = await getAccount(connection as any, fixture.ownerBaseAccount);
    const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerBaseAfter.amount - ownerBaseBefore.amount).to.equal(14_142n);
    expect(ownerQuoteAfter.amount - ownerQuoteBefore.amount).to.equal(28_284n);
  });

  it("swaps through the Dusk market and routes non-compounding swap fees", async function () {
    const fixture = await addBalancedLiquidity(47);
    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const sameSlot = svm.getClock().slot;

    const ordinaryAccount = svm.getAccount(fixture.market);
    expect(ordinaryAccount).to.not.equal(null);
    const ordinary = accountCoder.decode("Market", Buffer.from(ordinaryAccount!.data)) as any;
    const ordinaryBaseMint = svm.getAccount(fixture.baseMint);
    const ordinaryQuoteMint = svm.getAccount(fixture.quoteMint);
    expect(ordinaryBaseMint).to.not.equal(null);
    expect(ordinaryQuoteMint).to.not.equal(null);
    expect(new PublicKey(ordinaryBaseMint!.owner).equals(TOKEN_PROGRAM_ID)).to.equal(true);
    expect(new PublicKey(ordinaryQuoteMint!.owner).equals(TOKEN_PROGRAM_ID)).to.equal(true);
    expect(ordinary.debt.fixed_base_shares.isZero()).to.equal(true);
    expect(ordinary.debt.fixed_quote_shares.isZero()).to.equal(true);
    expect(ordinary.debt.isolated_base_shares.isZero()).to.equal(true);
    expect(ordinary.debt.isolated_quote_shares.isZero()).to.equal(true);
    expect(ordinary.base_hlp_vault.hlp_supply.isZero()).to.equal(true);
    expect(ordinary.quote_hlp_vault.hlp_supply.isZero()).to.equal(true);
    expect(ordinary.base_hlp_vault.residual_exposure.isZero()).to.equal(true);
    expect(ordinary.quote_hlp_vault.residual_exposure.isZero()).to.equal(true);
    expect(ordinary.amm.explicit_curve_cache.range_width_nad.isZero()).to.equal(true);
    expect(ordinary.amm.explicit_curve_cache.concentrated_liquidity_share_nad.isZero()).to.equal(
      true
    );
    expect(ordinary.config.amm.adjustment_step_nad.isZero()).to.equal(true);
    expect(ordinary.amm.deferred_controller_target.kind).to.equal(0);
    expect(ordinary.amm.last_observation_slot.toString()).to.equal(sameSlot.toString());

    const sameSlotMeasurement = await swapBaseForQuote(fixture);
    expect(svm.getClock().slot).to.equal(sameSlot);
    recordSwapComputeScenario("cpmm_same_slot", sameSlotMeasurement);
    trackV2Instruction("swap", this.test?.title);

    const swapEvent = cpiEvent(sameSlotMeasurement.transaction, "swapExecuted");
    expect(swapEvent.market.toString()).to.equal(fixture.market.toString());
    expect(swapEvent.trader.toString()).to.equal(payer.publicKey.toString());
    expect(swapEvent.assetInSide).to.equal(0);
    expect(swapEvent.amountIn.toString()).to.equal("1000");
    expect(swapEvent.amountOut.toString()).to.equal("1974");
    expect(swapEvent.amountInAfterFee.toString()).to.equal("997");
    expect(swapEvent.baseFee.toString()).to.equal("3");
    expect(swapEvent.divergenceFee.toString()).to.equal("0");
    expect(swapEvent.volatilityFee.toString()).to.equal("0");
    expect(swapEvent.retainedFee.toString()).to.equal("0");
    expect(swapEvent.baseLiveReserve.toString()).to.equal("100997");
    expect(swapEvent.quoteLiveReserve.toString()).to.equal("198026");

    const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerQuoteAfter.amount).to.equal(ownerQuoteBefore.amount + 1_974n);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_side.reserves.live_reserve.toNumber()).to.equal(100_997);
    expect(decoded.quote_side.reserves.live_reserve.toNumber()).to.equal(198_026);
    expect(decoded.base_side.fees.swap_fee_liability.toNumber()).to.equal(3);
    expect(decoded.base_side.fees.unallocated_swap_fee_liability.toNumber()).to.equal(0);
    expect(decoded.base_side.fees.swap_fee_custody_balance.toNumber()).to.equal(3);
    const baseReserveVault = await getAccount(connection as any, fixture.baseReserveVault);
    expect(baseReserveVault.amount).to.equal(
      BigInt(decoded.base_side.reserves.cash_reserve.toString()) +
        BigInt(decoded.base_side.fees.swap_fee_custody_balance.toString())
    );

    const advancedSlot = svm.getClock().slot + 1n;
    svm.warpToSlot(advancedSlot);
    const advancedSlotMeasurement = await swapBaseForQuote(fixture, [], 1_000, 1);
    recordSwapComputeScenario("cpmm_advanced_slot", advancedSlotMeasurement);
  });

  it("executes and previews the Dusk Concentrated AMM on SBF", async function () {
    const config = marketConfig();
    config.amm.rangeWidthNad = new BN("4000000000");
    config.amm.concentratedLiquidityShareNad = new BN("500000000");
    const fixture = await addBalancedLiquidity(75, config, {
      baseDeposit: 100_000_000,
      quoteDeposit: 200_000_000,
      minYlp: 1,
      baseMint: 500_000_000,
      quoteMint: 500_000_000,
    });
    expect(currentConcentrationDeltaNad(fixture) < 100_000_000n).to.equal(true);

    const preview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({
            exactAssetIn: new BN(1_000_000),
          })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.baseMint,
            assetOutMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewSwap", this.test?.title);

    const marketBeforeSubmittedPreview = svm.getAccount(fixture.market);
    expect(marketBeforeSubmittedPreview).to.not.equal(null);
    const submittedPreviewTx = await program.methods
      .previewSwap({
        exactAssetIn: new BN(1_000_000),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        assetInMint: fixture.baseMint,
        assetOutMint: fixture.quoteMint,
      })
      .transaction();
    await connection.sendTransaction(submittedPreviewTx, [payer]);
    const marketAfterSubmittedPreview = svm.getAccount(fixture.market);
    expect(marketAfterSubmittedPreview).to.not.equal(null);
    expect(Buffer.from(marketAfterSubmittedPreview!.data)).to.deep.equal(
      Buffer.from(marketBeforeSubmittedPreview!.data)
    );

    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const centeredMeasurement = await swapBaseForQuote(fixture, [], 1_000_000, 1);
    expect(currentConcentrationDeltaNad(fixture) < 100_000_000n).to.equal(true);
    recordSwapComputeScenario("concentrated_centered", centeredMeasurement);
    trackV2Instruction("swap", this.test?.title);
    const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);

    expect(ownerQuoteAfter.amount - ownerQuoteBefore.amount).to.equal(
      BigInt(preview.amountOut.toString())
    );
    // Near the initialized center, the concentrated curve must provide tighter execution than
    // the same reserves' zero-concentrated-liquidity CPMM output (1,974,316 raw quote units).
    expect(preview.amountOut.toNumber()).to.be.greaterThan(1_974_316);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.amm.explicit_curve_cache.range_width_nad.toString()).to.equal(
      config.amm.rangeWidthNad.toString()
    );
    expect(
      decoded.amm.explicit_curve_cache.concentrated_liquidity_share_nad.toString()
    ).to.equal(config.amm.concentratedLiquidityShareNad.toString());
  });

  it("measures concentrated transition and shifted-CPMM tail swap paths", async function () {
    const config = marketConfig();
    config.swapFeeBps = 0;
    config.amm.rangeWidthNad = new BN("4000000000");
    config.amm.concentratedLiquidityShareNad = new BN("500000000");
    const amounts = {
      baseDeposit: 100_000_000,
      quoteDeposit: 200_000_000,
      minYlp: 1,
      baseMint: 500_000_000,
      quoteMint: 500_000_000,
    };
    const transition = await addBalancedLiquidity(94, config, amounts);
    const transitionMeasurement = await swapBaseForQuote(transition, [], 30_000_000, 1);
    recordSwapComputeScenario("concentrated_transition", transitionMeasurement);

    const tail = await addBalancedLiquidity(95, config, amounts);
    // Enter the outer branch outside the measurement. The named sample below
    // starts and ends in the shifted-CPMM tail; it is not a center-to-tail solve.
    await swapBaseForQuote(tail, [], 120_000_000, 1);
    const tailInput = 1_000_000n;
    const ownerQuoteBeforeTail = await getAccount(connection as any, tail.ownerQuoteAccount);
    const tailMeasurement = await swapBaseForQuote(tail, [], tailInput, 1);
    const ownerQuoteAfterTail = await getAccount(connection as any, tail.ownerQuoteAccount);
    // Tail execution is constant-product over shifted curve coordinates, not
    // over raw live reserves. The curve module pins the exact formula; this
    // integration check only needs to prove a positive executable output.
    expect(ownerQuoteAfterTail.amount - ownerQuoteBeforeTail.amount > 0n).to.equal(true);
    recordSwapComputeScenario("concentrated_tail", tailMeasurement);
  });

  it("retains a concentrated dynamic surcharge as recentering principal on SBF", async function () {
    const config = marketConfig();
    config.swapFeeBps = 0;
    config.divergenceFeeShareCapBps = 5_000;
    config.amm.rangeWidthNad = new BN("4000000000");
    config.amm.concentratedLiquidityShareNad = new BN("500000000");
    config.amm.adjustmentThresholdNad = new BN("10000000");
    config.amm.adjustmentStepNad = new BN("10000000");
    config.amm.minAdjustmentIntervalSlots = new BN(1);
    config.amm.divergenceFeeCoefficientNad = new BN("10000000000");
    const fixture = await addBalancedLiquidity(76, config, {
      baseDeposit: 100_000_000,
      quoteDeposit: 200_000_000,
      minYlp: 1,
      baseMint: 500_000_000,
      quoteMint: 500_000_000,
    });

    // A centered pool has no recenter impairment yet. The first outward swap
    // creates an off-center state whose next permitted center step needs a
    // protected budget, so retention turns on for the following quote.
    await swapBaseForQuote(fixture, [], 5_000_000, 1);
    trackV2Instruction("swap", this.test?.title);
    const afterFirstSwap = svm.getAccount(fixture.market);
    expect(afterFirstSwap).to.not.equal(null);
    const firstDecoded = accountCoder.decode(
      "Market",
      Buffer.from(afterFirstSwap!.data)
    ) as any;
    expect(firstDecoded.amm.retain_dynamic_surcharge).to.equal(true);
    const custodyBefore = firstDecoded.base_side.fees.swap_fee_custody_balance;

    const preview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({
            exactAssetIn: new BN(1_000_000),
          })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.baseMint,
            assetOutMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewSwap", this.test?.title);

    expect(preview.retentionActive).to.equal(true);
    expect(preview.dynamicSurchargeDebit.toNumber()).to.be.greaterThan(0);
    expect(preview.retainedSurcharge.toString()).to.equal(
      preview.dynamicSurchargeDebit.toString()
    );
    expect(preview.distributedSurchargeDebit.toNumber()).to.equal(0);
    expect(preview.claimableFeeDebit.toString()).to.equal(
      preview.baseFeeDebit.toString()
    );
    expect(preview.reserveInputCredit.toString()).to.equal(
      preview.amountInForQuote.add(preview.retainedSurcharge).toString()
    );

    const retainedMeasurement = await swapBaseForQuote(fixture, [], 1_000_000, 1);
    recordSwapComputeScenario("retained_surcharge", retainedMeasurement);
    trackV2Instruction("swap", this.test?.title);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(
      BigInt(decoded.base_side.fees.swap_fee_custody_balance.toString()) -
        BigInt(custodyBefore.toString())
    ).to.equal(BigInt(preview.claimableFeeCredit.toString()));
    const reserveVault = await getAccount(connection as any, fixture.baseReserveVault);
    expect(reserveVault.amount).to.equal(
      BigInt(decoded.base_side.reserves.cash_reserve.toString()) +
        BigInt(decoded.base_side.fees.swap_fee_custody_balance.toString()) +
        BigInt(decoded.base_side.reserves.protected_recenter_reserve.toString())
    );
    expect(
      decoded.base_side.reserves.protected_recenter_reserve.toString()
    ).to.equal(preview.retainedSurcharge.toString());
    expect(decoded.amm.q_per_share_nad.toString()).to.equal(
      decoded.amm.protected_floor_per_share_nad.toString()
    );
    expect(decoded.amm.retention_target_stale).to.equal(true);
  });

  it("executes the valid wide-domain CPMM u128 divergence path below the SBF ceiling", async function () {
    const config = marketConfig();
    config.swapFeeBps = 0;
    config.divergenceFeeShareCapBps = 5_000;
    config.amm.divergenceFeeCoefficientNad = new BN("100000000000");
    const fixture = await addBalancedLiquidity(80, config, {
      baseDeposit: 10_000_000_000_000,
      quoteDeposit: 20_000_000_000_000,
      minYlp: 1,
      baseMint: 6_000_000_000_000_000,
      quoteMint: 50_000_000_000_000,
    }, 0);
    const grossInput = new BN("5000000000000000");
    const preview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: grossInput })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.baseMint,
            assetOutMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    const hardFeeLimit = grossInput.divn(2);
    expect(preview.divergenceSurchargeDebit.gt(new BN(0))).to.equal(true);
    expect(preview.dynamicSurchargeDebit.toString()).to.equal(
      preview.divergenceSurchargeDebit.toString()
    );
    expect(preview.totalFeeDebit.lte(hardFeeLimit)).to.equal(true);
    expect(preview.amountInForQuote.gte(grossInput.sub(hardFeeLimit))).to.equal(true);
    expect(preview.amountInForQuote.add(preview.totalFeeDebit).toString()).to.equal(
      grossInput.toString()
    );

    const submittedPreviewTx = await program.methods
      .previewSwap({ exactAssetIn: grossInput })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        assetInMint: fixture.baseMint,
        assetOutMint: fixture.quoteMint,
      })
      .transaction();
    await connection.sendTransaction(submittedPreviewTx, [payer]);
    trackV2Instruction("previewSwap", this.test?.title);

    await swapBaseForQuote(fixture, [], 5_000_000_000_000_000, 1);
    trackV2Instruction("swap", this.test?.title);
  });

  it("measures a decaying-volatility surcharge swap path", async function () {
    const config = marketConfig();
    config.swapFeeBps = 0;
    config.volatilityFeeShareCapBps = 5_000;
    config.amm.volatilityShockCapNad = new BN("1000000000");
    config.amm.volatilityCapNad = new BN("1000000000");
    config.amm.volatilityFeeCoefficientNad = new BN("100000000000");
    const fixture = await addBalancedLiquidity(96, config, {
      baseDeposit: 100_000_000,
      quoteDeposit: 200_000_000,
      minYlp: 1,
      baseMint: 500_000_000,
      quoteMint: 500_000_000,
    });

    await swapBaseForQuote(fixture, [], 10_000_000, 1);
    const armedAccount = svm.getAccount(fixture.market);
    expect(armedAccount).to.not.equal(null);
    const armed = accountCoder.decode("Market", Buffer.from(armedAccount!.data)) as any;
    expect(armed.amm.volatility_accumulator_nad.gt(new BN(0))).to.equal(true);

    // Decay is slot-driven. Advance the clock and explicitly rotate the
    // LiteSVM blockhash because the two swap transactions are otherwise
    // byte-identical.
    svm.warpToSlot(svm.getClock().slot + 1n);
    svm.expireBlockhash();

    const preview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: new BN(10_000_000) })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.baseMint,
            assetOutMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    expect(preview.volatilitySurchargeDebit.gt(new BN(0))).to.equal(true);

    const volatilityMeasurement = await swapBaseForQuote(fixture, [], 10_000_000, 1);
    recordSwapComputeScenario(
      "dynamic_fee_volatility_stress",
      volatilityMeasurement
    );
  });

  it("executes concentrated u128 divergence paths with distributed and retained surcharge", async function () {
    const concentratedConfig = marketConfig();
    concentratedConfig.swapFeeBps = 0;
    concentratedConfig.divergenceFeeShareCapBps = 5_000;
    concentratedConfig.amm.rangeWidthNad = new BN("4000000000");
    concentratedConfig.amm.concentratedLiquidityShareNad = new BN("500000000");
    concentratedConfig.amm.divergenceFeeCoefficientNad = new BN("100000000000");
    const amounts = {
      baseDeposit: 1_000_000_000_000_000n,
      quoteDeposit: 1_000_000_000_000n,
      minYlp: 1n,
      baseMint: 1_500_000_000_000_000n,
      quoteMint: 2_000_000_000_000n,
    };
    const grossInput = 100_000_000_000_000n;
    const retainedGrossInput = grossInput - 1n;

    // Nine-decimal mints keep each normalized common-coordinate reserve
    // inside the explicit u64/Q48 concentrated domain while the reserve
    // product still exceeds u128, exercising the intended wide-product path.
    const distributed = await addBalancedLiquidity(81, concentratedConfig, amounts, 9);
    const distributedPreview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: new BN(grossInput.toString()) })
          .accounts({
            market: distributed.market,
            futarchyAuthority,
            assetInMint: distributed.baseMint,
            assetOutMint: distributed.quoteMint,
          })
          .transaction()
      )
    ) as any;
    expect(distributedPreview.amountInForQuote.toNumber()).to.be.greaterThan(0);
    expect(distributedPreview.dynamicSurchargeDebit.toNumber()).to.be.greaterThan(0);
    expect(distributedPreview.retentionActive).to.equal(false);
    await swapBaseForQuote(distributed, [], grossInput, 1);
    trackV2Instruction("swap", this.test?.title);

    const retainedConfig = marketConfig();
    retainedConfig.swapFeeBps = 0;
    retainedConfig.divergenceFeeShareCapBps = 5_000;
    retainedConfig.amm.rangeWidthNad = new BN("4000000000");
    retainedConfig.amm.concentratedLiquidityShareNad = new BN("500000000");
    retainedConfig.amm.divergenceFeeCoefficientNad = new BN("100000000000");
    retainedConfig.amm.adjustmentThresholdNad = new BN("1000");
    retainedConfig.amm.adjustmentStepNad = new BN("1000");
    retainedConfig.amm.minAdjustmentIntervalSlots = new BN(1);
    const retained = await addBalancedLiquidity(82, retainedConfig, amounts, 9);
    await swapBaseForQuote(retained, [], grossInput, 1);
    trackV2Instruction("swap", this.test?.title);
    const afterFirst = svm.getAccount(retained.market);
    expect(afterFirst).to.not.equal(null);
    const decodedAfterFirst = accountCoder.decode(
      "Market",
      Buffer.from(afterFirst!.data)
    ) as any;
    expect(decodedAfterFirst.amm.retain_dynamic_surcharge).to.equal(true);

    const retainedPreview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: new BN(retainedGrossInput.toString()) })
          .accounts({
            market: retained.market,
            futarchyAuthority,
            assetInMint: retained.baseMint,
            assetOutMint: retained.quoteMint,
          })
          .transaction()
      )
    ) as any;
    expect(retainedPreview.amountInForQuote.toNumber()).to.be.greaterThan(0);
    expect(retainedPreview.dynamicSurchargeDebit.toNumber()).to.be.greaterThan(0);
    expect(retainedPreview.retentionActive).to.equal(true);
    expect(retainedPreview.retainedSurcharge.toString()).to.equal(
      retainedPreview.dynamicSurchargeDebit.toString()
    );
    await swapBaseForQuote(retained, [], retainedGrossInput, 1);
    trackV2Instruction("swap", this.test?.title);
  });

  it("keeps hard-capped divergence executable in both fee routes and rolls back an exhausted atom", async function () {
    const configFor = (retained: boolean) => {
      const config = marketConfig();
      config.swapFeeBps = 0;
      config.divergenceFeeShareCapBps = 5_000;
      config.amm.rangeWidthNad = new BN("4000000000");
      config.amm.concentratedLiquidityShareNad = new BN("500000000");
      config.amm.divergenceFeeCoefficientNad = new BN("100000000000");
      if (retained) {
        config.amm.adjustmentThresholdNad = new BN("1000");
        config.amm.adjustmentStepNad = new BN("1000");
        config.amm.minAdjustmentIntervalSlots = new BN(1);
      }
      return config;
    };
    const grossInput = 2_000_000_000_000n;
    const amounts = {
      baseDeposit: 100_000_000n,
      quoteDeposit: 100_000_000n,
      minYlp: 1n,
      baseMint: grossInput + 200_000_000n,
      quoteMint: 200_000_000n,
    };
    const assertExtremePreview = (preview: any, retained: boolean) => {
      const sharePpm = preview.divergenceSurchargeDebit
        .mul(new BN(1_000_000))
        .div(new BN(grossInput.toString()));
      expect(sharePpm.gte(new BN(499_000))).to.equal(true);
      expect(sharePpm.lte(new BN(500_000))).to.equal(true);
      expect(preview.totalFeeDebit.lte(new BN((grossInput / 2n).toString()))).to.equal(true);
      expect(preview.amountInForQuote.gte(new BN(((grossInput + 1n) / 2n).toString()))).to.equal(true);
      expect(preview.amountInForQuote.gt(new BN(0))).to.equal(true);
      expect(preview.retentionActive).to.equal(retained);
      if (retained) {
        expect(preview.retainedSurcharge.toString()).to.equal(
          preview.dynamicSurchargeDebit.toString()
        );
      } else {
        expect(preview.retainedSurcharge.toNumber()).to.equal(0);
      }
    };

    const distributed = await addBalancedLiquidity(91, configFor(false), amounts, 6);
    const distributedPreview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: new BN(grossInput.toString()) })
          .accounts({
            market: distributed.market,
            futarchyAuthority,
            assetInMint: distributed.baseMint,
            assetOutMint: distributed.quoteMint,
          })
          .transaction()
      )
    ) as any;
    assertExtremePreview(distributedPreview, false);
    const divergenceMeasurement = await swapBaseForQuote(distributed, [], grossInput, 1);
    recordSwapComputeScenario(
      "dynamic_fee_divergence_stress",
      divergenceMeasurement
    );
    trackV2Instruction("swap", this.test?.title);

    const retained = await addBalancedLiquidity(92, configFor(true), amounts, 6);
    await swapBaseForQuote(retained, [], 1_000_000, 1);
    trackV2Instruction("swap", this.test?.title);
    const armedAccount = svm.getAccount(retained.market);
    expect(armedAccount).to.not.equal(null);
    const armed = accountCoder.decode("Market", Buffer.from(armedAccount!.data)) as any;
    expect(armed.amm.retain_dynamic_surcharge).to.equal(true);

    const retainedPreview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: new BN(grossInput.toString()) })
          .accounts({
            market: retained.market,
            futarchyAuthority,
            assetInMint: retained.baseMint,
            assetOutMint: retained.quoteMint,
          })
          .transaction()
      )
    ) as any;
    assertExtremePreview(retainedPreview, true);
    await swapBaseForQuote(retained, [], grossInput, 1);
    trackV2Instruction("swap", this.test?.title);

    const marketBefore = svm.getAccount(retained.market);
    expect(marketBefore).to.not.equal(null);
    const [baseVaultBefore, quoteVaultBefore, ownerBaseBefore, ownerQuoteBefore] =
      await Promise.all([
        getAccount(connection as any, retained.baseReserveVault),
        getAccount(connection as any, retained.quoteReserveVault),
        getAccount(connection as any, retained.ownerBaseAccount),
        getAccount(connection as any, retained.ownerQuoteAccount),
      ]);
    let rejection: unknown;
    try {
      await swapBaseForQuote(retained, [], 1, 1);
    } catch (error) {
      rejection = error;
    }
    expect(rejection).to.not.equal(undefined);
    expect(String(rejection)).to.include("InsufficientOutputAmount");

    const marketAfter = svm.getAccount(retained.market);
    expect(marketAfter).to.not.equal(null);
    const [baseVaultAfter, quoteVaultAfter, ownerBaseAfter, ownerQuoteAfter] =
      await Promise.all([
        getAccount(connection as any, retained.baseReserveVault),
        getAccount(connection as any, retained.quoteReserveVault),
        getAccount(connection as any, retained.ownerBaseAccount),
        getAccount(connection as any, retained.ownerQuoteAccount),
      ]);
    expect(Buffer.from(marketAfter!.data).equals(Buffer.from(marketBefore!.data))).to.equal(true);
    expect(baseVaultAfter.amount).to.equal(baseVaultBefore.amount);
    expect(quoteVaultAfter.amount).to.equal(quoteVaultBefore.amount);
    expect(ownerBaseAfter.amount).to.equal(ownerBaseBefore.amount);
    expect(ownerQuoteAfter.amount).to.equal(ownerQuoteBefore.amount);
  });

  it("rejects the same zero post-retention mark in preview and execution without mutation", async function () {
    const config = marketConfig();
    config.swapFeeBps = 0;
    config.amm.rangeWidthNad = new BN("4000000000");
    config.amm.concentratedLiquidityShareNad = new BN("500000000");
    config.amm.divergenceFeeCoefficientNad = new BN("100000000000");
    config.amm.adjustmentThresholdNad = new BN("1000");
    config.amm.adjustmentStepNad = new BN("1000");
    config.amm.minAdjustmentIntervalSlots = new BN(1);
    const grossInput = 7_000_000_000_000_000n;
    const fixture = await addBalancedLiquidity(93, config, {
      baseDeposit: 100_000_000n,
      quoteDeposit: 100_000_000n,
      minYlp: 1n,
      baseMint: grossInput + 200_000_000n,
      quoteMint: 200_000_000n,
    }, 6);
    await swapBaseForQuote(fixture, [], 1_000_000, 1);
    trackV2Instruction("swap", this.test?.title);
    const armedAccount = svm.getAccount(fixture.market);
    expect(armedAccount).to.not.equal(null);
    const armed = accountCoder.decode("Market", Buffer.from(armedAccount!.data)) as any;
    expect(armed.amm.retain_dynamic_surcharge).to.equal(true);

    const marketBefore = svm.getAccount(fixture.market);
    expect(marketBefore).to.not.equal(null);
    const [baseVaultBefore, quoteVaultBefore, ownerBaseBefore, ownerQuoteBefore] =
      await Promise.all([
        getAccount(connection as any, fixture.baseReserveVault),
        getAccount(connection as any, fixture.quoteReserveVault),
        getAccount(connection as any, fixture.ownerBaseAccount),
        getAccount(connection as any, fixture.ownerQuoteAccount),
      ]);
    let previewRejection: unknown;
    try {
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: new BN(grossInput.toString()) })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.baseMint,
            assetOutMint: fixture.quoteMint,
          })
          .transaction()
      );
    } catch (error) {
      previewRejection = error;
    }
    expect(previewRejection).to.not.equal(undefined);
    expect(String(previewRejection)).to.include("InvalidSettlementPrice");

    let executionRejection: unknown;
    try {
      await swapBaseForQuote(fixture, [], grossInput, 1);
    } catch (error) {
      executionRejection = error;
    }
    expect(executionRejection).to.not.equal(undefined);
    expect(String(executionRejection)).to.include("InvalidSettlementPrice");

    const marketAfter = svm.getAccount(fixture.market);
    expect(marketAfter).to.not.equal(null);
    const [baseVaultAfter, quoteVaultAfter, ownerBaseAfter, ownerQuoteAfter] =
      await Promise.all([
        getAccount(connection as any, fixture.baseReserveVault),
        getAccount(connection as any, fixture.quoteReserveVault),
        getAccount(connection as any, fixture.ownerBaseAccount),
        getAccount(connection as any, fixture.ownerQuoteAccount),
      ]);
    expect(Buffer.from(marketAfter!.data).equals(Buffer.from(marketBefore!.data))).to.equal(true);
    expect(baseVaultAfter.amount).to.equal(baseVaultBefore.amount);
    expect(quoteVaultAfter.amount).to.equal(quoteVaultBefore.amount);
    expect(ownerBaseAfter.amount).to.equal(ownerBaseBefore.amount);
    expect(ownerQuoteAfter.amount).to.equal(ownerQuoteBefore.amount);
  });

  it("executes a fully funded concentrated recenter below the SBF compute ceiling", async function () {
    const config = marketConfig();
    config.amm.rangeWidthNad = new BN("4000000000");
    config.amm.concentratedLiquidityShareNad = new BN("500000000");
    config.amm.adjustmentThresholdNad = new BN("1000");
    config.amm.adjustmentStepNad = new BN("1000");
    config.amm.minAdjustmentIntervalSlots = new BN(1);
    config.amm.divergenceFeeCoefficientNad = new BN("10000000000");
    const fixture = await addBalancedLiquidity(77, config, {
      baseDeposit: 100_000_000,
      quoteDeposit: 200_000_000,
      minYlp: 1,
      baseMint: 500_000_000,
      quoteMint: 500_000_000,
    });

    await swapBaseForQuote(fixture, [], 5_000_000, 1);
    const accountBefore = svm.getAccount(fixture.market);
    expect(accountBefore).to.not.equal(null);
    const funded = accountCoder.decode(
      "Market",
      Buffer.from(accountBefore!.data)
    ) as any;
    expect(funded.amm.retention_target_stale).to.equal(true);
    expect(funded.amm.retention_hard_cap_nad.gt(new BN(0))).to.equal(true);

    // Fixture-only funding isolates the recenter compute path. Production can
    // create this same protected surplus only through retained surcharge.
    funded.amm.protected_floor_per_share_nad =
      funded.amm.q_per_share_nad.sub(funded.amm.retention_hard_cap_nad);
    funded.amm.price_ema_nad = funded.amm.last_trade_price_nad;
    funded.amm.retention_target_stale = true;
    // Anchor's generic account encoder allocates only 1,000 bytes internally;
    // Market is intentionally larger, so encode through its generated layout.
    const marketLayout = (accountCoder as any).accountLayouts.get("Market");
    const marketBody = Buffer.alloc(accountBefore!.data.length - 8);
    const marketBodyLength = marketLayout.layout.encode(funded, marketBody);
    const fundedData = Buffer.concat([
      (accountCoder as any).accountDiscriminator("Market"),
      marketBody.subarray(0, marketBodyLength),
    ]);
    expect(fundedData.length).to.equal(accountBefore!.data.length);
    svm.setAccount(fixture.market, {
      ...accountBefore!,
      data: new Uint8Array(fundedData),
    });
    const recenterSlot = BigInt(
      funded.amm.last_observation_slot.add(new BN(1)).toString()
    );
    svm.warpToSlot(recenterSlot);

    const oldCenter = funded.amm.center_price_nad;
    const beforeRejectedProbe = svm.getAccount(fixture.market);
    expect(beforeRejectedProbe).to.not.equal(null);
    let rejectedForSlippage = false;
    try {
      await swapBaseForQuote(fixture, [], 1_000_000, 500_000_000);
    } catch {
      rejectedForSlippage = true;
    }
    expect(rejectedForSlippage).to.equal(true);
    const afterRejectedProbe = svm.getAccount(fixture.market);
    expect(afterRejectedProbe).to.not.equal(null);
    expect(Buffer.from(afterRejectedProbe!.data)).to.deep.equal(
      Buffer.from(beforeRejectedProbe!.data)
    );

    const recenterMeasurement = await swapBaseForQuote(fixture, [], 1_000_000, 1);
    recordSwapComputeScenario("controller_due_recenter", recenterMeasurement);
    trackV2Instruction("swap", this.test?.title);

    const accountAfter = svm.getAccount(fixture.market);
    expect(accountAfter).to.not.equal(null);
    const recentered = accountCoder.decode(
      "Market",
      Buffer.from(accountAfter!.data)
    ) as any;
    expect(recentered.amm.center_price_nad.eq(oldCenter)).to.equal(false);
    expect(recentered.amm.last_adjustment_slot.toString()).to.equal(
      recenterSlot.toString()
    );
    expect(recentered.last_marginal_observation_nad.gt(new BN(0))).to.equal(true);
    // Ordinary swaps deliberately do not rebuild lending risk. The executable
    // curve advances and records its marginal observation; a later
    // risk-sensitive instruction materializes the exact snapshot.
    expect(recentered.curve_revision.gt(recentered.risk_revision)).to.equal(true);
  });

  it("executes a funded concentrated recenter with an active hLP below the SBF compute ceiling", async function () {
    const config = marketConfig();
    config.swapFeeBps = 0;
    config.divergenceFeeShareCapBps = 5_000;
    config.amm.rangeWidthNad = new BN("4000000000");
    config.amm.concentratedLiquidityShareNad = new BN("500000000");
    config.amm.adjustmentThresholdNad = new BN("1000");
    config.amm.adjustmentStepNad = new BN("1000");
    config.amm.minAdjustmentIntervalSlots = new BN(1);
    config.amm.divergenceFeeCoefficientNad = new BN("100000000000");
    const fixture = await addBalancedLiquidity(79, config, {
      baseDeposit: 100_000_000,
      quoteDeposit: 200_000_000,
      minYlp: 1,
      baseMint: 500_000_000,
      quoteMint: 500_000_000,
    });
    await openBaseHedge(fixture, 10_000_000);
    trackV2Instruction("depositSingleSided", this.test?.title);

    const hLpAccounts = hlpSwapAccounts(fixture);
    await swapBaseForQuote(fixture, hLpAccounts, 35_000_001, 1);
    // The first outward swap arms retention. The second pays the surcharge
    // into the custody-backed, non-quoteable Base bucket.
    await swapBaseForQuote(fixture, hLpAccounts, 35_000_000, 1);
    const accountBefore = svm.getAccount(fixture.market);
    expect(accountBefore).to.not.equal(null);
    const funded = accountCoder.decode(
      "Market",
      Buffer.from(accountBefore!.data)
    ) as any;
    expect(funded.base_hlp_vault.hlp_supply.gt(new BN(0))).to.equal(true);
    expect(funded.amm.retention_target_stale).to.equal(true);
    expect(funded.amm.retention_hard_cap_nad.gt(new BN(0))).to.equal(true);
    expect(
      funded.base_side.reserves.protected_recenter_reserve.gt(new BN(0))
    ).to.equal(true);
    funded.amm.price_ema_nad = funded.amm.last_trade_price_nad;
    funded.amm.retention_target_stale = true;
    const marketLayout = (accountCoder as any).accountLayouts.get("Market");
    const marketBody = Buffer.alloc(accountBefore!.data.length - 8);
    const marketBodyLength = marketLayout.layout.encode(funded, marketBody);
    const fundedData = Buffer.concat([
      (accountCoder as any).accountDiscriminator("Market"),
      marketBody.subarray(0, marketBodyLength),
    ]);
    expect(fundedData.length).to.equal(accountBefore!.data.length);
    svm.setAccount(fixture.market, {
      ...accountBefore!,
      data: new Uint8Array(fundedData),
    });
    const recenterSlot = BigInt(
      funded.amm.last_adjustment_slot.add(new BN(1)).toString()
    );
    svm.warpToSlot(recenterSlot);

    const oldCenter = funded.amm.center_price_nad;
    await swapBaseForQuote(fixture, hLpAccounts, 1_000_000, 1);
    trackV2Instruction("swap", this.test?.title);

    const accountAfter = svm.getAccount(fixture.market);
    expect(accountAfter).to.not.equal(null);
    const recentered = accountCoder.decode(
      "Market",
      Buffer.from(accountAfter!.data)
    ) as any;
    expect(recentered.amm.center_price_nad.eq(oldCenter)).to.equal(false);
    // The controller consumed the previously funded bucket. The triggering
    // swap may immediately retain a smaller surcharge for the next move.
    expect(
      recentered.base_side.reserves.protected_recenter_reserve.lt(
        funded.base_side.reserves.protected_recenter_reserve
      )
    ).to.equal(true);
    expect(recentered.base_hlp_vault.hlp_supply.gt(new BN(0))).to.equal(true);
    expect(recentered.last_marginal_observation_nad.gt(new BN(0))).to.equal(true);
    expect(recentered.curve_revision.gt(recentered.risk_revision)).to.equal(true);
  });

  it("measures O(1) concentrated hLP swap with fee compounding", async function () {
    this.timeout(120_000);

    const config = marketConfig();
    config.divergenceFeeShareCapBps = 2_000;
    config.amm.rangeWidthNad = new BN("4000000000");
    config.amm.concentratedLiquidityShareNad = new BN("500000000");
    config.amm.adjustmentThresholdNad = new BN("10000000");
    config.amm.adjustmentStepNad = new BN("1000000");
    config.amm.minAdjustmentIntervalSlots = new BN(1);
    config.amm.divergenceFeeCoefficientNad = new BN("10000000000");
    config.amm.compoundingFeeBps = 4_000;
    const fixture = await addBalancedLiquidity(99, config, {
      // Match the locked native six-decimal Spot fixture exactly: 1m/2m
      // ordinary depth, 100k/200k hLP deposits, and a 350k base input.
      baseDeposit: 1_000_000_000_000n,
      quoteDeposit: 2_000_000_000_000n,
      minYlp: 1,
      baseMint: 2_000_000_000_000n,
      quoteMint: 3_000_000_000_000n,
    });
    await openBaseHedge(fixture, 100_000_000_000);
    await openQuoteHedge(fixture, 200_000_000_000);

    const measurement = await swapBaseForQuote(
      fixture,
      hlpSwapAccounts(fixture),
      350_000_000_000,
      1
    );
    expect(measurement.computeUnits <= 350_000n).to.equal(true);
    trackV2Instruction("swap", this.test?.title);
  });

  it("executes active concentrated hLP spot swaps in both directions", async function () {
    this.timeout(120_000);

    const interestLiabilityTotal = (side: any) =>
      BigInt(side.fees.interest_liability.toString()) +
      BigInt(side.fees.unallocated_interest_liability.toString()) +
      BigInt(side.fees.referral_interest_liability.toString()) +
      BigInt(side.fees.interest_protocol_fee_liability.toString()) +
      BigInt(side.fees.interest_buyback_fee_liability.toString());

    for (const testCase of [
      { seed: 97, assetIn: "quote", exactAssetIn: 15_000_000 },
      { seed: 79, assetIn: "base", exactAssetIn: 35_000_000 },
    ] as const) {
      const config = marketConfig();
      config.amm.rangeWidthNad = new BN("4000000000");
      config.amm.concentratedLiquidityShareNad = new BN("500000000");
      const fixture = await addBalancedLiquidity(testCase.seed, config, {
        // Match the native funding-settlement fixture at 100x scale:
        // 150m/300m ordinary depth plus 10m/20m hLP deposits.
        baseDeposit: 150_000_000,
        quoteDeposit: 300_000_000,
        minYlp: 1,
        baseMint: 500_000_000,
        quoteMint: 500_000_000,
      });
      const baseHedge = await openBaseHedge(fixture, 10_000_000);
      const quoteHedge = await openQuoteHedge(fixture, 20_000_000);
      trackV2Instruction("depositSingleSided", this.test?.title);

      const marketBeforeAccount = svm.getAccount(fixture.market);
      expect(marketBeforeAccount).to.not.equal(null);
      const marketBefore = accountCoder.decode(
        "Market",
        Buffer.from(marketBeforeAccount!.data)
      ) as any;
      expect(marketBefore.debt.fixed_base_shares.toString()).to.equal("0");
      expect(marketBefore.debt.fixed_quote_shares.toString()).to.equal("0");
      expect(marketBefore.debt.fixed_base_principal.toString()).to.equal("0");
      expect(marketBefore.debt.fixed_quote_principal.toString()).to.equal("0");
      expect(marketBefore.debt.isolated_base_shares.toString()).to.equal("0");
      expect(marketBefore.debt.isolated_quote_shares.toString()).to.equal("0");
      expect(marketBefore.debt.isolated_base_principal.toString()).to.equal("0");
      expect(marketBefore.debt.isolated_quote_principal.toString()).to.equal("0");
      // Accrue a deterministic 10% funding premium without public-borrow
      // interest. hLP funding does not grow live reserves, so changing only
      // these indexes is the exact post-accrual state the swap must settle.
      marketBefore.debt.base_borrow_index_nad = new BN("1100000000");
      marketBefore.debt.quote_borrow_index_nad = new BN("1100000000");
      const marketLayout = (accountCoder as any).accountLayouts.get("Market");
      const marketBody = Buffer.alloc(marketBeforeAccount!.data.length - 8);
      const marketBodyLength = marketLayout.layout.encode(marketBefore, marketBody);
      const accruedMarketData = Buffer.concat([
        (accountCoder as any).accountDiscriminator("Market"),
        marketBody.subarray(0, marketBodyLength),
      ]);
      expect(accruedMarketData.length).to.equal(marketBeforeAccount!.data.length);
      svm.setAccount(fixture.market, {
        ...marketBeforeAccount!,
        data: new Uint8Array(accruedMarketData),
      });
      expect(marketBefore.base_hlp_vault.hlp_supply.toString()).to.equal("10000000");
      expect(marketBefore.quote_hlp_vault.hlp_supply.toString()).to.equal("20000000");
      const marketPreview = decodePreviewMarketReturnData(
        await simulateReturnData(
          await program.methods
            .previewMarket()
            .accounts({
              market: fixture.market,
            })
            .transaction()
        )
      ) as any;
      expect(marketPreview.amm.explicitCurveBranch).to.equal(1);

      const interestCheckpointBefore = [
        marketBefore.base_hlp_vault.base_interest_growth_index_q64,
        marketBefore.base_hlp_vault.base_interest_remainder_q64,
        marketBefore.base_hlp_vault.base_interest_growth_remainder_scaled,
        marketBefore.base_hlp_vault.unallocated_base_interest_amount,
        marketBefore.base_hlp_vault.quote_interest_growth_index_q64,
        marketBefore.base_hlp_vault.quote_interest_remainder_q64,
        marketBefore.base_hlp_vault.quote_interest_growth_remainder_scaled,
        marketBefore.base_hlp_vault.unallocated_quote_interest_amount,
        marketBefore.quote_hlp_vault.base_interest_growth_index_q64,
        marketBefore.quote_hlp_vault.base_interest_remainder_q64,
        marketBefore.quote_hlp_vault.base_interest_growth_remainder_scaled,
        marketBefore.quote_hlp_vault.unallocated_base_interest_amount,
        marketBefore.quote_hlp_vault.quote_interest_growth_index_q64,
        marketBefore.quote_hlp_vault.quote_interest_remainder_q64,
        marketBefore.quote_hlp_vault.quote_interest_growth_remainder_scaled,
        marketBefore.quote_hlp_vault.unallocated_quote_interest_amount,
      ].map((value: any) => value.toString());
      const baseInterestVaultBefore = await getAccount(
        connection as any,
        fixture.baseInterestVault
      );
      const quoteInterestVaultBefore = await getAccount(
        connection as any,
        fixture.quoteInterestVault
      );

      const baseHlpYlpBefore = await getAccount(
        connection as any,
        baseHedge.hlpYlpAccount,
        undefined,
        TOKEN_2022_PROGRAM_ID
      );
      const quoteHlpYlpBefore = await getAccount(
        connection as any,
        quoteHedge.hlpYlpAccount,
        undefined,
        TOKEN_2022_PROGRAM_ID
      );
      const ownerBaseBefore = await getAccount(connection as any, fixture.ownerBaseAccount);
      const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);
      const assetInMint = testCase.assetIn === "base" ? fixture.baseMint : fixture.quoteMint;
      const assetOutMint = testCase.assetIn === "base" ? fixture.quoteMint : fixture.baseMint;
      const preview = decodePreviewSwapReturnData(
        await simulateReturnData(
          await program.methods
            .previewSwap({ exactAssetIn: new BN(testCase.exactAssetIn) })
            .accounts({
              market: fixture.market,
              futarchyAuthority,
              assetInMint,
              assetOutMint,
            })
            .transaction()
        )
      ) as any;
      trackV2Instruction("previewSwap", this.test?.title);

      const measurement = testCase.assetIn === "base"
        ? await swapBaseForQuote(
            fixture,
            hlpSwapAccounts(fixture),
            testCase.exactAssetIn,
            1
          )
        : await swapQuoteForBase(
            fixture,
            hlpSwapAccounts(fixture),
            testCase.exactAssetIn,
            1
          );
      recordSwapComputeScenario("concentrated_hlp_active", measurement);
      recordSwapComputeScenario("concentrated_hlp_funding_interest", measurement);
      trackV2Instruction("swap", this.test?.title);

      const swapEvent = cpiEvent(measurement.transaction, "swapExecuted");
      expect(swapEvent.assetInSide).to.equal(testCase.assetIn === "base" ? 0 : 1);
      expect(swapEvent.amountIn.toString()).to.equal(testCase.exactAssetIn.toString());
      expect(swapEvent.amountOut.toString()).to.equal(preview.amountOut.toString());
      expect(swapEvent.amountInAfterFee.toString()).to.equal(
        preview.amountInForQuote.toString()
      );
      expect(swapEvent.baseFee.toString()).to.equal(preview.baseFeeDebit.toString());
      expect(swapEvent.divergenceFee.toString()).to.equal(
        preview.divergenceSurchargeDebit.toString()
      );
      expect(swapEvent.volatilityFee.toString()).to.equal(
        preview.volatilitySurchargeDebit.toString()
      );
      expect(swapEvent.retainedFee.toString()).to.equal(preview.retainedSurcharge.toString());

      const ownerBaseAfter = await getAccount(connection as any, fixture.ownerBaseAccount);
      const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);
      if (testCase.assetIn === "base") {
        expect(ownerBaseBefore.amount - ownerBaseAfter.amount).to.equal(
          BigInt(testCase.exactAssetIn)
        );
        expect(ownerQuoteAfter.amount - ownerQuoteBefore.amount).to.equal(
          BigInt(preview.amountOut.toString())
        );
      } else {
        expect(ownerQuoteBefore.amount - ownerQuoteAfter.amount).to.equal(
          BigInt(testCase.exactAssetIn)
        );
        expect(ownerBaseAfter.amount - ownerBaseBefore.amount).to.equal(
          BigInt(preview.amountOut.toString())
        );
      }

      const marketAfterAccount = svm.getAccount(fixture.market);
      expect(marketAfterAccount).to.not.equal(null);
      const marketAfter = accountCoder.decode(
        "Market",
        Buffer.from(marketAfterAccount!.data)
      ) as any;
      const interestCheckpointAfter = [
        marketAfter.base_hlp_vault.base_interest_growth_index_q64,
        marketAfter.base_hlp_vault.base_interest_remainder_q64,
        marketAfter.base_hlp_vault.base_interest_growth_remainder_scaled,
        marketAfter.base_hlp_vault.unallocated_base_interest_amount,
        marketAfter.base_hlp_vault.quote_interest_growth_index_q64,
        marketAfter.base_hlp_vault.quote_interest_remainder_q64,
        marketAfter.base_hlp_vault.quote_interest_growth_remainder_scaled,
        marketAfter.base_hlp_vault.unallocated_quote_interest_amount,
        marketAfter.quote_hlp_vault.base_interest_growth_index_q64,
        marketAfter.quote_hlp_vault.base_interest_remainder_q64,
        marketAfter.quote_hlp_vault.base_interest_growth_remainder_scaled,
        marketAfter.quote_hlp_vault.unallocated_base_interest_amount,
        marketAfter.quote_hlp_vault.quote_interest_growth_index_q64,
        marketAfter.quote_hlp_vault.quote_interest_remainder_q64,
        marketAfter.quote_hlp_vault.quote_interest_growth_remainder_scaled,
        marketAfter.quote_hlp_vault.unallocated_quote_interest_amount,
      ].map((value: any) => value.toString());
      expect(interestCheckpointAfter).to.deep.equal(interestCheckpointBefore);
      const baseInterestVaultAfter = await getAccount(
        connection as any,
        fixture.baseInterestVault
      );
      const quoteInterestVaultAfter = await getAccount(
        connection as any,
        fixture.quoteInterestVault
      );
      const baseInterestCredit = baseInterestVaultAfter.amount - baseInterestVaultBefore.amount;
      const quoteInterestCredit = quoteInterestVaultAfter.amount - quoteInterestVaultBefore.amount;
      expect(baseInterestCredit + quoteInterestCredit > 0n).to.equal(true);
      for (const { beforeSide, afterSide, interestCredit } of [
        {
          beforeSide: marketBefore.base_side,
          afterSide: marketAfter.base_side,
          interestCredit: baseInterestCredit,
        },
        {
          beforeSide: marketBefore.quote_side,
          afterSide: marketAfter.quote_side,
          interestCredit: quoteInterestCredit,
        },
      ]) {
        const liabilityBefore = interestLiabilityTotal(beforeSide);
        const liabilityAfter = interestLiabilityTotal(afterSide);
        expect(liabilityAfter - liabilityBefore).to.equal(interestCredit);
        if (interestCredit === 0n) {
          expect(liabilityAfter).to.equal(liabilityBefore);
        }
        expect(afterSide.fees.unallocated_interest_liability.toString()).to.equal(
          beforeSide.fees.unallocated_interest_liability.toString()
        );
        expect(afterSide.fees.referral_interest_liability.toString()).to.equal(
          beforeSide.fees.referral_interest_liability.toString()
        );
      }
      expect(baseInterestCredit).to.equal(
        BigInt(marketAfter.base_side.fees.interest_vault_balance.toString()) -
          BigInt(marketBefore.base_side.fees.interest_vault_balance.toString())
      );
      expect(quoteInterestCredit).to.equal(
        BigInt(marketAfter.quote_side.fees.interest_vault_balance.toString()) -
          BigInt(marketBefore.quote_side.fees.interest_vault_balance.toString())
      );
      for (const vault of [marketAfter.base_hlp_vault, marketAfter.quote_hlp_vault]) {
        expect(vault.base_interest_checkpoint_q64.toString()).to.equal(
          marketAfter.base_side.fees.interest_growth_index_q64.toString()
        );
        expect(vault.quote_interest_checkpoint_q64.toString()).to.equal(
          marketAfter.quote_side.fees.interest_growth_index_q64.toString()
        );
      }
      if (baseInterestCredit > 0n) {
        expect(
          marketAfter.base_side.fees.interest_growth_index_q64.gt(
            marketBefore.base_side.fees.interest_growth_index_q64
          )
        ).to.equal(true);
      }
      if (quoteInterestCredit > 0n) {
        expect(
          marketAfter.quote_side.fees.interest_growth_index_q64.gt(
            marketBefore.quote_side.fees.interest_growth_index_q64
          )
        ).to.equal(true);
      }
      expect(marketAfter.base_side.reserves.live_reserve.toString()).to.equal(
        swapEvent.baseLiveReserve.toString()
      );
      expect(marketAfter.quote_side.reserves.live_reserve.toString()).to.equal(
        swapEvent.quoteLiveReserve.toString()
      );
      expect(swapEvent.baseLiveReserve.toString()).to.equal(
        (testCase.assetIn === "base"
          ? preview.reserveInLiveReserve
          : preview.reserveOutLiveReserve
        ).toString()
      );
      expect(swapEvent.quoteLiveReserve.toString()).to.equal(
        (testCase.assetIn === "base"
          ? preview.reserveOutLiveReserve
          : preview.reserveInLiveReserve
        ).toString()
      );

      const baseHlpYlpAfter = await getAccount(
        connection as any,
        baseHedge.hlpYlpAccount,
        undefined,
        TOKEN_2022_PROGRAM_ID
      );
      const quoteHlpYlpAfter = await getAccount(
        connection as any,
        quoteHedge.hlpYlpAccount,
        undefined,
        TOKEN_2022_PROGRAM_ID
      );
      expect(
        baseHlpYlpAfter.amount !== baseHlpYlpBefore.amount ||
          quoteHlpYlpAfter.amount !== quoteHlpYlpBefore.amount
      ).to.equal(true);
      expect(
        marketAfter.base_hlp_vault.debt_shares.toString() !==
          marketBefore.base_hlp_vault.debt_shares.toString() ||
          marketAfter.quote_hlp_vault.debt_shares.toString() !==
            marketBefore.quote_hlp_vault.debt_shares.toString()
      ).to.equal(true);
      expect(baseHlpYlpAfter.amount.toString()).to.equal(
        marketAfter.base_hlp_vault.ylp_shares.toString()
      );
      expect(quoteHlpYlpAfter.amount.toString()).to.equal(
        marketAfter.quote_hlp_vault.ylp_shares.toString()
      );

      const baseReserveVault = await getAccount(connection as any, fixture.baseReserveVault);
      const quoteReserveVault = await getAccount(connection as any, fixture.quoteReserveVault);
      expect(baseReserveVault.amount).to.equal(
        BigInt(marketAfter.base_side.reserves.cash_reserve.toString()) +
          BigInt(marketAfter.base_side.fees.swap_fee_custody_balance.toString()) +
          BigInt(marketAfter.base_side.reserves.base_hlp_backing_inventory.toString()) +
          BigInt(marketAfter.base_side.reserves.quote_hlp_backing_inventory.toString())
      );
      expect(quoteReserveVault.amount).to.equal(
        BigInt(marketAfter.quote_side.reserves.cash_reserve.toString()) +
          BigInt(marketAfter.quote_side.fees.swap_fee_custody_balance.toString()) +
          BigInt(marketAfter.quote_side.reserves.base_hlp_backing_inventory.toString()) +
          BigInt(marketAfter.quote_side.reserves.quote_hlp_backing_inventory.toString())
      );
      expect(measurement.computeUnits < LITESVM_COMPUTE_UNIT_LIMIT).to.equal(true);
    }
  });

  it("updates Dusk futarchy revenue, recipients, and authority", async function () {
    await initializeFinalMarket(52);
    const futarchyTreasury = Keypair.generate().publicKey;
    const buybacksVault = Keypair.generate().publicKey;
    const replacementTeamTreasury = Keypair.generate().publicKey;

    const updateRevenueTx = await program.methods
      .updateProtocolRevenue({
        swapBps: 10_000,
        interestBps: 250,
        maxReferralInterestShareBps: null,
        revenueDistribution: {
          futarchyTreasuryBps: 0,
          buybacksVaultBps: 0,
          teamTreasuryBps: 10_000,
        },
        protocolAuctionSplit: null,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(updateRevenueTx, [payer]);
    trackV2Instruction("updateProtocolRevenue", this.test?.title);

    const updateRecipientsTx = await program.methods
      .updateRevenueRecipients({
        futarchyTreasury,
        buybacksVault,
        teamTreasury: replacementTeamTreasury,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    await connection.sendTransaction(updateRecipientsTx, [payer]);
    trackV2Instruction("updateRevenueRecipients", this.test?.title);

    const updateAuthorityTx = await program.methods
      .updateFutarchyAuthority({
        newAuthority: payer.publicKey,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    await connection.sendTransaction(updateAuthorityTx, [payer]);
    trackV2Instruction("updateFutarchyAuthority", this.test?.title);

    const authorityAccount = svm.getAccount(futarchyAuthority);
    expect(authorityAccount).to.not.equal(null);
    const authority = accountCoder.decode(
      "FutarchyAuthority",
      Buffer.from(authorityAccount!.data)
    ) as any;
    expect(authority.revenue_share.swap_bps).to.equal(10_000);
    expect(authority.revenue_share.interest_bps).to.equal(250);
    expect(authority.recipients.futarchy_treasury.toString()).to.equal(
      futarchyTreasury.toString()
    );
    expect(authority.recipients.buybacks_vault.toString()).to.equal(buybacksVault.toString());
    expect(authority.recipients.team_treasury.toString()).to.equal(
      replacementTeamTreasury.toString()
    );

    await resetFutarchyDefaults();
  });

  it("toggles global and market reduce-only through the emergency signer", async function () {
    const fixture = await initializeFinalMarket(57);

    const globalTx = await program.methods
      .setGlobalReduceOnly({
        reduceOnly: true,
      })
      .accounts({
        authoritySigner: REDUCE_ONLY_EMERGENCY_AUTHORITY,
        futarchyAuthority,
      })
      .transaction();
    await sendTransactionWithUncheckedSigners(globalTx, [payer], [REDUCE_ONLY_EMERGENCY_AUTHORITY]);
    trackV2Instruction("setGlobalReduceOnly", this.test?.title);

    let authorityAccount = svm.getAccount(futarchyAuthority);
    expect(authorityAccount).to.not.equal(null);
    let authority = accountCoder.decode(
      "FutarchyAuthority",
      Buffer.from(authorityAccount!.data)
    ) as any;
    expect(authority.global_reduce_only).to.equal(true);

    const marketTx = await program.methods
      .setMarketReduceOnly({
        reduceOnly: true,
      })
      .accounts({
        market: fixture.market,
        authoritySigner: REDUCE_ONLY_EMERGENCY_AUTHORITY,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await sendTransactionWithUncheckedSigners(marketTx, [payer], [REDUCE_ONLY_EMERGENCY_AUTHORITY]);
    trackV2Instruction("setMarketReduceOnly", this.test?.title);

    const marketAccount = svm.getAccount(fixture.market);
    expect(marketAccount).to.not.equal(null);
    const decodedMarket = accountCoder.decode("Market", Buffer.from(marketAccount!.data)) as any;
    expect(decodedMarket.reduce_only).to.equal(true);

    await resetFutarchyDefaults();
    authorityAccount = svm.getAccount(futarchyAuthority);
    expect(authorityAccount).to.not.equal(null);
    authority = accountCoder.decode("FutarchyAuthority", Buffer.from(authorityAccount!.data)) as any;
    expect(authority.global_reduce_only).to.equal(false);
  });

  it("settles protocol swap and interest revenue from source-specific custody", async function () {
    const fixture = await addBalancedLiquidity(53);
    const treasury = Keypair.generate().publicKey;
    const stakingVault = Keypair.generate().publicKey;
    const treasuryAccounts = await createRecipientAssetAccounts(fixture, treasury);
    const stakingAccounts = await createRecipientAssetAccounts(fixture, stakingVault);

    const updateAuctionConfigTx = await program.methods
      .updateProtocolAuctionConfig({
        lane: { fee: {} },
        acceptedMint: fixture.quoteMint,
        params: null,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(updateAuctionConfigTx, [payer]);
    trackV2Instruction("updateProtocolAuctionConfig", this.test?.title);

    const updateAuctionRouteTx = await program.methods
      .updateProtocolAuctionRoute({
        lane: { fee: {} },
        soldMint: fixture.baseMint,
        referenceMarket: PublicKey.default,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        market: fixture.market,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(updateAuctionRouteTx, [payer]);
    trackV2Instruction("updateProtocolAuctionRoute", this.test?.title);

    const updateAuctionRecipientsTx = await program.methods
      .updateProtocolAuctionRecipients({
        lane: { fee: {} },
        treasury,
        stakingVault,
        treasuryBps: 10_000,
        stakingVaultBps: 0,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(updateAuctionRecipientsTx, [payer]);
    trackV2Instruction("updateProtocolAuctionRecipients", this.test?.title);

    const updateRevenueTx = await program.methods
      .updateProtocolRevenue({
        swapBps: 10_000,
        interestBps: 0,
        maxReferralInterestShareBps: null,
        revenueDistribution: null,
        protocolAuctionSplit: null,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(updateRevenueTx, [payer]);
    trackV2Instruction("updateProtocolRevenue", this.test?.title);

    await swapBaseForQuote(fixture);

    const settleTx = await program.methods
      .settleProtocolAuction({
        lane: { fee: {} },
        source: { swap: {} },
        soldAmount: new BN(3),
        maxPaymentAmount: new BN(1_000),
      })
      .accounts({
        bidder: payer.publicKey,
        market: fixture.market,
        futarchyAuthority,
        soldMint: fixture.baseMint,
        acceptedMint: fixture.quoteMint,
        soldVault: fixture.baseReserveVault,
        bidderPaymentAccount: fixture.ownerQuoteAccount,
        bidderReceiveAccount: fixture.ownerBaseAccount,
        treasuryPaymentAccount: treasuryAccounts.quoteAccount,
        stakingVaultPaymentAccount: stakingAccounts.quoteAccount,
        referenceMarket: fixture.market,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(settleTx, [payer]);
    trackV2Instruction("settleProtocolAuction", this.test?.title);

    const treasuryQuoteBalance = await getAccount(connection as any, treasuryAccounts.quoteAccount);
    expect(treasuryQuoteBalance.amount > 0n).to.equal(true);
    const marketAccount = svm.getAccount(fixture.market);
    expect(marketAccount).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(marketAccount!.data)) as any;
    expect(decoded.base_side.fees.swap_protocol_fee_liability.toNumber()).to.equal(0);
    expect(decoded.base_side.fees.swap_fee_custody_balance.toNumber()).to.equal(0);
    const baseReserveVault = await getAccount(connection as any, fixture.baseReserveVault);
    expect(baseReserveVault.amount).to.equal(
      BigInt(decoded.base_side.reserves.cash_reserve.toString()) +
        BigInt(decoded.base_side.fees.swap_fee_custody_balance.toString())
    );

    const updateInterestAuctionConfigTx = await program.methods
      .updateProtocolAuctionConfig({
        lane: { fee: {} },
        acceptedMint: fixture.baseMint,
        params: null,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(updateInterestAuctionConfigTx, [payer]);

    const updateInterestRevenueTx = await program.methods
      .updateProtocolRevenue({
        swapBps: 0,
        interestBps: 10_000,
        maxReferralInterestShareBps: null,
        revenueDistribution: null,
        protocolAuctionSplit: null,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(updateInterestRevenueTx, [payer]);

    const borrowPositionId = Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPositionAddress(
      fixture.market,
      borrowPositionId
    )[0];
    const depositTx = await program.methods
      .depositCollateral({
        positionId: borrowPositionId,
        depositAmount: new BN(20_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: fixture.ownerBaseAccount,
        borrowPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(depositTx, [payer]);

    const borrowTx = await program.methods
      .borrow({
        borrowAmount: new BN(10_000),
        minDebtAmountOut: new BN(10_000),
        minLiquidationCfBps: 8_500,
        referrer: null,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(borrowTx, [payer]);

    advanceClockByYear();
    const repayTx = await program.methods
      .repay({ repayAmount: new BN(5_000) })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        reserveVault: fixture.quoteReserveVault,
        interestVault: fixture.quoteInterestVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(repayTx, [payer]);

    const beforeInterestSettlementAccount = svm.getAccount(fixture.market);
    expect(beforeInterestSettlementAccount).to.not.equal(null);
    const beforeInterestSettlement = accountCoder.decode(
      "Market",
      Buffer.from(beforeInterestSettlementAccount!.data)
    ) as any;
    const interestLiability = BigInt(
      beforeInterestSettlement.quote_side.fees.interest_protocol_fee_liability.toString()
    );
    expect(interestLiability > 0n).to.equal(true);
    const quoteInterestVaultBefore = await getAccount(
      connection as any,
      fixture.quoteInterestVault
    );
    const quoteReserveVaultBefore = await getAccount(
      connection as any,
      fixture.quoteReserveVault
    );
    const quoteSwapCustodyBefore = BigInt(
      beforeInterestSettlement.quote_side.fees.swap_fee_custody_balance.toString()
    );

    const settleInterestTx = await program.methods
      .settleProtocolAuction({
        lane: { fee: {} },
        source: { interest: {} },
        soldAmount: new BN(interestLiability.toString()),
        maxPaymentAmount: new BN(1_000),
      })
      .accounts({
        bidder: payer.publicKey,
        market: fixture.market,
        futarchyAuthority,
        soldMint: fixture.quoteMint,
        acceptedMint: fixture.baseMint,
        soldVault: fixture.quoteInterestVault,
        bidderPaymentAccount: fixture.ownerBaseAccount,
        bidderReceiveAccount: fixture.ownerQuoteAccount,
        treasuryPaymentAccount: treasuryAccounts.baseAccount,
        stakingVaultPaymentAccount: stakingAccounts.baseAccount,
        referenceMarket: fixture.market,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(settleInterestTx, [payer]);

    const afterInterestSettlementAccount = svm.getAccount(fixture.market);
    expect(afterInterestSettlementAccount).to.not.equal(null);
    const afterInterestSettlement = accountCoder.decode(
      "Market",
      Buffer.from(afterInterestSettlementAccount!.data)
    ) as any;
    expect(
      afterInterestSettlement.quote_side.fees.interest_protocol_fee_liability.toNumber()
    ).to.equal(0);
    const quoteInterestVaultAfter = await getAccount(
      connection as any,
      fixture.quoteInterestVault
    );
    const quoteReserveVaultAfter = await getAccount(
      connection as any,
      fixture.quoteReserveVault
    );
    expect(quoteInterestVaultBefore.amount - quoteInterestVaultAfter.amount).to.equal(
      interestLiability
    );
    expect(quoteReserveVaultAfter.amount).to.equal(quoteReserveVaultBefore.amount);
    expect(
      BigInt(afterInterestSettlement.quote_side.fees.swap_fee_custody_balance.toString())
    ).to.equal(quoteSwapCustodyBefore);
    expect(quoteReserveVaultAfter.amount).to.equal(
      BigInt(afterInterestSettlement.quote_side.reserves.cash_reserve.toString()) +
        quoteSwapCustodyBefore
    );

    await resetFutarchyDefaults();
  });

  it("checkpoints active hLP vaults during swaps with canonical vault accounts", async function () {
    const fixture = await addBalancedLiquidity(51);
    const hedge = await openBaseHedge(fixture);
    const ylpBefore = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );

    const preview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: new BN(1_000) })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.baseMint,
            assetOutMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewSwap", this.test?.title);

    const activeHlpMeasurement = await swapBaseForQuote(fixture, hlpSwapAccounts(fixture));
    recordSwapComputeScenario("hlp_active", activeHlpMeasurement);
    trackV2Instruction("swap", this.test?.title);

    const ylpAfter = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ylpAfter.amount < ylpBefore.amount).to.equal(true);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_hlp_vault.hlp_supply.toNumber()).to.equal(10_000);
    expect(decoded.base_hlp_vault.ylp_shares.toNumber()).to.be.lessThan(14_142);
    expect(decoded.base_side.reserves.live_reserve.toString()).to.equal(
      preview.reserveInLiveReserve.toString()
    );
    expect(decoded.quote_side.reserves.live_reserve.toString()).to.equal(
      preview.reserveOutLiveReserve.toString()
    );
  });

  it("keeps an active base hLP vault exactly hedged through large opposite-direction swaps", async function () {
    this.timeout(120_000);

    const fixture = await addBalancedLiquidity(57, marketConfig(), {
      baseDeposit: 100_000_000_000,
      quoteDeposit: 100_000_000_000,
      minYlp: 1,
      baseMint: 500_000_000_000,
      quoteMint: 500_000_000_000,
    });
    const hedge = await openBaseHedge(fixture, 10_000_000_000);
    const ylpBefore = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );

    await swapBaseForQuote(
      fixture,
      hlpSwapAccounts(fixture),
      80_000_000_000,
      1
    );
    const residualAccount = svm.getAccount(fixture.market);
    expect(residualAccount).to.not.equal(null);
    const residualMarket = accountCoder.decode(
      "Market",
      Buffer.from(residualAccount!.data)
    ) as any;
    expect(residualMarket.base_hlp_vault.residual_exposure.isZero()).to.equal(true);

    await swapBaseForQuote(
      fixture,
      hlpSwapAccounts(fixture),
      5_000_000_000,
      1
    );

    const residualCorrectionMeasurement = await swapQuoteForBase(
      fixture,
      hlpSwapAccounts(fixture),
      5_000_000_000,
      1
    );
    recordSwapComputeScenario("hlp_active", residualCorrectionMeasurement);
    trackV2Instruction("swap", this.test?.title);

    const ylpAfter = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ylpAfter.amount).to.not.equal(ylpBefore.amount);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_hlp_vault.hlp_supply.toString()).to.equal("10000000000");
    expect(decoded.base_hlp_vault.residual_exposure.isZero()).to.equal(true);
  });

  it("checkpoints quote hLP vaults during opposite-direction swaps", async function () {
    const fixture = await addBalancedLiquidity(55);
    const hedge = await openQuoteHedge(fixture);
    const ylpBefore = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );

    await swapQuoteForBase(fixture, hlpSwapAccounts(fixture));
    trackV2Instruction("swap", this.test?.title);

    const ylpAfter = await getAccount(
      connection as any,
      hedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ylpAfter.amount < ylpBefore.amount).to.equal(true);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.quote_hlp_vault.hlp_supply.toNumber()).to.equal(20_000);
    expect(decoded.quote_hlp_vault.ylp_shares.toNumber()).to.be.lessThan(14_142);
  });

  it("pre-solves and finishes both aggregate hLP vaults in one large swap", async function () {
    const fixture = await addBalancedLiquidity(56, marketConfig(), {
      baseDeposit: 100_000_000_000,
      quoteDeposit: 200_000_000_000,
      minYlp: 1,
      baseMint: 500_000_000_000,
      quoteMint: 500_000_000_000,
    });
    const baseHedge = await openBaseHedge(fixture, 10_000_000_000);
    const quoteHedge = await openQuoteHedge(fixture, 20_000_000_000);
    const baseHlpYlpBefore = await getAccount(
      connection as any,
      baseHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const quoteHlpYlpBefore = await getAccount(
      connection as any,
      quoteHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const aggregateBeforeAccount = svm.getAccount(fixture.market);
    expect(aggregateBeforeAccount).to.not.equal(null);
    const aggregateBefore = accountCoder.decode(
      "Market",
      Buffer.from(aggregateBeforeAccount!.data)
    ) as any;
    expect(aggregateBefore.base_hlp_vault.residual_exposure.isZero()).to.equal(true);
    expect(aggregateBefore.quote_hlp_vault.residual_exposure.isZero()).to.equal(true);
    await swapBaseForQuote(
      fixture,
      hlpSwapAccounts(fixture),
      20_000_000_000,
      1
    );
    trackV2Instruction("swap", this.test?.title);

    const baseHlpYlpAfter = await getAccount(
      connection as any,
      baseHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const quoteHlpYlpAfter = await getAccount(
      connection as any,
      quoteHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(baseHlpYlpAfter.amount).to.not.equal(baseHlpYlpBefore.amount);
    expect(quoteHlpYlpAfter.amount).to.not.equal(quoteHlpYlpBefore.amount);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_hlp_vault.hlp_supply.toString()).to.equal("10000000000");
    expect(decoded.quote_hlp_vault.hlp_supply.toString()).to.equal("20000000000");
  });

  it("sets a yield recipient and claims non-compounding yLP swap fees", async function () {
    const fixture = await addBalancedLiquidity(48);
    const recipient = Keypair.generate().publicKey;
    const recipientBaseAccount = await createAccount(
      connection as any,
      payer,
      fixture.baseMint,
      recipient
    );
    const baseYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.ylpMint,
      fixture.baseMint,
      "ylp"
    )[0];

    const setRecipientTx = await program.methods
      .setYieldRecipient({
        tokenKind: { ylp: {} },
        recipient,
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        lpMint: fixture.ylpMint,
        yieldAccount: baseYieldAccount,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(setRecipientTx, [payer]);
    trackV2Instruction("setYieldRecipient", this.test?.title);

    await swapBaseForQuote(fixture);

    const claimTx = await program.methods
      .harvest({
        tokenKind: { ylp: {} },
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        lpMint: fixture.ylpMint,
        ownerLpAccount: fixture.ownerYlpAccount,
        reserveVault: fixture.baseReserveVault,
        interestVault: fixture.baseInterestVault,
        recipientAssetAccount: recipientBaseAccount,
        yieldAccount: baseYieldAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(claimTx, [payer]);
    trackV2Instruction("harvest", this.test?.title);

    const recipientBalance = await getAccount(connection as any, recipientBaseAccount);
    const yieldClaimedEvent = cpiEvent(claimTx, "yieldClaimed");
    expect(recipientBalance.amount).to.equal(2n);
    expect(yieldClaimedEvent.recipientCredit.toString()).to.equal(recipientBalance.amount.toString());
    const reserveVault = await getAccount(connection as any, fixture.baseReserveVault);

    const account = svm.getAccount(fixture.market);
    expect(account).to.not.equal(null);
    const decoded = accountCoder.decode("Market", Buffer.from(account!.data)) as any;
    expect(decoded.base_side.fees.swap_fee_liability.toNumber()).to.equal(1);
    expect(decoded.base_side.fees.unallocated_swap_fee_liability.toNumber()).to.equal(0);
    expect(decoded.base_side.fees.swap_fee_custody_balance.toNumber()).to.equal(1);
    expect(reserveVault.amount).to.equal(
      BigInt(decoded.base_side.reserves.cash_reserve.toString()) +
        BigInt(decoded.base_side.fees.swap_fee_custody_balance.toString())
    );
  });

  it("checkpoints yLP yield accounts during a Token-2022 transfer hook", async function () {
    const fixture = await addBalancedLiquidity(58);
    const recipient = Keypair.generate().publicKey;
    const destinationYlpAccount = await createToken2022Ata(fixture.ylpMint, recipient);
    const sourceBaseYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.ylpMint,
      fixture.baseMint,
      "ylp"
    )[0];
    const destinationBaseYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      recipient,
      fixture.ylpMint,
      fixture.baseMint,
      "ylp"
    )[0];
    const sourceQuoteYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      payer.publicKey,
      fixture.ylpMint,
      fixture.quoteMint,
      "ylp"
    )[0];
    const destinationQuoteYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      recipient,
      fixture.ylpMint,
      fixture.quoteMint,
      "ylp"
    )[0];
    const validationAccount = deriveYieldTransferHookValidationAddress(fixture.ylpMint)[0];
    await connection.sendTransaction(
      new Transaction().add(
        SystemProgram.transfer({
          fromPubkey: payer.publicKey,
          toPubkey: validationAccount,
          lamports: Number(svm.minimumBalanceForRentExemption(0n)),
        })
      ),
      [payer]
    );
    expect(svm.getAccount(validationAccount)?.owner.toString()).to.equal(
      SystemProgram.programId.toString()
    );
    expect(await initializeLpTransferHook(fixture, fixture.ylpMint)).to.deep.equal(
      validationAccount
    );
    // An otherwise identical transaction signed against LiteSVM's unchanged
    // blockhash has the same signature and is rejected as AlreadyProcessed.
    // Rotate the blockhash so this call actually exercises program idempotency.
    svm.expireBlockhash();
    await initializeLpTransferHook(fixture, fixture.ylpMint);
    expect(svm.getAccount(validationAccount)?.owner.toString()).to.equal(DUSK_PROGRAM_ID.toString());

    const baseHlpValidationAccount = await initializeLpTransferHook(
      fixture,
      fixture.baseHlpMint
    );
    expect(svm.getAccount(baseHlpValidationAccount)?.owner.toString()).to.equal(
      DUSK_PROGRAM_ID.toString()
    );

    const metas = buildLpTransferHookAccountMetas({
      lpMint: fixture.ylpMint,
      market: fixture.market,
      sourceOwner: payer.publicKey,
      destinationOwner: recipient,
      baseMint: fixture.baseMint,
      quoteMint: fixture.quoteMint,
      tokenKind: "ylp",
    });

    expect(metas.map((meta) => meta.pubkey.toString())).to.deep.equal([
      fixture.market.toString(),
      fixture.baseMint.toString(),
      fixture.quoteMint.toString(),
      sourceBaseYieldAccount.toString(),
      destinationBaseYieldAccount.toString(),
      sourceQuoteYieldAccount.toString(),
      destinationQuoteYieldAccount.toString(),
      DUSK_PROGRAM_ID.toString(),
      validationAccount.toString(),
    ]);
    expect(metas.map((meta) => meta.isWritable)).to.deep.equal([
      true,
      false,
      false,
      true,
      true,
      true,
      true,
      false,
      false,
    ]);
    const selfTransferMetas = buildLpTransferHookAccountMetas({
      lpMint: fixture.ylpMint,
      market: fixture.market,
      sourceOwner: payer.publicKey,
      destinationOwner: payer.publicKey,
      baseMint: fixture.baseMint,
      quoteMint: fixture.quoteMint,
      tokenKind: "ylp",
    });
    expect(selfTransferMetas.map((meta) => meta.pubkey.toString())).to.deep.equal([
      fixture.market.toString(),
      fixture.baseMint.toString(),
      fixture.quoteMint.toString(),
      sourceBaseYieldAccount.toString(),
      sourceBaseYieldAccount.toString(),
      sourceQuoteYieldAccount.toString(),
      sourceQuoteYieldAccount.toString(),
      DUSK_PROGRAM_ID.toString(),
      validationAccount.toString(),
    ]);

    await initializeYieldAccounts(fixture, recipient, fixture.ylpMint, "ylp");
    // LiteSVM keeps one blockhash until explicitly rotated. A byte-identical
    // signed re-entry otherwise fails replay protection before reaching Dusk.
    svm.expireBlockhash();
    await initializeYieldAccounts(fixture, recipient, fixture.ylpMint, "ylp", true);
    await swapBaseForQuote(fixture);
    // Permissionless idempotent re-entry must not erase the source holder's
    // already-earned, not-yet-checkpointed interval.
    svm.expireBlockhash();
    await initializeYieldAccounts(fixture, payer.publicKey, fixture.ylpMint, "ylp", true);

    const transferIx = await createTransferCheckedWithTransferHookInstruction(
      connection as any,
      fixture.ownerYlpAccount,
      fixture.ylpMint,
      destinationYlpAccount,
      payer.publicKey,
      BigInt(10_000),
      6,
      [],
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const externalHookMeasurement = await connection.sendTransactionMeasured(
      new Transaction().add(transferIx),
      [payer]
    );
    recordExternalTransferHookComputeUnits(externalHookMeasurement.computeUnits);

    const sourceBaseYieldData = svm.getAccount(sourceBaseYieldAccount);
    const destinationBaseYieldData = svm.getAccount(destinationBaseYieldAccount);
    const sourceQuoteYieldData = svm.getAccount(sourceQuoteYieldAccount);
    const destinationQuoteYieldData = svm.getAccount(destinationQuoteYieldAccount);
    expect(sourceBaseYieldData).to.not.equal(null);
    expect(destinationBaseYieldData).to.not.equal(null);
    expect(sourceQuoteYieldData).to.not.equal(null);
    expect(destinationQuoteYieldData).to.not.equal(null);
    const sourceBaseYield = accountCoder.decode(
      "YieldAccount",
      Buffer.from(sourceBaseYieldData!.data)
    ) as any;
    const destinationBaseYield = accountCoder.decode(
      "YieldAccount",
      Buffer.from(destinationBaseYieldData!.data)
    ) as any;
    const sourceQuoteYield = accountCoder.decode(
      "YieldAccount",
      Buffer.from(sourceQuoteYieldData!.data)
    ) as any;
    const destinationQuoteYield = accountCoder.decode(
      "YieldAccount",
      Buffer.from(destinationQuoteYieldData!.data)
    ) as any;
    expect(sourceBaseYield.accrued_swap_fee_amount.toNumber()).to.equal(2);
    expect(destinationBaseYield.accrued_swap_fee_amount.toNumber()).to.equal(0);
    expect(sourceQuoteYield.accrued_swap_fee_amount.toNumber()).to.equal(0);
    expect(destinationQuoteYield.accrued_swap_fee_amount.toNumber()).to.equal(0);

    const sourceYlpAfter = await getAccount(
      connection as any,
      fixture.ownerYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const destinationYlpAfter = await getAccount(
      connection as any,
      destinationYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(sourceYlpAfter.amount).to.equal(130_421n);
    expect(destinationYlpAfter.amount).to.equal(10_000n);
  });

  it("deposits collateral, borrows fixed quote debt, repays, and withdraws idle collateral", async function () {
    const fixture = await addBalancedLiquidity(49);
    const borrowPositionId = Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPositionAddress(fixture.market, borrowPositionId)[0];
    const ownerBaseBefore = await getAccount(connection as any, fixture.ownerBaseAccount);
    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);

    const depositTx = await program.methods
      .depositCollateral({
        positionId: borrowPositionId,
        depositAmount: new BN(10_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: fixture.ownerBaseAccount,
        borrowPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(depositTx, [payer]);
    trackV2Instruction("depositCollateral", this.test?.title);

    const capacityPreview = decodePreviewBorrowCapacityReturnData(
      await simulateReturnData(
        await program.methods
          .previewBorrowCapacity({
            collateralAmount: new BN(10_000),
            projectedBorrowAmount: new BN(5_000),
          })
          .accounts({
            market: fixture.market,
            collateralAssetMint: fixture.baseMint,
            debtAssetMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewBorrowCapacity", this.test?.title);

    expect(capacityPreview.collateralAsset).to.deep.equal({ base: {} });
    expect(capacityPreview.debtAsset).to.deep.equal({ quote: {} });
    expect(capacityPreview.collateralAmount.toNumber()).to.equal(10_000);
    expect(capacityPreview.maxDebtByHealth.toNumber()).to.be.greaterThanOrEqual(5_000);
    expect(capacityPreview.maxDebt.toNumber()).to.be.greaterThanOrEqual(5_000);
    expect(capacityPreview.projectedDebtAmount.toNumber()).to.equal(5_000);
    expect(capacityPreview.projectedHealthBps.toNumber()).to.be.greaterThanOrEqual(11_000);
    expect(capacityPreview.projectedGlobalHealthContribution.toNumber()).to.be.greaterThan(0);
    expect(capacityPreview.projectedGlobalMarketHealthBps.toNumber()).to.be.greaterThanOrEqual(11_000);
    expect(capacityPreview.projectedEffectiveExistingDebtNad.toString()).to.equal("0");
    expect(capacityPreview.maxCfBps).to.be.greaterThan(0);
    expect(capacityPreview.liquidationCfBps).to.be.greaterThanOrEqual(capacityPreview.maxCfBps);
    expect(capacityPreview.liquidationDebtPerCollateralPriceNad.toNumber()).to.be.greaterThan(0);

    const borrowTx = await program.methods
      .borrow({
        borrowAmount: new BN(5_000),
        minDebtAmountOut: new BN(5_000),
        minLiquidationCfBps: 8_500,
        referrer: null,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(borrowTx, [payer]);
    trackV2Instruction("borrow", this.test?.title);

    let ownerBase = await getAccount(connection as any, fixture.ownerBaseAccount);
    let ownerQuote = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerBase.amount).to.equal(ownerBaseBefore.amount - 10_000n);
    expect(ownerQuote.amount).to.equal(ownerQuoteBefore.amount + 5_000n);

    let positionAccount = svm.getAccount(borrowPosition);
    expect(positionAccount).to.not.equal(null);
    let position = accountCoder.decode("BorrowPosition", Buffer.from(positionAccount!.data)) as any;
    expect(position.base_collateral.toNumber()).to.equal(10_000);
    expect(position.fixed_quote_shares.toNumber()).to.equal(5_000);
    expect(position.global_health_base_contribution_for_quote_debt.toNumber()).to.be.greaterThan(0);

    const debtMarketBefore = svm.getAccount(fixture.market);
    expect(debtMarketBefore).to.not.equal(null);
    const debtBefore = accountCoder.decode("Market", Buffer.from(debtMarketBefore!.data)) as any;
    const quoteBorrowIndexBefore = debtBefore.debt.quote_borrow_index_nad;
    const activeDebtAccrualSlot = svm.getClock().slot + 10_000n;
    svm.warpToSlot(activeDebtAccrualSlot);
    svm.expireBlockhash();

    const activeDebtSwapInput = new BN(1_000);
    const activeDebtSwapPreview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: activeDebtSwapInput })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.baseMint,
            assetOutMint: fixture.quoteMint,
          })
          .transaction()
      )
    ) as any;
    const activeDebtMeasurement = await swapBaseForQuote(fixture, [], activeDebtSwapInput.toNumber(), 1);
    recordSwapComputeScenario("cpmm_active_debt", activeDebtMeasurement);
    const debtMarketAfter = svm.getAccount(fixture.market);
    expect(debtMarketAfter).to.not.equal(null);
    const debtAfter = accountCoder.decode("Market", Buffer.from(debtMarketAfter!.data)) as any;
    expect(debtAfter.debt.quote_borrow_index_nad.gt(quoteBorrowIndexBefore)).to.equal(true);
    expect(debtAfter.debt.quote_last_accrual_slot.toString()).to.equal(activeDebtAccrualSlot.toString());

    const positionPreview = decodePreviewBorrowPositionReturnData(
      await simulateReturnData(
        await program.methods
          .previewBorrowPosition()
          .accounts({
            market: fixture.market,
            borrowPosition,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewBorrowPosition", this.test?.title);

    expect(positionPreview.owner.toString()).to.equal(payer.publicKey.toString());
    expect(positionPreview.positionId.toString()).to.equal(borrowPositionId.toString());
    expect(positionPreview.baseCollateral.toNumber()).to.equal(10_000);
    expect(positionPreview.fixedQuoteDebt.toNumber()).to.equal(5_000);
    expect(positionPreview.baseDebt.fixedDebt.toNumber()).to.equal(0);
    expect(positionPreview.quoteDebt.fixedDebt.toNumber()).to.equal(5_000);
    expect(positionPreview.quoteDebt.isLiquidatable).to.equal(false);
    expect(positionPreview.quoteDebt.maxRepayAmount.toNumber()).to.equal(0);

    const repayTx = await program.methods
      .repay({
        repayAmount: new BN(5_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        reserveVault: fixture.quoteReserveVault,
        interestVault: fixture.quoteInterestVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(repayTx, [payer]);
    trackV2Instruction("repay", this.test?.title);

    const withdrawTx = await program.methods
      .withdrawCollateral({
        withdrawAmount: new BN(10_000),
        minAssetAmountOut: new BN(10_000),
        minLiquidationCfBps: 0,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: fixture.ownerBaseAccount,
        borrowPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(withdrawTx, [payer]);
    trackV2Instruction("withdrawCollateral", this.test?.title);

    ownerBase = await getAccount(connection as any, fixture.ownerBaseAccount);
    ownerQuote = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerBase.amount).to.equal(
      ownerBaseBefore.amount - BigInt(activeDebtSwapInput.toString())
    );
    expect(ownerQuote.amount).to.equal(
      ownerQuoteBefore.amount + BigInt(activeDebtSwapPreview.amountOut.toString())
    );

    positionAccount = svm.getAccount(borrowPosition);
    expect(positionAccount).to.not.equal(null);
    position = accountCoder.decode("BorrowPosition", Buffer.from(positionAccount!.data)) as any;
    expect(position.base_collateral.toNumber()).to.equal(0);
    expect(position.fixed_quote_shares.toNumber()).to.equal(0);
    expect(position.global_health_base_contribution_for_quote_debt.toNumber()).to.equal(0);

    const decoded = accountCoder.decode(
      "Market",
      Buffer.from(svm.getAccount(fixture.market)!.data)
    ) as any;
    expect(decoded.base_side.reserves.live_reserve.toString()).to.equal(
      activeDebtSwapPreview.reserveInLiveReserve.toString()
    );
    expect(decoded.quote_side.reserves.live_reserve.toString()).to.equal(
      activeDebtSwapPreview.reserveOutLiveReserve.toString()
    );
    expect(decoded.quote_side.reserves.cash_reserve.toString()).to.equal(
      activeDebtSwapPreview.reserveOutLiveReserve.toString()
    );
    expect(decoded.debt.fixed_quote_shares.toNumber()).to.equal(0);
  });

  it("permissioned referrals accrue a capped DAO-interest share and remain claimable", async function () {
    const fixture = await addBalancedLiquidity(69);
    const borrowPositionId = Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPositionAddress(fixture.market, borrowPositionId)[0];

    const unlistedReferrer = Keypair.generate().publicKey;
    const unlistedPartner = deriveReferralPartnerAddress(unlistedReferrer)[0];
    const unlistedAccrual = deriveReferralAccrualAddress(
      unlistedPartner,
      fixture.market,
      fixture.quoteMint
    )[0];
    const unlistedInitTx = await program.methods
      .initializeReferralAccrual()
      .accounts({
        payer: payer.publicKey,
        referralPartner: unlistedPartner,
        market: fixture.market,
        assetMint: fixture.quoteMint,
        referralAccrual: unlistedAccrual,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    let unlistedRejected = false;
    try {
      await connection.sendTransaction(unlistedInitTx, [payer]);
    } catch {
      unlistedRejected = true;
    }
    expect(unlistedRejected).to.equal(true);

    const referralPartner = await configureReferralPartner(payer.publicKey, 7_500);
    const referralAccrual = await initializeReferralAccrual(
      payer.publicKey,
      fixture.market,
      fixture.quoteMint
    );
    await updateInterestRevenue(10_000, 2_500);

    const overCapTx = await program.methods
      .updateProtocolRevenue({
        swapBps: null,
        interestBps: null,
        maxReferralInterestShareBps: 10_001,
        revenueDistribution: null,
        protocolAuctionSplit: null,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    let overCapRejected = false;
    try {
      await connection.sendTransaction(overCapTx, [payer]);
    } catch {
      overCapRejected = true;
    }
    expect(overCapRejected).to.equal(true);

    const invalidPartner = Keypair.generate().publicKey;
    const invalidPartnerTx = await program.methods
      .configureReferralPartner({
        referrer: invalidPartner,
        interestShareBps: 10_001,
        active: true,
      })
      .accounts({
        authoritySigner: payer.publicKey,
        futarchyAuthority,
        referralPartner: deriveReferralPartnerAddress(invalidPartner)[0],
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    let invalidPartnerRejected = false;
    try {
      await connection.sendTransaction(invalidPartnerTx, [payer]);
    } catch {
      invalidPartnerRejected = true;
    }
    expect(invalidPartnerRejected).to.equal(true);

    const depositTx = await program.methods
      .depositCollateral({
        positionId: borrowPositionId,
        depositAmount: new BN(20_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: fixture.ownerBaseAccount,
        borrowPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(depositTx, [payer]);

    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const borrowTx = await program.methods
      .borrow({
        borrowAmount: new BN(10_000),
        minDebtAmountOut: new BN(10_000),
        minLiquidationCfBps: 8_500,
        referrer: payer.publicKey,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(borrowTx, [payer]);

    const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerQuoteAfter.amount - ownerQuoteBefore.amount).to.equal(10_000n);
    let position = accountCoder.decode(
      "BorrowPosition",
      Buffer.from(svm.getAccount(borrowPosition)!.data)
    ) as any;
    expect(position.fixed_quote_shares.toNumber()).to.equal(10_000);
    expect(position.quote_referral_partner.toString()).to.equal(referralPartner.toString());
    expect(position.quote_referral_interest_share_bps).to.equal(2_500);

    const secondReferrer = Keypair.generate().publicKey;
    const secondPartner = await configureReferralPartner(secondReferrer, 2_000);
    const secondAccrual = await initializeReferralAccrual(
      secondReferrer,
      fixture.market,
      fixture.quoteMint
    );
    const rebindTx = await program.methods
      .borrow({
        borrowAmount: new BN(100),
        minDebtAmountOut: new BN(100),
        minLiquidationCfBps: 8_500,
        referrer: secondReferrer,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner: secondPartner,
        referralAccrual: secondAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    let rebindRejected = false;
    try {
      await connection.sendTransaction(rebindTx, [payer]);
    } catch {
      rebindRejected = true;
    }
    expect(rebindRejected).to.equal(true);

    advanceClockByYear();
    const repayTx = await program.methods
      .repay({ repayAmount: new BN(5_000) })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        reserveVault: fixture.quoteReserveVault,
        interestVault: fixture.quoteInterestVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(repayTx, [payer]);

    const interestVault = await getAccount(connection as any, fixture.quoteInterestVault);
    let accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    const accruedBeforeDeactivation = BigInt(accrual.amount.toString());
    expect(accruedBeforeDeactivation).to.equal((interestVault.amount * 2_500n) / 10_000n);
    expect(accruedBeforeDeactivation > 0n).to.equal(true);

    await configureReferralPartner(payer.publicKey, 7_500, false);
    const inactiveAccrualSetupTx = await program.methods
      .initializeReferralAccrual()
      .accounts({
        payer: payer.publicKey,
        referralPartner,
        market: fixture.market,
        assetMint: fixture.quoteMint,
        referralAccrual,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    await connection.sendTransaction(inactiveAccrualSetupTx, [payer]);

    const rejectedPositionId = Keypair.generate().publicKey;
    const rejectedPosition = deriveBorrowPositionAddress(fixture.market, rejectedPositionId)[0];
    const rejectedDepositTx = await program.methods
      .depositCollateral({
        positionId: rejectedPositionId,
        depositAmount: new BN(2_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: fixture.ownerBaseAccount,
        borrowPosition: rejectedPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(rejectedDepositTx, [payer]);
    const inactiveNewBindingTx = await program.methods
      .borrow({
        borrowAmount: new BN(100),
        minDebtAmountOut: new BN(100),
        minLiquidationCfBps: 8_500,
        referrer: payer.publicKey,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition: rejectedPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    let inactiveNewBindingRejected = false;
    try {
      await connection.sendTransaction(inactiveNewBindingTx, [payer]);
    } catch {
      inactiveNewBindingRejected = true;
    }
    expect(inactiveNewBindingRejected).to.equal(true);

    const ownerQuoteBeforeInactiveBorrow = await getAccount(
      connection as any,
      fixture.ownerQuoteAccount
    );
    const existingBoundBorrowAfterDeactivationTx = await program.methods
      .borrow({
        borrowAmount: new BN(100),
        minDebtAmountOut: new BN(100),
        minLiquidationCfBps: 8_500,
        referrer: null,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(existingBoundBorrowAfterDeactivationTx, [payer]);
    const ownerQuoteAfterInactiveBorrow = await getAccount(
      connection as any,
      fixture.ownerQuoteAccount
    );
    expect(
      ownerQuoteAfterInactiveBorrow.amount - ownerQuoteBeforeInactiveBorrow.amount
    ).to.equal(100n);
    position = accountCoder.decode(
      "BorrowPosition",
      Buffer.from(svm.getAccount(borrowPosition)!.data)
    ) as any;
    expect(position.quote_referral_interest_share_bps).to.equal(2_500);

    advanceClockByYear();
    const interestVaultBeforeInactiveRepay = await getAccount(
      connection as any,
      fixture.quoteInterestVault
    );
    const postDeactivationRepayTx = await program.methods
      .repay({ repayAmount: new BN(1_000) })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        reserveVault: fixture.quoteReserveVault,
        interestVault: fixture.quoteInterestVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(postDeactivationRepayTx, [payer]);
    accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    const interestVaultAfterInactiveRepay = await getAccount(
      connection as any,
      fixture.quoteInterestVault
    );
    const interestCreditAfterDeactivation =
      interestVaultAfterInactiveRepay.amount - interestVaultBeforeInactiveRepay.amount;
    const accruedAfterDeactivation =
      accruedBeforeDeactivation + (interestCreditAfterDeactivation * 2_500n) / 10_000n;
    expect(BigInt(accrual.amount.toString())).to.equal(accruedAfterDeactivation);

    const rotatedRecipient = Keypair.generate().publicKey;
    const recipientTokenAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      rotatedRecipient
    );
    const rotateTx = await program.methods
      .setReferralRecipient({ recipient: rotatedRecipient })
      .accounts({
        authority: payer.publicKey,
        referralPartner,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(rotateTx, [payer]);

    const claimTx = await program.methods
      .claimReferralInterest()
      .accounts({
        market: fixture.market,
        authority: payer.publicKey,
        referralPartner,
        assetMint: fixture.quoteMint,
        referralAccrual,
        interestVault: fixture.quoteInterestVault,
        recipientTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(claimTx, [payer]);
    trackV2Instruction("claimReferralInterest", this.test?.title);

    expect((await getAccount(connection as any, recipientTokenAccount)).amount).to.equal(
      accruedAfterDeactivation
    );
    accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    expect(accrual.amount.toNumber()).to.equal(0);
    position = accountCoder.decode(
      "BorrowPosition",
      Buffer.from(svm.getAccount(borrowPosition)!.data)
    ) as any;
    expect(position.quote_referral_partner.toString()).to.equal(referralPartner.toString());
    expect(position.quote_referral_interest_share_bps).to.equal(2_500);
  });
  it("binds leverage referrals without changing debt and accrues on interest repayment", async function () {
    const fixture = await addBalancedLiquidity(70);
    await updateInterestRevenue(10_000, 5_000);
    const referralPartner = await configureReferralPartner(payer.publicKey, 5_000);
    const referralAccrual = await initializeReferralAccrual(
      payer.publicKey,
      fixture.market,
      fixture.quoteMint
    );

    const positionId = Keypair.generate().publicKey;
    const leveragePosition = deriveLeveragePositionAddress(fixture.market, positionId)[0];
    const leverageCollateralVault = deriveLeverageCollateralVaultAddress(
      fixture.market,
      fixture.baseMint
    )[0];
    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const openTx = await program.methods
      .openLeverage({
        positionId,
        debtAsset: 1,
        marginAmount: new BN(1_000),
        multiplierBps: new BN(20_000),
        minCollateralOut: new BN(1),
        referrer: payer.publicKey,
        positionOwner: null,
        limitPriceNad: new BN(0),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        payer: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        leverageCollateralVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        referralPartner,
        referralAccrual,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(openTx, [payer]);
    trackV2Instruction("openLeverage", this.test?.title);

    let position = accountCoder.decode(
      "LeveragePosition",
      Buffer.from(svm.getAccount(leveragePosition)!.data)
    ) as any;
    expect(position.debt_principal.toNumber()).to.equal(1_000);
    expect(position.debt_shares.toNumber()).to.equal(1_000);
    expect(position.referral_partner.toString()).to.equal(referralPartner.toString());
    expect(position.referral_interest_share_bps).to.equal(5_000);
    expect((await getAccount(connection as any, fixture.ownerQuoteAccount)).amount).to.equal(
      ownerQuoteBefore.amount - 1_000n
    );

    const increaseTx = await program.methods
      .increaseLeverage({
        debtAsset: 1,
        debtAmount: new BN(100),
        minCollateralOut: new BN(1),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        leverageCollateralVault,
        owner: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(increaseTx, [payer]);
    trackV2Instruction("increaseLeverage", this.test?.title);

    position = accountCoder.decode(
      "LeveragePosition",
      Buffer.from(svm.getAccount(leveragePosition)!.data)
    ) as any;
    expect(position.debt_principal.toNumber()).to.equal(1_100);
    expect(position.referral_partner.toString()).to.equal(referralPartner.toString());
    expect(position.referral_interest_share_bps).to.equal(5_000);
    let accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    expect(accrual.amount.toNumber()).to.equal(0);

    await configureReferralPartner(payer.publicKey, 1_000, false);
    advanceClockByYear();
    const addMarginTx = await program.methods
      .addLeverageMargin({
        debtAsset: 1,
        amount: new BN(500),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        debtReserveVault: fixture.quoteReserveVault,
        debtInterestVault: fixture.quoteInterestVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        referralPartner,
        referralAccrual,
        owner: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(addMarginTx, [payer]);
    trackV2Instruction("addLeverageMargin", this.test?.title);

    accrual = accountCoder.decode(
      "ReferralAccrual",
      Buffer.from(svm.getAccount(referralAccrual)!.data)
    ) as any;
    expect(accrual.amount.toNumber()).to.be.greaterThan(0);
    const market = accountCoder.decode(
      "Market",
      Buffer.from(svm.getAccount(fixture.market)!.data)
    ) as any;
    expect(market.quote_side.fees.referral_interest_liability.toString()).to.equal(
      accrual.amount.toString()
    );
  });
  it("liquidates unhealthy fixed quote debt after collateral price moves", async function () {
    const liquidationConfig = marketConfig();
    const fixture = await addBalancedLiquidity(54, liquidationConfig);
    const borrowPositionId = Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPositionAddress(fixture.market, borrowPositionId)[0];

    const depositTx = await program.methods
      .depositCollateral({
        positionId: borrowPositionId,
        depositAmount: new BN(10_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: fixture.ownerBaseAccount,
        borrowPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(depositTx, [payer]);

    const borrowTx = await program.methods
      .borrow({
        borrowAmount: new BN(14_500),
        minDebtAmountOut: new BN(14_500),
        minLiquidationCfBps: 8_500,
        referrer: null,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(borrowTx, [payer]);

    await swapBaseForQuote(fixture, [], 40_000, 30_000);
    const clock = svm.getClock();
    clock.slot += 10_000n;
    clock.unixTimestamp += 1_000n;
    svm.setClock(clock);

    const liquidationProbe = decodePreviewBorrowPositionReturnData(
      await simulateReturnData(
        await program.methods
          .previewBorrowPosition()
          .accounts({ market: fixture.market, borrowPosition })
          .transaction()
      )
    ) as any;
    expect(liquidationProbe.quoteDebt.isLiquidatable).to.equal(true);
    expect(liquidationProbe.quoteDebt.maxRepayAmount.gt(new BN(0))).to.equal(true);

    const positionBeforeAccount = svm.getAccount(borrowPosition);
    expect(positionBeforeAccount).to.not.equal(null);
    const positionBefore = accountCoder.decode(
      "BorrowPosition",
      Buffer.from(positionBeforeAccount!.data)
    ) as any;
    const baseCollateralBefore = positionBefore.base_collateral.toNumber();
    const quoteDebtSharesBefore = BigInt(positionBefore.fixed_quote_shares.toString());
    const ownerBaseBefore = await getAccount(connection as any, fixture.ownerBaseAccount);
    const triggerAuctionTx = await program.methods
      .startLiquidationAuction()
      .accounts({
        market: fixture.market,
        borrowPosition,
        debtAssetMint: fixture.quoteMint,
      })
      .transaction();
    await connection.sendTransaction(triggerAuctionTx, [payer]);
    trackV2Instruction("startLiquidationAuction", this.test?.title);

    const bidTx = await program.methods
      .fillLiquidationAuction({
        repayAmount: new BN(1_000),
        minCollateralOut: new BN(1),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        liquidator: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        interestVault: fixture.quoteInterestVault,
        collateralVault: fixture.baseCollateralVault,
        insuranceVault: fixture.quoteInsuranceVault,
        collateralInsuranceVault: fixture.baseInsuranceVault,
        liquidatorDebtAccount: fixture.ownerQuoteAccount,
        liquidatorCollateralAccount: fixture.ownerBaseAccount,
        borrowPosition,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(bidTx, [payer]);
    trackV2Instruction("fillLiquidationAuction", this.test?.title);

    const liquidationEvent = cpiEvent(bidTx, "borrowPositionLiquidated");
    expect(liquidationEvent.market.toString()).to.equal(fixture.market.toString());
    expect(liquidationEvent.borrowPosition.toString()).to.equal(borrowPosition.toString());
    expect(liquidationEvent.borrower.toString()).to.equal(payer.publicKey.toString());
    expect(liquidationEvent.liquidator.toString()).to.equal(payer.publicKey.toString());
    expect(liquidationEvent.debtAssetSide).to.equal(1);
    expect(liquidationEvent.repaidAmount.toString()).to.equal("1000");
    expect(
      BigInt(liquidationEvent.collateralSeized.toString()) >=
        BigInt(liquidationEvent.collateralToLiquidator.toString())
    ).to.equal(true);

    const ownerBaseAfter = await getAccount(connection as any, fixture.ownerBaseAccount);
    expect(ownerBaseAfter.amount > ownerBaseBefore.amount).to.equal(true);
    expect(liquidationEvent.collateralCredit.toString()).to.equal(
      (ownerBaseAfter.amount - ownerBaseBefore.amount).toString()
    );

    const positionAfterAccount = svm.getAccount(borrowPosition);
    expect(positionAfterAccount).to.not.equal(null);
    const positionAfter = accountCoder.decode(
      "BorrowPosition",
      Buffer.from(positionAfterAccount!.data)
    ) as any;
    expect(positionAfter.base_collateral.toNumber()).to.be.lessThan(baseCollateralBefore);
    expect(BigInt(positionAfter.fixed_quote_shares.toString()) < quoteDebtSharesBefore).to.equal(
      true
    );
  });

  it("settles an expired liquidation auction with external capital at its stored floor", async function () {
    const fixture = await addBalancedLiquidity(81, marketConfig());
    const borrowPositionId = Keypair.generate().publicKey;
    const borrowPosition = deriveBorrowPositionAddress(fixture.market, borrowPositionId)[0];

    const depositTx = await program.methods
      .depositCollateral({
        positionId: borrowPositionId,
        depositAmount: new BN(10_000),
      })
      .accounts({
        market: fixture.market,
        owner: payer.publicKey,
        assetMint: fixture.baseMint,
        collateralVault: fixture.baseCollateralVault,
        ownerAssetAccount: fixture.ownerBaseAccount,
        borrowPosition,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(depositTx, [payer]);

    const borrowTx = await program.methods
      .borrow({
        borrowAmount: new BN(14_500),
        minDebtAmountOut: new BN(14_500),
        minLiquidationCfBps: 8_500,
        referrer: null,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        owner: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        borrowPosition,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(borrowTx, [payer]);

    await swapBaseForQuote(fixture, [], 40_000, 30_000);
    const triggerClock = svm.getClock();
    triggerClock.slot += 10_000n;
    triggerClock.unixTimestamp += 1_000n;
    svm.setClock(triggerClock);

    const triggerTx = await program.methods
      .startLiquidationAuction()
      .accounts({
        market: fixture.market,
        borrowPosition,
        debtAssetMint: fixture.quoteMint,
      })
      .transaction();
    await connection.sendTransaction(triggerTx, [payer]);

    const beforeAccount = svm.getAccount(borrowPosition);
    expect(beforeAccount).to.not.equal(null);
    const before = accountCoder.decode(
      "BorrowPosition",
      Buffer.from(beforeAccount!.data)
    ) as any;
    const collateralBefore = before.base_collateral.toNumber();
    const debtSharesBefore = BigInt(before.fixed_quote_shares.toString());

    // External floor settlement becomes executable only after the auction's
    // exponential price reaches its stored floor.
    const expiredClock = svm.getClock();
    expiredClock.slot += 10_000n;
    expiredClock.unixTimestamp += 10_000n;
    svm.setClock(expiredClock);

    const settleTx = await program.methods
      .backstopLiquidationAuction({
        repayAmount: new BN(1_000),
        minCollateralOut: new BN(1),
        maxInsuranceDraw: new BN(0),
        maxSocializedLoss: new BN(0),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        liquidator: payer.publicKey,
        debtAssetMint: fixture.quoteMint,
        collateralAssetMint: fixture.baseMint,
        reserveVault: fixture.quoteReserveVault,
        interestVault: fixture.quoteInterestVault,
        collateralVault: fixture.baseCollateralVault,
        insuranceVault: fixture.quoteInsuranceVault,
        collateralInsuranceVault: fixture.baseInsuranceVault,
        liquidatorDebtAccount: fixture.ownerQuoteAccount,
        liquidatorCollateralAccount: fixture.ownerBaseAccount,
        borrowPosition,
        referralPartner: null,
        referralAccrual: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(settleTx, [payer]);
    trackV2Instruction("backstopLiquidationAuction", this.test?.title);

    const afterAccount = svm.getAccount(borrowPosition);
    expect(afterAccount).to.not.equal(null);
    const after = accountCoder.decode(
      "BorrowPosition",
      Buffer.from(afterAccount!.data)
    ) as any;
    expect(after.base_collateral.toNumber()).to.be.lessThan(collateralBefore);
    expect(BigInt(after.fixed_quote_shares.toString()) < debtSharesBefore).to.equal(true);
  });

  it("opens leverage through active concentrated hLP preparation", async function () {
    this.timeout(120_000);

    const config = marketConfig();
    config.amm.rangeWidthNad = new BN("4000000000");
    config.amm.concentratedLiquidityShareNad = new BN("500000000");
    const fixture = await addBalancedLiquidity(98, config, {
      baseDeposit: 100_000_000,
      quoteDeposit: 200_000_000,
      minYlp: 1,
      baseMint: 500_000_000,
      quoteMint: 500_000_000,
    });
    const baseHedge = await openBaseHedge(fixture, 10_000_000);
    const quoteHedge = await openQuoteHedge(fixture, 20_000_000);
    const baseHlpYlpBefore = await getAccount(
      connection as any,
      baseHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const quoteHlpYlpBefore = await getAccount(
      connection as any,
      quoteHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const marketBeforeAccount = svm.getAccount(fixture.market);
    expect(marketBeforeAccount).to.not.equal(null);
    const marketBefore = accountCoder.decode(
      "Market",
      Buffer.from(marketBeforeAccount!.data)
    ) as any;
    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);

    // Keep the round-trip unwind inside the protocol's fixed 2% impact cap;
    // this fixture validates active-hLP integration rather than cap rejection.
    const marginAmount = 2_000_000;
    const notional = 4_000_000;
    const preview = decodePreviewSwapReturnData(
      await simulateReturnData(
        await program.methods
          .previewSwap({ exactAssetIn: new BN(notional) })
          .accounts({
            market: fixture.market,
            futarchyAuthority,
            assetInMint: fixture.quoteMint,
            assetOutMint: fixture.baseMint,
          })
          .transaction()
      )
    ) as any;
    trackV2Instruction("previewSwap", this.test?.title);

    const { leveragePosition, leverageCollateralVault, measurement } =
      await openQuoteDebtLeverage(
        fixture,
        marginAmount,
        hlpSwapAccounts(fixture)
      );
    trackV2Instruction("openLeverage", this.test?.title);
    expect(measurement.computeUnits < LITESVM_COMPUTE_UNIT_LIMIT).to.equal(true);

    const openEvent = cpiEvent(measurement.transaction, "leveragePositionOpened");
    expect(openEvent.marginAmount.toString()).to.equal(marginAmount.toString());
    expect(openEvent.borrowedAmount.toString()).to.equal(marginAmount.toString());
    expect(openEvent.swap.assetInSide).to.equal(1);
    expect(openEvent.swap.amountIn.toString()).to.equal(notional.toString());
    expect(openEvent.swap.amountOut.toString()).to.equal(preview.amountOut.toString());
    expect(openEvent.swap.amountInAfterFee.toString()).to.equal(
      preview.amountInForQuote.toString()
    );
    expect(openEvent.swap.baseLiveReserve.toString()).to.equal(
      preview.reserveOutLiveReserve.toString()
    );
    expect(openEvent.swap.quoteLiveReserve.toString()).to.equal(
      preview.reserveInLiveReserve.toString()
    );

    const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerQuoteBefore.amount - ownerQuoteAfter.amount).to.equal(BigInt(marginAmount));
    const collateralVault = await getAccount(connection as any, leverageCollateralVault);
    expect(collateralVault.amount.toString()).to.equal(openEvent.collateralAmount.toString());
    const positionAccount = svm.getAccount(leveragePosition);
    expect(positionAccount).to.not.equal(null);
    const position = accountCoder.decode(
      "LeveragePosition",
      Buffer.from(positionAccount!.data)
    ) as any;
    expect(position.debt_principal.toString()).to.equal(marginAmount.toString());
    expect(position.collateral_amount.toString()).to.equal(openEvent.collateralAmount.toString());

    const marketAfterAccount = svm.getAccount(fixture.market);
    expect(marketAfterAccount).to.not.equal(null);
    const marketAfter = accountCoder.decode(
      "Market",
      Buffer.from(marketAfterAccount!.data)
    ) as any;
    expect(marketAfter.base_side.reserves.live_reserve.toString()).to.equal(
      openEvent.swap.baseLiveReserve.toString()
    );
    expect(marketAfter.quote_side.reserves.live_reserve.toString()).to.equal(
      openEvent.swap.quoteLiveReserve.toString()
    );
    const baseHlpYlpAfter = await getAccount(
      connection as any,
      baseHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const quoteHlpYlpAfter = await getAccount(
      connection as any,
      quoteHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(
      baseHlpYlpAfter.amount !== baseHlpYlpBefore.amount ||
        quoteHlpYlpAfter.amount !== quoteHlpYlpBefore.amount
    ).to.equal(true);
    expect(
      marketAfter.base_hlp_vault.debt_shares.toString() !==
        marketBefore.base_hlp_vault.debt_shares.toString() ||
        marketAfter.quote_hlp_vault.debt_shares.toString() !==
          marketBefore.quote_hlp_vault.debt_shares.toString()
    ).to.equal(true);
    expect(baseHlpYlpAfter.amount.toString()).to.equal(
      marketAfter.base_hlp_vault.ylp_shares.toString()
    );
    expect(quoteHlpYlpAfter.amount.toString()).to.equal(
      marketAfter.quote_hlp_vault.ylp_shares.toString()
    );

    const baseReserveVault = await getAccount(connection as any, fixture.baseReserveVault);
    const quoteReserveVault = await getAccount(connection as any, fixture.quoteReserveVault);
    expect(baseReserveVault.amount).to.equal(
      BigInt(marketAfter.base_side.reserves.cash_reserve.toString()) +
        BigInt(marketAfter.base_side.fees.swap_fee_custody_balance.toString()) +
        BigInt(marketAfter.base_side.reserves.base_hlp_backing_inventory.toString()) +
        BigInt(marketAfter.base_side.reserves.quote_hlp_backing_inventory.toString())
    );
    expect(quoteReserveVault.amount).to.equal(
      BigInt(marketAfter.quote_side.reserves.cash_reserve.toString()) +
        BigInt(marketAfter.quote_side.fees.swap_fee_custody_balance.toString()) +
        BigInt(marketAfter.quote_side.reserves.base_hlp_backing_inventory.toString()) +
        BigInt(marketAfter.quote_side.reserves.quote_hlp_backing_inventory.toString())
    );

    // Realize nonzero isolated-debt interest after predictive hLP positioning.
    // The interest belongs to the yLP/hLP ownership snapshot above, not to
    // shares minted or burned while preparing this close.
    advanceClockByYear();
    const ownerQuoteBeforeClose = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const quoteInterestVaultBefore = await getAccount(
      connection as any,
      fixture.quoteInterestVault
    );
    const closeTx = await program.methods
      .closeLeverage({
        debtAsset: 1,
        minAmountOut: new BN(0),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        debtInterestVault: fixture.quoteInterestVault,
        leverageCollateralVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        referralPartner: null,
        referralAccrual: null,
        leverageDelegation: null,
        delegatedProgram: null,
        authority: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .remainingAccounts(hlpSwapAccounts(fixture))
      .transaction();
    const closeMeasurement = await connection.sendTransactionMeasured(closeTx, [payer]);
    trackV2Instruction("closeLeverage", this.test?.title);
    expect(closeMeasurement.computeUnits < LITESVM_COMPUTE_UNIT_LIMIT).to.equal(true);

    const closeEvent = cpiEvent(closeMeasurement.transaction, "leveragePositionClosed");
    expect(BigInt(closeEvent.interestPaid.toString()) > 0n).to.equal(true);
    const ownerQuoteAfterClose = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerQuoteAfterClose.amount - ownerQuoteBeforeClose.amount).to.equal(
      BigInt(closeEvent.residual.toString())
    );
    const quoteInterestVaultAfter = await getAccount(
      connection as any,
      fixture.quoteInterestVault
    );
    const quoteInterestCredit = quoteInterestVaultAfter.amount - quoteInterestVaultBefore.amount;
    expect(quoteInterestCredit >= BigInt(closeEvent.interestPaid.toString())).to.equal(true);
    expect(svm.getAccount(leveragePosition)).to.equal(null);

    const afterCloseAccount = svm.getAccount(fixture.market);
    expect(afterCloseAccount).to.not.equal(null);
    const afterClose = accountCoder.decode(
      "Market",
      Buffer.from(afterCloseAccount!.data)
    ) as any;
    expect(afterClose.base_side.reserves.live_reserve.toString()).to.equal(
      closeEvent.swap.baseLiveReserve.toString()
    );
    expect(afterClose.quote_side.reserves.live_reserve.toString()).to.equal(
      closeEvent.swap.quoteLiveReserve.toString()
    );
    expect(
      BigInt(afterClose.quote_side.fees.interest_vault_balance.toString()) -
        BigInt(marketAfter.quote_side.fees.interest_vault_balance.toString())
    ).to.equal(quoteInterestCredit);
    const baseHlpYlpAfterClose = await getAccount(
      connection as any,
      baseHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const quoteHlpYlpAfterClose = await getAccount(
      connection as any,
      quoteHedge.hlpYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(
      baseHlpYlpAfterClose.amount.toString() !==
        marketAfter.base_hlp_vault.ylp_shares.toString() ||
        quoteHlpYlpAfterClose.amount.toString() !==
          marketAfter.quote_hlp_vault.ylp_shares.toString()
    ).to.equal(true);
    expect(baseHlpYlpAfterClose.amount.toString()).to.equal(
      afterClose.base_hlp_vault.ylp_shares.toString()
    );
    expect(quoteHlpYlpAfterClose.amount.toString()).to.equal(
      afterClose.quote_hlp_vault.ylp_shares.toString()
    );

    const baseReserveVaultAfterClose = await getAccount(
      connection as any,
      fixture.baseReserveVault
    );
    const quoteReserveVaultAfterClose = await getAccount(
      connection as any,
      fixture.quoteReserveVault
    );
    expect(baseReserveVaultAfterClose.amount).to.equal(
      BigInt(afterClose.base_side.reserves.cash_reserve.toString()) +
        BigInt(afterClose.base_side.fees.swap_fee_custody_balance.toString()) +
        BigInt(afterClose.base_side.reserves.base_hlp_backing_inventory.toString()) +
        BigInt(afterClose.base_side.reserves.quote_hlp_backing_inventory.toString())
    );
    expect(quoteReserveVaultAfterClose.amount).to.equal(
      BigInt(afterClose.quote_side.reserves.cash_reserve.toString()) +
        BigInt(afterClose.quote_side.fees.swap_fee_custody_balance.toString()) +
        BigInt(afterClose.quote_side.reserves.base_hlp_backing_inventory.toString()) +
        BigInt(afterClose.quote_side.reserves.quote_hlp_backing_inventory.toString())
    );

    const q64 = 1n << 64n;
    const quoteInterestGrowthAfter = BigInt(
      afterClose.quote_side.fees.interest_growth_index_q64.toString()
    );
    expect(afterClose.base_hlp_vault.quote_interest_checkpoint_q64.toString()).to.equal(
      quoteInterestGrowthAfter.toString()
    );
    expect(afterClose.quote_hlp_vault.quote_interest_checkpoint_q64.toString()).to.equal(
      quoteInterestGrowthAfter.toString()
    );
    expect(BigInt(afterClose.base_hlp_vault.quote_interest_remainder_q64.toString()) < q64).to.equal(true);
    expect(BigInt(afterClose.quote_hlp_vault.quote_interest_remainder_q64.toString()) < q64).to.equal(true);
  });

  it("opens leverage, updates exposure, and manages delegated permissions", async function () {
    const fixture = await addBalancedLiquidity(62);
    const { leveragePosition, leverageCollateralVault } = await openQuoteDebtLeverage(fixture);
    trackV2Instruction("openLeverage", this.test?.title);

    const positionAccount = svm.getAccount(leveragePosition);
    expect(positionAccount).to.not.equal(null);
    let position = accountCoder.decode("LeveragePosition", Buffer.from(positionAccount!.data)) as any;
    expect(position.owner.toString()).to.equal(payer.publicKey.toString());
    expect(position.market.toString()).to.equal(fixture.market.toString());
    expect(position.debt_asset).to.equal(1);
    expect(position.collateral_amount.toNumber()).to.be.greaterThan(0);
    expect(BigInt(position.debt_shares.toString()) > 0n).to.equal(true);
    const collateralAfterOpen = position.collateral_amount.toNumber();
    const debtSharesAfterOpen = BigInt(position.debt_shares.toString());

    const addMarginTx = await program.methods
      .addLeverageMargin({
        debtAsset: 1,
        amount: new BN(100),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        debtReserveVault: fixture.quoteReserveVault,
        debtInterestVault: fixture.quoteInterestVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        referralPartner: null,
        referralAccrual: null,
        owner: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(addMarginTx, [payer]);
    trackV2Instruction("addLeverageMargin", this.test?.title);

    let updatedPositionAccount = svm.getAccount(leveragePosition);
    expect(updatedPositionAccount).to.not.equal(null);
    position = accountCoder.decode(
      "LeveragePosition",
      Buffer.from(updatedPositionAccount!.data)
    ) as any;
    const debtSharesAfterAddMargin = BigInt(position.debt_shares.toString());
    expect(debtSharesAfterAddMargin < debtSharesAfterOpen).to.equal(true);

    const removeMarginTx = await program.methods
      .removeLeverageMargin({
        debtAsset: 1,
        amount: new BN(50),
        minAmountOut: new BN(50),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        owner: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(removeMarginTx, [payer]);
    trackV2Instruction("removeLeverageMargin", this.test?.title);

    updatedPositionAccount = svm.getAccount(leveragePosition);
    expect(updatedPositionAccount).to.not.equal(null);
    position = accountCoder.decode(
      "LeveragePosition",
      Buffer.from(updatedPositionAccount!.data)
    ) as any;
    expect(BigInt(position.debt_shares.toString()) > debtSharesAfterAddMargin).to.equal(true);

    const increaseTx = await program.methods
      .increaseLeverage({
        debtAsset: 1,
        debtAmount: new BN(100),
        minCollateralOut: new BN(1),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        leverageCollateralVault,
        owner: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(increaseTx, [payer]);
    trackV2Instruction("increaseLeverage", this.test?.title);

    updatedPositionAccount = svm.getAccount(leveragePosition);
    expect(updatedPositionAccount).to.not.equal(null);
    position = accountCoder.decode(
      "LeveragePosition",
      Buffer.from(updatedPositionAccount!.data)
    ) as any;
    expect(position.collateral_amount.toNumber()).to.be.greaterThan(collateralAfterOpen);
    const collateralAfterIncrease = position.collateral_amount.toNumber();

    const decreaseTx = await program.methods
      .decreaseLeverage({
        debtAsset: 1,
        collateralAmount: new BN(25),
        minRepayOut: new BN(1),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        debtInterestVault: fixture.quoteInterestVault,
        leverageCollateralVault,
        referralPartner: null,
        referralAccrual: null,
        owner: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(decreaseTx, [payer]);
    trackV2Instruction("decreaseLeverage", this.test?.title);

    updatedPositionAccount = svm.getAccount(leveragePosition);
    expect(updatedPositionAccount).to.not.equal(null);
    position = accountCoder.decode(
      "LeveragePosition",
      Buffer.from(updatedPositionAccount!.data)
    ) as any;
    expect(position.collateral_amount.toNumber()).to.equal(collateralAfterIncrease - 25);

    const leverageDelegation = deriveLeverageDelegationAddress(leveragePosition)[0];
    const delegatedProgram = Keypair.generate().publicKey;
    const createDelegationTx = await program.methods
      .createLeverageDelegation({
        debtAsset: 1,
        delegatedProgram,
        approvedActions: 1,
      })
      .accounts({
        market: fixture.market,
        leveragePosition,
        leverageDelegation,
        owner: payer.publicKey,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(createDelegationTx, [payer]);
    trackV2Instruction("createLeverageDelegation", this.test?.title);

    let delegationAccount = svm.getAccount(leverageDelegation);
    expect(delegationAccount).to.not.equal(null);
    let delegation = accountCoder.decode(
      "LeverageDelegation",
      Buffer.from(delegationAccount!.data)
    ) as any;
    expect(delegation.owner.toString()).to.equal(payer.publicKey.toString());
    expect(delegation.market.toString()).to.equal(fixture.market.toString());
    expect(delegation.position.toString()).to.equal(leveragePosition.toString());
    expect(delegation.debt_asset).to.equal(1);
    expect(delegation.delegated_program.toString()).to.equal(delegatedProgram.toString());
    expect(delegation.approved_actions).to.equal(1);

    const updatedProgram = Keypair.generate().publicKey;
    const updateDelegationTx = await program.methods
      .updateLeverageDelegation({
        debtAsset: 1,
        delegatedProgram: updatedProgram,
        approvedActions: 1 | 2 | 4,
      })
      .accounts({
        market: fixture.market,
        leveragePosition,
        leverageDelegation,
        owner: payer.publicKey,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(updateDelegationTx, [payer]);
    trackV2Instruction("updateLeverageDelegation", this.test?.title);

    delegationAccount = svm.getAccount(leverageDelegation);
    expect(delegationAccount).to.not.equal(null);
    delegation = accountCoder.decode(
      "LeverageDelegation",
      Buffer.from(delegationAccount!.data)
    ) as any;
    expect(delegation.delegated_program.toString()).to.equal(updatedProgram.toString());
    expect(delegation.approved_actions).to.equal(7);

    const closeDelegationTx = await program.methods
      .closeLeverageDelegation({
        position: leveragePosition,
      })
      .accounts({
        leverageDelegation,
        owner: payer.publicKey,
      })
      .transaction();
    await connection.sendTransaction(closeDelegationTx, [payer]);
    trackV2Instruction("closeLeverageDelegation", this.test?.title);

    delegationAccount = svm.getAccount(leverageDelegation);
    expect(delegationAccount).to.equal(null);
  });

  it("closes an owner-controlled leverage position", async function () {
    const fixture = await addBalancedLiquidity(63);
    const { leveragePosition, leverageCollateralVault } = await openQuoteDebtLeverage(fixture);
    trackV2Instruction("openLeverage", this.test?.title);

    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const closeTx = await program.methods
      .closeLeverage({
        debtAsset: 1,
        minAmountOut: new BN(0),
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        debtInterestVault: fixture.quoteInterestVault,
        leverageCollateralVault,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        referralPartner: null,
        referralAccrual: null,
        leverageDelegation: null,
        delegatedProgram: null,
        authority: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(closeTx, [payer]);
    trackV2Instruction("closeLeverage", this.test?.title);

    const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);
    expect(ownerQuoteAfter.amount >= ownerQuoteBefore.amount).to.equal(true);
    expect(svm.getAccount(leveragePosition)).to.equal(null);
  });

  it("closes a leverage position through a delegated callback settlement", async function () {
    const fixture = await addBalancedLiquidity(65);
    const { leveragePosition, leverageCollateralVault } = await openQuoteDebtLeverage(fixture);
    trackV2Instruction("openLeverage", this.test?.title);

    const leverageDelegation = deriveLeverageDelegationAddress(leveragePosition)[0];
    const createDelegationTx = await program.methods
      .createLeverageDelegation({
        debtAsset: 1,
        delegatedProgram: LEVERAGE_DELEGATE_PROGRAM_ID,
        approvedActions: LEVERAGE_DELEGATE_CLOSE,
      })
      .accounts({
        market: fixture.market,
        leveragePosition,
        leverageDelegation,
        owner: payer.publicKey,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(createDelegationTx, [payer]);
    trackV2Instruction("createLeverageDelegation", this.test?.title);

    const orderId = new BN(1);
    const order = deriveLeverageOrderAddress(leveragePosition, payer.publicKey, orderId)[0];
    const custodyTokenAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      order,
      Keypair.generate()
    );
    const executor = Keypair.generate();
    await connection.requestAirdrop(executor.publicKey, LAMPORTS_PER_SOL);
    const executorTokenAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      executor.publicKey,
      Keypair.generate()
    );

    const createOrderTx = await leverageDelegateProgram.methods
      .createLeverageOrder({
        orderId,
        kind: ORDER_KIND_TAKE_PROFIT,
        triggerCloseoutPriceNad: new BN(1),
        closeBps: 10_000,
      })
      .accounts({
        market: fixture.market,
        leveragePosition,
        order,
        owner: payer.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .transaction();
    await connection.sendTransaction(createOrderTx, [payer]);

    const beforeIx = await leverageDelegateProgram.methods
      .beforeTakeProfit({ orderId })
      .accounts({
        order,
        market: fixture.market,
        leveragePosition,
        leverageDelegation,
        custodyTokenAccount,
        collateralMint: fixture.baseMint,
        tokenMint: fixture.quoteMint,
        executor: executor.publicKey,
      })
      .instruction();
    const afterIx = await leverageDelegateProgram.methods
      .afterCloseOrder({ orderId })
      .accounts({
        order,
        owner: payer.publicKey,
        leveragePosition,
        leverageDelegation,
        custodyTokenAccount,
        executorTokenAccount,
        ownerTokenAccount: fixture.ownerQuoteAccount,
        tokenMint: fixture.quoteMint,
        executor: executor.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
      })
      .instruction();

    const ownerQuoteBefore = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const executorQuoteBefore = await getAccount(connection as any, executorTokenAccount);
    const delegatedCloseTx = await program.methods
      .delegatedCloseLeverage({
        debtAsset: 1,
        minAmountOut: new BN(0),
        closeBps: 10_000,
        delegated: {
          beforeIxData: Buffer.from(beforeIx.data),
          afterIxData: Buffer.from(afterIx.data),
          beforeAccountsLen: beforeIx.keys.length,
        },
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        debtInterestVault: fixture.quoteInterestVault,
        leverageCollateralVault,
        ownerDebtAccount: custodyTokenAccount,
        referralPartner: null,
        referralAccrual: null,
        leverageDelegation,
        delegatedProgram: LEVERAGE_DELEGATE_PROGRAM_ID,
        authority: executor.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .remainingAccounts([...beforeIx.keys, ...afterIx.keys])
      .transaction();
    await connection.sendTransaction(delegatedCloseTx, [payer, executor]);
    trackV2Instruction("delegatedCloseLeverage", this.test?.title);

    const ownerQuoteAfter = await getAccount(connection as any, fixture.ownerQuoteAccount);
    const executorQuoteAfter = await getAccount(connection as any, executorTokenAccount);
    const custodyAfter = await getAccount(connection as any, custodyTokenAccount);

    expect(ownerQuoteAfter.amount > ownerQuoteBefore.amount).to.equal(true);
    expect(executorQuoteAfter.amount > executorQuoteBefore.amount).to.equal(true);
    expect(custodyAfter.amount).to.equal(0n);
    expect(svm.getAccount(leveragePosition)).to.equal(null);
    expect(svm.getAccount(order)).to.equal(null);
  });

  it("liquidates an unhealthy leverage position", async function () {
    const config = marketConfig();
    const fixture = await addBalancedLiquidity(64, config);
    const { leveragePosition, leverageCollateralVault } = await openQuoteDebtLeverage(fixture);
    trackV2Instruction("openLeverage", this.test?.title);

    await swapBaseForQuote(fixture, [], 80_000, 1);

    const liquidatorQuoteAccount = await createAccount(
      connection as any,
      payer,
      fixture.quoteMint,
      payer.publicKey,
      Keypair.generate()
    );
    const liquidatorBefore = await getAccount(connection as any, liquidatorQuoteAccount);
    const liquidateTx = await program.methods
      .liquidateLeveragePosition({
        debtAsset: 1,
      })
      .accounts({
        market: fixture.market,
        futarchyAuthority,
        positionOwner: payer.publicKey,
        leveragePosition,
        debtMint: fixture.quoteMint,
        collateralMint: fixture.baseMint,
        debtReserveVault: fixture.quoteReserveVault,
        collateralReserveVault: fixture.baseReserveVault,
        debtInterestVault: fixture.quoteInterestVault,
        leverageCollateralVault,
        liquidatorDebtAccount: liquidatorQuoteAccount,
        ownerDebtAccount: fixture.ownerQuoteAccount,
        referralPartner: null,
        referralAccrual: null,
        liquidator: payer.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(liquidateTx, [payer]);
    trackV2Instruction("liquidateLeveragePosition", this.test?.title);

    const liquidatorAfter = await getAccount(connection as any, liquidatorQuoteAccount);
    expect(liquidatorAfter.amount >= liquidatorBefore.amount).to.equal(true);
    expect(svm.getAccount(leveragePosition)).to.equal(null);
  });

  it("runs the direct-yLP parameter proposal lifecycle", async function () {
    const fixture = await addBalancedLiquidity(61);
    const proposer = payer.publicKey;
    const nonce = new BN(1);
    const proposal = PublicKey.findProgramAddressSync(
      [
        Buffer.from("parameter_proposal"),
        fixture.market.toBuffer(),
        proposer.toBuffer(),
        nonce.toArrayLike(Buffer, "le", 8),
      ],
      DUSK_PROGRAM_ID
    )[0];
    const proposalSupport = PublicKey.findProgramAddressSync(
      [Buffer.from("proposal_support"), proposal.toBuffer(), proposer.toBuffer()],
      DUSK_PROGRAM_ID
    )[0];
    const baseYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      proposer,
      fixture.ylpMint,
      fixture.baseMint,
      "ylp"
    )[0];
    const quoteYieldAccount = deriveYieldAccountAddress(
      fixture.market,
      proposer,
      fixture.ylpMint,
      fixture.quoteMint,
      "ylp"
    )[0];
    const ylpMintBefore = await getMint(
      connection as any,
      fixture.ylpMint,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(svm.getAccount(fixture.baseHlpYlpVault)).to.equal(null);
    expect(svm.getAccount(fixture.quoteHlpYlpVault)).to.equal(null);
    const eligibleSupply = ylpMintBefore.supply;
    const sponsorship = (eligibleSupply + 99n) / 100n;
    const strictMajority = eligibleSupply / 2n + 1n;
    const additionalSupport = strictMajority - sponsorship;
    const ownerYlpBefore = await getAccount(
      connection as any,
      fixture.ownerYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );

    const createTx = await program.methods
      .createParameterProposal({
        nonce,
        update: {
          dailyBorrowLimit: {
            maxDailyBorrowBps: 1_900,
          },
        },
        metadata: {
          version: 1,
          title: "Lower daily borrow limit",
          descriptionUri: "ipfs://dusk-litesvm-parameter-proposal",
          descriptionSha256: Array(32).fill(1),
          descriptionLen: 1,
        },
        initialSupport: new BN(sponsorship.toString()),
      })
      .accounts({
        proposer,
        market: fixture.market,
        proposal,
        proposalSupport,
        ylpMint: fixture.ylpMint,
        proposerYlpAccount: fixture.ownerYlpAccount,
        baseYieldAccount,
        quoteYieldAccount,
        baseHlpYlpVault: fixture.baseHlpYlpVault,
        quoteHlpYlpVault: fixture.quoteHlpYlpVault,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(createTx, [payer]);
    expect(cpiEvent(createTx, "parameterProposalCreated").proposal.toString()).to.equal(
      proposal.toString()
    );
    trackV2Instruction("createParameterProposal", this.test?.title);

    const baseHlpYlp = await getAccount(
      connection as any,
      fixture.baseHlpYlpVault,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    const quoteHlpYlp = await getAccount(
      connection as any,
      fixture.quoteHlpYlpVault,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(baseHlpYlp.amount).to.equal(0n);
    expect(quoteHlpYlp.amount).to.equal(0n);

    let marketAccount = svm.getAccount(fixture.market);
    expect(marketAccount).to.not.equal(null);
    let market = accountCoder.decode("Market", Buffer.from(marketAccount!.data)) as any;
    expect(BigInt(market.governance_locked_ylp.toString())).to.equal(sponsorship);

    const queueTx = await program.methods
      .queueParameterProposal()
      .accounts({
        market: fixture.market,
        proposal,
        ylpMint: fixture.ylpMint,
        baseHlpYlpVault: fixture.baseHlpYlpVault,
        quoteHlpYlpVault: fixture.quoteHlpYlpVault,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    let insufficientQueueRejected = false;
    try {
      await connection.sendTransaction(queueTx, [payer]);
    } catch {
      insufficientQueueRejected = true;
    }
    expect(insufficientQueueRejected).to.equal(true);
    trackV2Instruction("queueParameterProposal", this.test?.title);

    const supportTx = await program.methods
      .supportParameterProposal({
        amount: new BN(additionalSupport.toString()),
      })
      .accounts({
        supporter: proposer,
        market: fixture.market,
        proposal,
        proposalSupport,
        ylpMint: fixture.ylpMint,
        supporterYlpAccount: fixture.ownerYlpAccount,
        baseYieldAccount,
        quoteYieldAccount,
        baseHlpYlpVault: fixture.baseHlpYlpVault,
        quoteHlpYlpVault: fixture.quoteHlpYlpVault,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(supportTx, [payer]);
    expect(cpiEvent(supportTx, "parameterProposalSupported").proposal.toString()).to.equal(
      proposal.toString()
    );
    expect(cpiEvent(supportTx, "parameterProposalQueued").proposal.toString()).to.equal(
      proposal.toString()
    );
    trackV2Instruction("supportParameterProposal", this.test?.title);

    marketAccount = svm.getAccount(fixture.market);
    expect(marketAccount).to.not.equal(null);
    market = accountCoder.decode("Market", Buffer.from(marketAccount!.data)) as any;
    expect(BigInt(market.governance_locked_ylp.toString())).to.equal(strictMajority);

    const executeTx = await program.methods
      .executeParameterProposal()
      .accounts({
        market: fixture.market,
        proposal,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    let earlyExecutionRejected = false;
    try {
      await connection.sendTransaction(executeTx, [payer]);
    } catch {
      earlyExecutionRejected = true;
    }
    expect(earlyExecutionRejected).to.equal(true);

    const clock = svm.getClock();
    clock.unixTimestamp += 7n * 24n * 60n * 60n + 1n;
    svm.setClock(clock);
    svm.expireBlockhash();

    const maturedExecuteTx = await program.methods
      .executeParameterProposal()
      .accounts({
        market: fixture.market,
        proposal,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(maturedExecuteTx, [payer]);
    expect(cpiEvent(maturedExecuteTx, "parameterProposalExecuted").proposal.toString()).to.equal(
      proposal.toString()
    );
    trackV2Instruction("executeParameterProposal", this.test?.title);

    marketAccount = svm.getAccount(fixture.market);
    expect(marketAccount).to.not.equal(null);
    market = accountCoder.decode("Market", Buffer.from(marketAccount!.data)) as any;
    expect(market.config.max_daily_borrow_bps).to.equal(1_900);
    expect(market.parameter_revisions[4].toNumber()).to.equal(1);

    const withdrawTx = await program.methods
      .withdrawParameterSupport()
      .accounts({
        supporter: proposer,
        market: fixture.market,
        proposal,
        proposalSupport,
        ylpMint: fixture.ylpMint,
        supporterYlpAccount: fixture.ownerYlpAccount,
        baseYieldAccount,
        quoteYieldAccount,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(withdrawTx, [payer]);
    expect(cpiEvent(withdrawTx, "parameterProposalSupportWithdrawn").proposal.toString()).to.equal(
      proposal.toString()
    );
    trackV2Instruction("withdrawParameterSupport", this.test?.title);

    const ownerYlpAfter = await getAccount(
      connection as any,
      fixture.ownerYlpAccount,
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    expect(ownerYlpAfter.amount).to.equal(ownerYlpBefore.amount);
    expect(svm.getAccount(proposalSupport)).to.equal(null);
    marketAccount = svm.getAccount(fixture.market);
    expect(marketAccount).to.not.equal(null);
    market = accountCoder.decode("Market", Buffer.from(marketAccount!.data)) as any;
    expect(market.governance_locked_ylp.toNumber()).to.equal(0);

    const queueNonce = new BN(2);
    const queuedProposal = PublicKey.findProgramAddressSync(
      [
        Buffer.from("parameter_proposal"),
        fixture.market.toBuffer(),
        proposer.toBuffer(),
        queueNonce.toArrayLike(Buffer, "le", 8),
      ],
      DUSK_PROGRAM_ID
    )[0];
    const queuedProposalSupport = PublicKey.findProgramAddressSync(
      [Buffer.from("proposal_support"), queuedProposal.toBuffer(), proposer.toBuffer()],
      DUSK_PROGRAM_ID
    )[0];
    const queuedSupport = eligibleSupply / 2n;
    const queueCreateTx = await program.methods
      .createParameterProposal({
        nonce: queueNonce,
        update: {
          dailyBorrowLimit: {
            maxDailyBorrowBps: 1_800,
          },
        },
        metadata: {
          version: 1,
          title: "Measure denominator-fall queue",
          descriptionUri: "ipfs://dusk-litesvm-denominator-fall-proposal",
          descriptionSha256: Array(32).fill(2),
          descriptionLen: 1,
        },
        initialSupport: new BN(queuedSupport.toString()),
      })
      .accounts({
        proposer,
        market: fixture.market,
        proposal: queuedProposal,
        proposalSupport: queuedProposalSupport,
        ylpMint: fixture.ylpMint,
        proposerYlpAccount: fixture.ownerYlpAccount,
        baseYieldAccount,
        quoteYieldAccount,
        baseHlpYlpVault: fixture.baseHlpYlpVault,
        quoteHlpYlpVault: fixture.quoteHlpYlpVault,
        token2022Program: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(queueCreateTx, [payer]);

    await connection.sendTransaction(
      new Transaction().add(
        createBurnCheckedInstruction(
          fixture.ownerYlpAccount,
          fixture.ylpMint,
          payer.publicKey,
          2n,
          6,
          [],
          TOKEN_2022_PROGRAM_ID
        )
      ),
      [payer]
    );

    const successfulQueueTx = await program.methods
      .queueParameterProposal()
      .accounts({
        market: fixture.market,
        proposal: queuedProposal,
        ylpMint: fixture.ylpMint,
        baseHlpYlpVault: fixture.baseHlpYlpVault,
        quoteHlpYlpVault: fixture.quoteHlpYlpVault,
        eventAuthority: eventAuthority(),
        program: DUSK_PROGRAM_ID,
      })
      .transaction();
    await connection.sendTransaction(successfulQueueTx, [payer]);
    expect(
      cpiEvent(successfulQueueTx, "parameterProposalQueued").proposal.toString()
    ).to.equal(queuedProposal.toString());
  });
});
