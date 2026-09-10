/**
 * Create a market on devnet using only the SDK.
 *
 * `scripts/v2/bootstrap_market.ts` does this too, but against the Anchor
 * program directly with a keypair on disk. This one goes through
 * `dusk.write.*` and `market-bootstrap`, which is the path the webapp has to
 * take, so it is the proof that path works before any UI is written.
 *
 * It seeds liquidity in the same run, deliberately. `/api/dusk/v1/config`
 * previews every market and fails as a unit, so a market that exists but
 * cannot be previewed takes the whole API down — and with it every script
 * that reads its configuration from there, including the one that would fix
 * it.
 *
 *   LABEL=sdk-demo node --experimental-strip-types scripts/devnet/create_market_via_sdk.ts
 */
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  NATIVE_MINT,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { createHash } from "crypto";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";
import {
  Dusk,
  createHookedLpMintInstructions,
  defaultMarketLaunchConfig,
  deriveMarketAddress,
  deriveMarketParamsHash,
  lpMintRent,
} from "../../packages/dusk-sdk/dist/index.js";

const API = process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";
const FAUCET_PROGRAM_ID =
  process.env.DUSK_FAUCET_PROGRAM_ID ?? "EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz";
/** Opening depth. Enough that the API can preview the market it just created. */
const SEED = BigInt(process.env.SEED ?? "1000");
/** `MAX_MINT_PER_REQUEST` in the deployed faucet, in raw atoms. */
const MAX_MINT_RAW = 10_000_000_000n;
/** `FaucetError::CooldownActive`. */
const COOLDOWN_ACTIVE = 6002;

const discriminator = (name: string) =>
  createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

function faucetMint(owner: PublicKey, mint: PublicKey, amount: bigint): TransactionInstruction {
  const programId = new PublicKey(FAUCET_PROGRAM_ID);
  const [authority] = PublicKey.findProgramAddressSync(
    [Buffer.from("faucet_authority"), programId.toBuffer()],
    programId,
  );
  const [claim] = PublicKey.findProgramAddressSync(
    [Buffer.from("faucet_claim"), owner.toBuffer(), mint.toBuffer()],
    programId,
  );
  const data = Buffer.alloc(8);
  data.writeBigUInt64LE(amount);
  return new TransactionInstruction({
    data: Buffer.concat([discriminator("faucet_mint"), data]),
    keys: [
      { isSigner: true, isWritable: true, pubkey: owner },
      { isSigner: false, isWritable: false, pubkey: owner },
      { isSigner: false, isWritable: false, pubkey: authority },
      { isSigner: false, isWritable: true, pubkey: getAssociatedTokenAddressSync(mint, owner) },
      { isSigner: false, isWritable: true, pubkey: claim },
      { isSigner: false, isWritable: true, pubkey: mint },
      { isSigner: false, isWritable: false, pubkey: SystemProgram.programId },
      { isSigner: false, isWritable: false, pubkey: TOKEN_PROGRAM_ID },
      { isSigner: false, isWritable: false, pubkey: ASSOCIATED_TOKEN_PROGRAM_ID },
    ],
    programId,
  });
}

async function main() {
  const label = process.env.LABEL ?? `sdk-${Date.now().toString(36)}`;
  const keypair = Keypair.fromSecretKey(
    Uint8Array.from(
      JSON.parse(
        readFileSync(process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json"), "utf8"),
      ),
    ),
  );
  const connection = new Connection(RPC, "confirmed");
  const config = (await (await fetch(`${API}/api/dusk/v1/config`)).json()).data;
  if (!config) throw new Error("the API did not return a deployment config");

  const owner = keypair.publicKey;
  const provider = new AnchorProvider(connection, new Wallet(keypair), {
    commitment: "confirmed",
  });
  const dusk = new Dusk({ programId: new PublicKey(config.programId), provider });

  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);
  const baseDecimals = Number(config.baseDecimals);
  const quoteDecimals = Number(config.quoteDecimals ?? config.baseDecimals);
  const baseUnit = 10n ** BigInt(baseDecimals);
  const quoteUnit = 10n ** BigInt(quoteDecimals);

  const send = async (instructions: TransactionInstruction[], signers: Keypair[] = []) => {
    const tx = new Transaction().add(
      ComputeBudgetProgram.setComputeUnitLimit({ units: 800_000 }),
      ...instructions,
    );
    const bh = await connection.getLatestBlockhash("confirmed");
    tx.recentBlockhash = bh.blockhash;
    tx.feePayer = owner;
    tx.sign(keypair, ...signers);
    const sig = await connection.sendRawTransaction(tx.serialize(), {
      preflightCommitment: "confirmed",
    });
    const result = await connection.confirmTransaction({ ...bh, signature: sig }, "confirmed");
    if (result.value.err) throw new Error(JSON.stringify(result.value.err));
    return sig;
  };

  // The treasury is chosen by governance, not by whoever creates the market.
  const futarchy = await dusk.get.futarchyAuthority();
  const teamTreasury = new PublicKey(futarchy.recipients.teamTreasury);
  const teamTreasuryWsolAccount = getAssociatedTokenAddressSync(
    NATIVE_MINT,
    teamTreasury,
    true,
  );
  console.log(`label          ${label}`);
  console.log(`team treasury  ${teamTreasury.toBase58()}`);

  const paramsHash = await deriveMarketParamsHash({
    namespace: "dusk-final-devnet",
    label,
    baseMint,
    quoteMint,
  });

  const rent = await lpMintRent(connection);
  const [market] = deriveMarketAddress(baseMint, quoteMint, paramsHash);
  console.log(`market         ${market.toBase58()}`);

  if (await connection.getAccountInfo(market, "confirmed")) {
    throw new Error(`market ${market.toBase58()} already exists; choose another LABEL`);
  }

  // Every LP mint is owned by the market that does not exist yet. That is
  // fine: the address is a PDA and is known before anything is on chain.
  const ylp = createHookedLpMintInstructions({
    payer: owner,
    decimals: baseDecimals,
    mintAuthority: market,
    transferHookProgramId: dusk.program.programId,
    lamports: rent,
  });
  const baseHlp = createHookedLpMintInstructions({
    payer: owner,
    decimals: baseDecimals,
    mintAuthority: market,
    transferHookProgramId: dusk.program.programId,
    lamports: rent,
  });
  const quoteHlp = createHookedLpMintInstructions({
    payer: owner,
    decimals: quoteDecimals,
    mintAuthority: market,
    transferHookProgramId: dusk.program.programId,
    lamports: rent,
  });

  await send(
    [
      createAssociatedTokenAccountIdempotentInstruction(
        owner,
        teamTreasuryWsolAccount,
        teamTreasury,
        NATIVE_MINT,
        TOKEN_PROGRAM_ID,
      ),
      ...ylp.instructions,
      ...baseHlp.instructions,
      ...quoteHlp.instructions,
    ],
    [ylp.keypair, baseHlp.keypair, quoteHlp.keypair],
  );
  console.log(`lp mints       ylp ${ylp.mint.toBase58()}`);
  console.log(`               base-hlp ${baseHlp.mint.toBase58()}`);
  console.log(`               quote-hlp ${quoteHlp.mint.toBase58()}`);

  const initSig = await send([
    await dusk.write.initializeMarketInstruction({
      payer: owner,
      baseMint,
      quoteMint,
      ylpMint: ylp.mint,
      baseHlpMint: baseHlp.mint,
      quoteHlpMint: quoteHlp.mint,
      teamTreasury,
      teamTreasuryWsolAccount,
      paramsHash,
      config: defaultMarketLaunchConfig(),
    }),
  ]);
  console.log(`initialized    ${initSig}`);

  const upper = label.toUpperCase().slice(0, 6);
  for (const [mint, kind] of [
    [ylp.mint, "YLP"],
    [baseHlp.mint, "BHLP"],
    [quoteHlp.mint, "QHLP"],
  ] as const) {
    await send([
      await dusk.write.initializeLpMetadataInstruction({
        payer: owner,
        market,
        lpMint: mint,
        name: `Dusk ${upper} ${kind}`,
        symbol: `${kind}`,
        uri: "https://ipfs.omnipair.fi/dusk-lp.json",
      }),
    ]);
  }
  console.log(`metadata       3 mints named`);

  // Seed before returning, so the API can preview what was just created.
  const ata = (mint: PublicKey) => getAssociatedTokenAddressSync(mint, owner);
  const held = async (mint: PublicKey) => {
    const balance = await connection
      .getTokenAccountBalance(ata(mint), "confirmed")
      .catch(() => null);
    return balance ? BigInt(balance.value.amount) : 0n;
  };
  const cooldown = new RegExp(`${COOLDOWN_ACTIVE}|0x${COOLDOWN_ACTIVE.toString(16)}`);
  const acquire = async (mint: PublicKey, want: bigint) => {
    const shortfall = want - (await held(mint));
    if (shortfall <= 0n) return;
    const ask = shortfall > MAX_MINT_RAW ? MAX_MINT_RAW : shortfall;
    try {
      await send([faucetMint(owner, mint, ask)]);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (!cooldown.test(message)) throw e;
      console.log(`  faucet cooldown on ${mint.toBase58().slice(0, 8)}, using held balance`);
    }
  };

  await acquire(baseMint, SEED * baseUnit);
  await acquire(quoteMint, SEED * quoteUnit);
  const [haveBase, haveQuote] = [await held(baseMint), await held(quoteMint)];
  if (haveBase < SEED * baseUnit || haveQuote < SEED * quoteUnit) {
    throw new Error(
      `market ${market.toBase58()} exists but cannot be seeded: holds ` +
        `${haveBase / baseUnit} base and ${haveQuote / quoteUnit} quote, needs ${SEED} of each. ` +
        `Seed it before the next config read, or the API cannot preview it.`,
    );
  }

  await send([
    createAssociatedTokenAccountIdempotentInstruction(
      owner,
      getAssociatedTokenAddressSync(ylp.mint, owner, false, TOKEN_2022_PROGRAM_ID),
      owner,
      ylp.mint,
      TOKEN_2022_PROGRAM_ID,
    ),
  ]);
  const seedSig = await send([
    await dusk.write.addLiquidityInstruction({
      baseMint,
      market,
      owner,
      ownerBaseAccount: ata(baseMint),
      ownerQuoteAccount: ata(quoteMint),
      ownerYlpAccount: getAssociatedTokenAddressSync(ylp.mint, owner, false, TOKEN_2022_PROGRAM_ID),
      quoteMint,
      ylpMint: ylp.mint,
      baseDepositAmount: (SEED * baseUnit).toString(),
      quoteDepositAmount: (SEED * quoteUnit).toString(),
      minYlpAmount: "0",
    }),
  ]);
  console.log(`seeded         ${SEED} a side — ${seedSig}`);
  console.log(`\nmarket ${market.toBase58()} is live and previewable`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
