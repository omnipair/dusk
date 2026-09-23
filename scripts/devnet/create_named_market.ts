/**
 * Create a market the way mainnet will: LP mints at vanity `yLP`/`hLP`
 * addresses, transfer hooks made usable, names per `lp-naming`, and per-mint
 * images and metadata pinned to IPFS before `initialize_lp_metadata` writes
 * the URIs on chain.
 *
 * The mints are `create_with_seed` accounts, so the only signer is the
 * creator; the seeds come from the vanity server, which grinds the Token-2022
 * derivation for that creator's key. Liquidity is seeded in the same run for
 * the same reason `create_market_via_sdk.ts` does it: the API previews every
 * market as a unit.
 *
 *   LABEL=named-1 VANITY_API_URL=https://vanity.omnipair.fi \
 *   PINATA_SIGNED_URL_ENDPOINT=http://localhost:3009/api/pinata/create-signed-url \
 *   node scripts/devnet/create_named_market.ts
 *
 * `PINATA_JWT` works instead of the signed-URL endpoint. `LP_METADATA_DRY_RUN=1`
 * renders and writes the metadata under target/lp-metadata without pinning or
 * naming anything on chain, and stops before touching the chain at all.
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
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import {
  DEFAULT_VANITY_SERVER_URL,
  Dusk,
  MARKET_LP_MINT_KINDS,
  createHookedLpMintWithSeedInstructions,
  defaultMarketLaunchConfig,
  deriveMarketAddress,
  deriveMarketParamsHash,
  grindMarketLpMintSeeds,
  hasLpMintAddressSuffix,
  lpMintRent,
  marketLpTokenNaming,
} from "../../packages/dusk-sdk/dist/index.js";
import { readTokenMetadata } from "../lp-metadata/logos.ts";
import { pinataConfigFromEnv } from "../lp-metadata/pinata.ts";
import { publishLpMetadata } from "../lp-metadata/publish.ts";

const API = process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";
const NETWORK = process.env.DUSK_NETWORK ?? "devnet";
const VANITY = process.env.VANITY_API_URL ?? DEFAULT_VANITY_SERVER_URL;
const FAUCET_PROGRAM_ID =
  process.env.DUSK_FAUCET_PROGRAM_ID ?? "EMmV9HKeQndxFd4duqp65rUSjikVWCPakBH1UjJJ32dz";
const SEED = BigInt(process.env.SEED ?? "1000");
const MAX_MINT_RAW = 10_000_000_000n;
const COOLDOWN_ACTIVE = 6002;
const DRY_RUN = process.env.LP_METADATA_DRY_RUN === "1";

const discriminator = (name: string) =>
  createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

function faucetMint(owner: PublicKey, mint: PublicKey, amount: bigint): TransactionInstruction {
  const programId = new PublicKey(FAUCET_PROGRAM_ID);
  const [authority] = PublicKey.findProgramAddressSync(
    [Buffer.from("faucet_authority"), programId.toBuffer()],
    programId
  );
  const [claim] = PublicKey.findProgramAddressSync(
    [Buffer.from("faucet_claim"), owner.toBuffer(), mint.toBuffer()],
    programId
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
  const label = process.env.LABEL ?? `named-${Date.now().toString(36)}`;
  const keypair = Keypair.fromSecretKey(
    Uint8Array.from(
      JSON.parse(
        readFileSync(process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json"), "utf8")
      )
    )
  );
  const connection = new Connection(RPC, "confirmed");
  const config = (await (await fetch(`${API}/api/dusk/v1/config`)).json()).data;
  if (!config) throw new Error("the API did not return a deployment config");

  const owner = keypair.publicKey;
  const provider = new AnchorProvider(connection, new Wallet(keypair), { commitment: "confirmed" });
  const dusk = new Dusk({ programId: new PublicKey(config.programId), provider });

  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);
  const baseDecimals = Number(config.baseDecimals);
  const quoteDecimals = Number(config.quoteDecimals ?? config.baseDecimals);
  const baseUnit = 10n ** BigInt(baseDecimals);
  const quoteUnit = 10n ** BigInt(quoteDecimals);

  // Asset symbols drive every LP name; read them from the asset metadata.
  const [baseMeta, quoteMeta] = await Promise.all([
    readTokenMetadata(connection, baseMint),
    readTokenMetadata(connection, quoteMint),
  ]);
  const baseSymbol = process.env.BASE_SYMBOL ?? baseMeta.symbol;
  const quoteSymbol = process.env.QUOTE_SYMBOL ?? quoteMeta.symbol;
  if (!baseSymbol || !quoteSymbol) {
    throw new Error("asset symbols are missing from metadata; set BASE_SYMBOL and QUOTE_SYMBOL");
  }
  const naming = marketLpTokenNaming({ baseSymbol, quoteSymbol });
  const pinata = DRY_RUN ? undefined : pinataConfigFromEnv();

  const paramsHash = await deriveMarketParamsHash({
    namespace: "dusk-final-devnet",
    label,
    baseMint,
    quoteMint,
  });
  const [market] = deriveMarketAddress(baseMint, quoteMint, paramsHash);
  console.log(`label          ${label}`);
  console.log(`assets         ${baseSymbol}/${quoteSymbol}`);
  console.log(`market         ${market.toBase58()}`);
  for (const kind of MARKET_LP_MINT_KINDS) {
    console.log(`${kind.padEnd(14)} ${naming[kind].symbol.padEnd(11)} ${naming[kind].name}`);
  }

  console.log(`grinding       ${VANITY}`);
  const started = Date.now();
  const seeds = await grindMarketLpMintSeeds({ base: owner, serverUrl: VANITY });
  console.log(`ground         3 seeds in ${((Date.now() - started) / 1000).toFixed(1)}s`);
  const mints = {
    ylp: seeds.ylp.mint,
    baseHlp: seeds.baseHlp.mint,
    quoteHlp: seeds.quoteHlp.mint,
  };
  for (const kind of MARKET_LP_MINT_KINDS) {
    if (!hasLpMintAddressSuffix(kind, mints[kind])) throw new Error(`${kind} mint lacks its suffix`);
    console.log(`${kind.padEnd(14)} ${mints[kind].toBase58()}`);
  }

  const outDir = join("target", "lp-metadata", market.toBase58());
  mkdirSync(outDir, { recursive: true });
  if (DRY_RUN) {
    const rendered = await publishLpMetadata({
      connection, network: NETWORK, market, baseMint, quoteMint, baseSymbol, quoteSymbol,
      mints, naming, outDir,
      logoUrls: { base: process.env.BASE_LOGO, quote: process.env.QUOTE_LOGO },
    });
    for (const kind of MARKET_LP_MINT_KINDS) console.log(`${kind.padEnd(14)} ${rendered[kind].image}`);
    console.log(`\ndry run: nothing sent to the chain or Pinata; files under ${outDir}`);
    return;
  }

  if (await connection.getAccountInfo(market, "confirmed")) {
    throw new Error(`market ${market.toBase58()} already exists; choose another LABEL`);
  }

  const send = async (instructions: TransactionInstruction[], signers: Keypair[] = []) => {
    const tx = new Transaction().add(
      ComputeBudgetProgram.setComputeUnitLimit({ units: 800_000 }),
      ...instructions
    );
    const bh = await connection.getLatestBlockhash("confirmed");
    tx.recentBlockhash = bh.blockhash;
    tx.feePayer = owner;
    tx.sign(keypair, ...signers);
    const sig = await connection.sendRawTransaction(tx.serialize(), { preflightCommitment: "confirmed" });
    const result = await connection.confirmTransaction({ ...bh, signature: sig }, "confirmed");
    if (result.value.err) throw new Error(JSON.stringify(result.value.err));
    return sig;
  };

  const futarchy = await dusk.get.futarchyAuthority();
  const teamTreasury = new PublicKey(futarchy.recipients.teamTreasury);
  const teamTreasuryWsolAccount = getAssociatedTokenAddressSync(NATIVE_MINT, teamTreasury, true);

  // Only the creator signs: the mints derive from its key and the ground seeds.
  const rent = await lpMintRent(connection);
  const seeded = await Promise.all(
    MARKET_LP_MINT_KINDS.map((kind) =>
      createHookedLpMintWithSeedInstructions({
        payer: owner,
        base: owner,
        seed: seeds[kind].seed,
        mint: mints[kind],
        decimals: kind === "quoteHlp" ? quoteDecimals : baseDecimals,
        mintAuthority: market,
        transferHookProgramId: dusk.program.programId,
        lamports: rent,
      })
    )
  );
  const mintSig = await send([
    createAssociatedTokenAccountIdempotentInstruction(
      owner, teamTreasuryWsolAccount, teamTreasury, NATIVE_MINT, TOKEN_PROGRAM_ID
    ),
    ...seeded.flatMap((entry) => entry.instructions),
  ]);
  console.log(`lp mints       ${mintSig}`);

  const initSig = await send([
    await dusk.write.initializeMarketInstruction({
      payer: owner,
      baseMint,
      quoteMint,
      ylpMint: mints.ylp,
      baseHlpMint: mints.baseHlp,
      quoteHlpMint: mints.quoteHlp,
      teamTreasury,
      teamTreasuryWsolAccount,
      paramsHash,
      config: defaultMarketLaunchConfig(),
    }),
  ]);
  console.log(`initialized    ${initSig}`);

  // Token-2022 refuses to move a hooked token until the hook's validation
  // account exists, so no LP token can leave a wallet without this step.
  const hookSig = await send(
    await Promise.all(
      MARKET_LP_MINT_KINDS.map((kind) =>
        dusk.write.initializeLpTransferHookInstruction({ payer: owner, market, lpMint: mints[kind] })
      )
    )
  );
  console.log(`lp hooks       ${hookSig}`);

  const published = await publishLpMetadata({
    connection, network: NETWORK, market, baseMint, quoteMint, baseSymbol, quoteSymbol,
    mints, naming, pinata, outDir,
    logoUrls: { base: process.env.BASE_LOGO, quote: process.env.QUOTE_LOGO },
  });
  const metadataSigs: Record<string, string> = {};
  for (const kind of MARKET_LP_MINT_KINDS) {
    metadataSigs[kind] = await send([
      await dusk.write.initializeLpMetadataInstruction({
        payer: owner,
        market,
        lpMint: mints[kind],
        name: naming[kind].name,
        symbol: naming[kind].symbol,
        uri: published[kind].uri,
      }),
    ]);
    console.log(`${kind.padEnd(14)} ${naming[kind].symbol.padEnd(11)} ${published[kind].uri}`);
  }

  writeFileSync(
    join(outDir, "market.json"),
    JSON.stringify(
      {
        network: NETWORK, label, market: market.toBase58(), creator: owner.toBase58(),
        base: { mint: baseMint.toBase58(), symbol: baseSymbol },
        quote: { mint: quoteMint.toBase58(), symbol: quoteSymbol },
        lp: Object.fromEntries(
          MARKET_LP_MINT_KINDS.map((kind) => [
            kind,
            {
              mint: mints[kind].toBase58(), seed: seeds[kind].seed, ...naming[kind],
              image: published[kind].image, uri: published[kind].uri, metadataSignature: metadataSigs[kind],
            },
          ])
        ),
        signatures: { mints: mintSig, initialize: initSig, transferHooks: hookSig },
      },
      null,
      2
    ) + "\n"
  );

  // Seed before returning, so the API can preview what was just created.
  const ata = (mint: PublicKey) => getAssociatedTokenAddressSync(mint, owner);
  const held = async (mint: PublicKey) => {
    const balance = await connection.getTokenAccountBalance(ata(mint), "confirmed").catch(() => null);
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
        `${haveBase / baseUnit} base and ${haveQuote / quoteUnit} quote, needs ${SEED} of each.`
    );
  }
  const ownerYlp = getAssociatedTokenAddressSync(mints.ylp, owner, false, TOKEN_2022_PROGRAM_ID);
  await send([
    createAssociatedTokenAccountIdempotentInstruction(owner, ownerYlp, owner, mints.ylp, TOKEN_2022_PROGRAM_ID),
  ]);
  const seedSig = await send([
    await dusk.write.addLiquidityInstruction({
      baseMint, market, owner,
      ownerBaseAccount: ata(baseMint),
      ownerQuoteAccount: ata(quoteMint),
      ownerYlpAccount: ownerYlp,
      quoteMint,
      ylpMint: mints.ylp,
      baseDepositAmount: (SEED * baseUnit).toString(),
      quoteDepositAmount: (SEED * quoteUnit).toString(),
      minYlpAmount: "0",
    }),
  ]);
  console.log(`seeded         ${SEED} a side — ${seedSig}`);
  console.log(`\nmarket ${market.toBase58()} is live; record in ${join(outDir, "market.json")}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
