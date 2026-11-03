import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { ComputeBudget, LiteSVM } from "litesvm";
import { Keypair, PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const binary = path.join(here, "target/deploy/opamm_sbf_probe.so");
if (!fs.existsSync(binary)) throw new Error(`Build first: ${binary}`);
const binarySha256 = createHash("sha256").update(fs.readFileSync(binary)).digest("hex");
const programId = new PublicKey(Buffer.alloc(32, 7));
const budget = new ComputeBudget();
budget.computeUnitLimit = 1_400_000n;
const svm = new LiteSVM().withComputeBudget(budget).withSigverify(false);
svm.addProgramFromFile(programId, binary);
const payer = Keypair.generate();
svm.airdrop(payer.publicKey, 1_000_000_000n);

function dataFor(op, pref, grossInput, base, quote, feeBps) {
  const data = Buffer.alloc(43);
  data[0] = op;
  data.writeBigUInt64LE(100_000_000_000n, 1); // P0 = $100, 1e9 price scale
  data.writeBigUInt64LE(BigInt(pref), 9);
  data.writeBigUInt64LE(base, 17);
  data.writeBigUInt64LE(quote, 25);
  data.writeBigUInt64LE(BigInt(grossInput), 33);
  data.writeUInt16LE(feeBps, 41);
  return data;
}

function run(op, pref, input, base = 5_000_000_000n, quote = 500_000_000_000n, feeBps = 3) {
  const tx = new Transaction().add(new TransactionInstruction({
    programId, keys: [], data: dataFor(op, pref, input, base, quote, feeBps),
  }));
  tx.recentBlockhash = svm.latestBlockhash();
  tx.feePayer = payer.publicKey;
  tx.sign(payer);
  const result = svm.simulateTransaction(tx);
  if (typeof result.err === "function") {
    const meta = result.meta?.();
    throw new Error(`op=${op}, pref=${pref}, input=${input}: ${result.err()}\n${meta?.prettyLogs?.() ?? ""}`);
  }
  const meta = result.meta();
  const returned = meta.returnData();
  if (!returned) throw new Error("Missing return data");
  return {
    op, pref_nad: String(pref), input_atoms: String(input),
    base_atoms: String(base), quote_atoms: String(quote), fee_bps: feeBps,
    output_atoms: Buffer.from(returned.data()).readBigUInt64LE().toString(),
    cu: Number(meta.computeUnitsConsumed()),
  };
}

const stress = process.argv.includes("--stress");
const rows = [];
if (stress) {
  for (const ratio of [0.5, 0.8, 1, 1.2, 2]) {
    for (const price of [50, 75, 95, 100, 105, 150, 200]) {
      for (const usd of [0.000001, 0.01, 1, 100, 10_000, 50_000]) {
        for (const fee of [0, 3, 30, 100]) {
          for (const op of [0, 1, 2, 3]) {
            const input = BigInt(Math.round(op < 2 ? usd * 1_000_000 : usd * 10_000));
            if (input > 0n) rows.push(run(op, BigInt(price) * 1_000_000_000n, input,
              5_000_000_000n, BigInt(Math.round(500_000_000_000 * ratio)), fee));
          }
        }
      }
    }
  }
} else {
  for (const pref of [96_000_000_000n, 100_000_000_000n, 100_125_000_000n, 104_000_000_000n, 110_000_000_000n]) {
    for (const usd of [1, 100, 1_000, 10_000, 50_000]) {
      for (const op of [0, 1, 2, 3]) {
        const input = BigInt(op < 2 ? usd * 1_000_000 : usd * 10_000);
        rows.push(run(op, pref, input));
      }
    }
  }
}
const out = path.join(here, stress ? "results-sbf-stress.json" : "results-sbf.json");
fs.writeFileSync(out, JSON.stringify({ binary: "target/deploy/opamm_sbf_probe.so", binary_sha256: binarySha256, rows }, null, 2) + "\n");
const group = (op) => rows.filter((r) => r.op === op).map((r) => r.cu).sort((a, b) => a - b);
for (const op of [0, 1, 2, 3]) {
  const g = group(op);
  console.log(`op ${op}: CU min=${g[0]} median=${g[Math.floor(g.length / 2)]} max=${g.at(-1)}`);
}
console.log(`Wrote ${out}`);
