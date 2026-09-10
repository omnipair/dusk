/**
 * Account-layout proof for the SDK's three liquidation builders.
 *
 * These are the last write paths the SDK did not reach, and the webapp's
 * liquidate button now depends on them. A wrong account order in a builder
 * is invisible to the type checker and to any test that does not talk to the
 * cluster: Anchor happily serializes an instruction the program then rejects.
 *
 * So each builder is assembled against the live market and a real borrow
 * position and simulated. The test is not "did it succeed" — a healthy
 * position must not be liquidatable, and a fill against no auction must
 * fail. The test is *how* it fails. A program error code means the runtime
 * resolved every account, loaded them, and reached the handler's own
 * checks; an `AccountNotFound`, `ConstraintSeeds`, `InvalidVault` or
 * `AccountNotEnoughKeys` means a builder pointed somewhere wrong.
 *
 *   node --experimental-strip-types scripts/devnet/liquidation_builders_proof.ts
 */

import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, Transaction } from "@solana/web3.js";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";

import { Dusk } from "../../packages/dusk-sdk/dist/index.js";

const API =
  process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";

/**
 * Errors that mean a builder is wrong rather than that the program declined.
 *
 * Each is raised before the handler runs: the first four by Anchor's account
 * resolution, the last by a vault key that does not match the market's own.
 */
const LAYOUT_FAILURES = [
  "AccountNotFound",
  "AccountNotEnoughKeys",
  "AccountOwnedByWrongProgram",
  "ConstraintSeeds",
  "ConstraintAddress",
  "ConstraintTokenMint",
  "InvalidVault",
  "InvalidMint",
  "AccountNotInitialized",
  "InstructionFallbackNotFound",
];

interface DeploymentConfig {
  baseMint: string;
  primaryMarket: string;
  programId: string;
  quoteMint: string;
}

function loadKeypair(): Keypair {
  const path =
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json");
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))),
  );
}

async function deploymentConfig(): Promise<DeploymentConfig> {
  const response = await fetch(`${API}/api/dusk/v1/config`);
  if (!response.ok) throw new Error(`config: HTTP ${response.status}`);
  return ((await response.json()) as { data: DeploymentConfig }).data;
}

let failures = 0;

async function probe(
  connection: Connection,
  payer: PublicKey,
  name: string,
  build: () => Promise<Transaction>,
) {
  let transaction: Transaction;
  try {
    transaction = await build();
  } catch (error) {
    failures += 1;
    console.log(`  ✗ ${name}: builder threw — ${(error as Error).message}`);
    return;
  }
  transaction.feePayer = payer;
  transaction.recentBlockhash = (
    await connection.getLatestBlockhash("confirmed")
  ).blockhash;

  const simulation = await connection.simulateTransaction(transaction);
  const logs = (simulation.value.logs ?? []).join("\n");
  if (!simulation.value.err) {
    console.log(`  ✓ ${name}: accepted`);
    return;
  }
  const layoutFailure = LAYOUT_FAILURES.find((code) => logs.includes(code));
  if (layoutFailure) {
    failures += 1;
    console.log(`  ✗ ${name}: ${layoutFailure} — the account list is wrong`);
    console.log(
      logs
        .split("\n")
        .filter((line) => /Error|error|left|right/.test(line))
        .slice(0, 6)
        .map((line) => `      ${line}`)
        .join("\n"),
    );
    return;
  }
  const programError =
    logs.match(/Error Code: (\w+)/)?.[1] ??
    logs.match(/custom program error: (0x[0-9a-f]+)/)?.[1] ??
    JSON.stringify(simulation.value.err);
  console.log(`  ✓ ${name}: reached the handler — ${programError}`);
}

async function main() {
  const keypair = loadKeypair();
  const connection = new Connection(RPC, "confirmed");
  const config = await deploymentConfig();
  const provider = new AnchorProvider(connection, new Wallet(keypair), {
    commitment: "confirmed",
  });
  const dusk = new Dusk({
    programId: new PublicKey(config.programId),
    provider,
  });

  const market = new PublicKey(config.primaryMarket);
  const baseMint = new PublicKey(config.baseMint);
  const quoteMint = new PublicKey(config.quoteMint);

  // A real position, because a fabricated PDA fails at account resolution
  // for a reason that has nothing to do with the builder.
  const positions = await dusk.program.account.borrowPosition.all();
  const onMarket = positions.filter(
    (entry) =>
      (entry.account as { market: PublicKey }).market.toBase58() ===
      market.toBase58(),
  );
  if (onMarket.length === 0) {
    throw new Error(
      "no borrow position on the primary market; run live_write_proof.ts first",
    );
  }
  const position = onMarket[0];
  const account = position.account as {
    owner: PublicKey;
    positionId: PublicKey;
  };
  console.log(`market   ${market.toBase58()}`);
  console.log(`position ${position.publicKey.toBase58()}`);
  console.log(`owner    ${account.owner.toBase58()}\n`);

  // Which side carries debt decides which mint is the debt mint; a position
  // with no debt on the probed side fails for the wrong reason.
  const preview = (await dusk.get.previewBorrowPosition({
    market: market.toBase58(),
    borrowPosition: position.publicKey.toBase58(),
  })) as unknown as {
    baseDebt: { fixedDebt: { toString(): string } };
    quoteDebt: { fixedDebt: { toString(): string } };
  };
  const debtIsBase =
    BigInt(preview.baseDebt.fixedDebt.toString()) >
    BigInt(preview.quoteDebt.fixedDebt.toString());
  const debtAssetMint = debtIsBase ? baseMint : quoteMint;
  const collateralAssetMint = debtIsBase ? quoteMint : baseMint;
  console.log(`debt side ${debtIsBase ? "base" : "quote"}\n`);

  const shared = {
    market: market.toBase58(),
    positionId: account.positionId.toBase58(),
    positionOwner: account.owner.toBase58(),
    debtAssetMint: debtAssetMint.toBase58(),
    collateralAssetMint: collateralAssetMint.toBase58(),
  };

  await probe(connection, keypair.publicKey, "startLiquidationAuction", () =>
    dusk.write.startLiquidationAuctionTransaction({
      market: shared.market,
      positionId: shared.positionId,
      debtAssetMint: shared.debtAssetMint,
    }),
  );

  await probe(connection, keypair.publicKey, "fillLiquidationAuction", () =>
    dusk.write.fillLiquidationAuctionTransaction({
      ...shared,
      liquidator: keypair.publicKey.toBase58(),
      repayAmount: 1n,
      minCollateralOut: 0n,
    }),
  );

  await probe(connection, keypair.publicKey, "backstopLiquidationAuction", () =>
    dusk.write.backstopLiquidationAuctionTransaction({
      ...shared,
      liquidator: keypair.publicKey.toBase58(),
      minCallerBountyOut: 0n,
    }),
  );

  console.log(
    failures === 0
      ? "\nall three builders resolved every account"
      : `\n${failures} builder(s) point at the wrong accounts`,
  );
  if (failures > 0) process.exit(1);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
