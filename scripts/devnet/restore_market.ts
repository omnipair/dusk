/**
 * Return the devnet market to a usable state.
 *
 * Testing liquidation means deliberately leaving debt and open auctions
 * behind, and both make the market worse for everyone else: while any
 * meaningful debt stands, most swaps revert (see
 * `repro_swap_reverts_intermittently.ts`). This repays every position the
 * running wallet owns, which also resolves any auction against them.
 *
 * Positions are read from the chain rather than from the API, because the
 * API's market state is cached and a stale debt figure is exactly the thing
 * that makes a restored market look broken.
 *
 *   node --experimental-strip-types scripts/devnet/restore_market.ts
 */

import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { createHash } from "crypto";
import { readFileSync } from "fs";
import { homedir } from "os";
import { join } from "path";

import { Dusk } from "../../packages/dusk-sdk/dist/index.js";

const API =
  process.env.DUSK_API_URL ?? "https://dusk-api-production-291f.up.railway.app";
const RPC = process.env.DUSK_RPC_URL ?? "https://api.devnet.solana.com";

/** Repayments tried per position, largest first; the first accepted wins. */
const LADDER = [5_000n, 2_000n, 1_000n, 500n, 200n, 100n, 50n, 20n, 10n, 5n, 1n];

function loadKeypair(): Keypair {
  const path =
    process.env.DUSK_KEYPAIR ?? join(homedir(), ".config/solana/id.json");
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))),
  );
}

function accountDiscriminator(name: string): Buffer {
  return createHash("sha256").update(`account:${name}`).digest().subarray(0, 8);
}

function base58(bytes: Buffer): string {
  const alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let value = BigInt(`0x${bytes.toString("hex") || "0"}`);
  let encoded = "";
  while (value > 0n) {
    encoded = alphabet[Number(value % 58n)] + encoded;
    value /= 58n;
  }
  return encoded;
}

async function main() {
  const keypair = loadKeypair();
  const connection = new Connection(RPC, "confirmed");
  const config = (await (await fetch(`${API}/api/dusk/v1/config`)).json()).data;
  const programId = new PublicKey(config.programId);
  const market = new PublicKey(config.primaryMarket);
  const quoteMint = new PublicKey(config.quoteMint);
  const baseMint = new PublicKey(config.baseMint);
  const unit = 10n ** BigInt(config.quoteDecimals);

  const dusk = new Dusk({
    programId,
    provider: new AnchorProvider(connection, new Wallet(keypair), {
      commitment: "confirmed",
    }),
  });

  const accounts = await connection.getProgramAccounts(programId, {
    commitment: "confirmed",
    filters: [
      { memcmp: { bytes: base58(accountDiscriminator("BorrowPosition")), offset: 0 } },
    ],
  });

  const send = async (instructions: TransactionInstruction[]) => {
    const transaction = new Transaction().add(...instructions);
    const blockhash = await connection.getLatestBlockhash("confirmed");
    transaction.recentBlockhash = blockhash.blockhash;
    transaction.feePayer = keypair.publicKey;
    transaction.sign(keypair);
    const signature = await connection.sendRawTransaction(
      transaction.serialize(),
      { preflightCommitment: "confirmed" },
    );
    const result = await connection.confirmTransaction(
      { ...blockhash, signature },
      "confirmed",
    );
    if (result.value.err) throw new Error(JSON.stringify(result.value.err));
    return signature;
  };

  let repaid = 0;
  let indebted = 0;
  for (const { account, pubkey } of accounts) {
    const data = account.data;
    const owner = new PublicKey(data.subarray(8, 40));
    if (!owner.equals(keypair.publicKey)) continue;
    const quoteShares = data.readBigUInt64LE(224);
    const baseShares = data.readBigUInt64LE(208);
    if (quoteShares === 0n && baseShares === 0n) continue;
    indebted += 1;

    const positionId = new PublicKey(data.subarray(72, 104));
    const debtIsBase = baseShares > 0n;
    for (const amount of LADDER) {
      try {
        const signature = await send([
          await dusk.write.repayInstruction({
            debtAssetMint: debtIsBase ? baseMint : quoteMint,
            market,
            owner: keypair.publicKey,
            ownerDebtAccount: getAssociatedTokenAddressSync(
              debtIsBase ? baseMint : quoteMint,
              keypair.publicKey,
            ),
            positionId,
            repayAmount: (amount * unit).toString(),
          }),
        ]);
        console.log(
          `  ${pubkey.toBase58().slice(0, 10)}: repaid ${amount} ${debtIsBase ? "base" : "quote"}  ${signature.slice(0, 12)}`,
        );
        repaid += 1;
        break;
      } catch {
        // More than is outstanding, or the market refused; try less.
      }
    }
  }

  console.log(
    indebted === 0
      ? "no position owned by this wallet carries debt"
      : `${repaid}/${indebted} indebted positions repaid`,
  );
}

main().catch((error) => {
  console.error(`FAILED: ${error instanceof Error ? error.message : error}`);
  process.exit(1);
});
