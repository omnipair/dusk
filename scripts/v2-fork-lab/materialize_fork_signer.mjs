import { chmodSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { Keypair } from "@solana/web3.js";

function decodeSigner(value) {
  const trimmed = value.trim();
  const json = trimmed.startsWith("[")
    ? trimmed
    : Buffer.from(trimmed, "base64").toString("utf8");
  const bytes = JSON.parse(json);
  if (!Array.isArray(bytes)) {
    throw new Error("Surfpool signer must decode to a JSON byte array");
  }
  return Keypair.fromSecretKey(Uint8Array.from(bytes));
}

export function materializeForkSigner(destination) {
  const inline =
    process.env.FORK_LAB_PAYER_KEYPAIR_JSON ??
    process.env.FORK_LAB_PAYER_KEYPAIR_BASE64;
  if (!inline) {
    throw new Error(
      "Set the same FORK_LAB_PAYER_KEYPAIR_JSON or " +
        "FORK_LAB_PAYER_KEYPAIR_BASE64 on the hosted RPC and API services",
    );
  }
  const keypair = decodeSigner(inline);
  const path = resolve(destination);
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  writeFileSync(path, JSON.stringify(Array.from(keypair.secretKey)), {
    mode: 0o600,
  });
  chmodSync(path, 0o600);
  return { path, publicKey: keypair.publicKey.toBase58() };
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const destination = process.argv[2];
  if (!destination) throw new Error("Signer destination path is required");
  const result = materializeForkSigner(destination);
  console.log(`Surfpool signer ready: ${result.publicKey}`);
}
