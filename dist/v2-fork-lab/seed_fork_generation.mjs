import { randomBytes } from "node:crypto";
import { pathToFileURL } from "node:url";
import { PublicKey } from "@solana/web3.js";

async function rpc(rpcUrl, method, params) {
  const response = await fetch(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const payload = await response.json();
  if (payload.error)
    throw new Error(`${method} failed: ${JSON.stringify(payload.error)}`);
  return payload.result;
}

export async function seedForkGeneration() {
  const rpcUrl = process.env.SURFPOOL_RPC_URL ?? "http://127.0.0.1:8899";
  const programId = new PublicKey(
    process.env.DUSK_PROGRAM_ID ??
      "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv",
  );
  const [marker] = PublicKey.findProgramAddressSync(
    [Buffer.from("surfpool_fork_generation_v1")],
    programId,
  );

  async function readMarker() {
    const response = await rpc(rpcUrl, "getAccountInfo", [
      marker.toBase58(),
      { commitment: "confirmed", encoding: "base64" },
    ]);
    return response?.value ?? null;
  }

  let account = await readMarker();
  if (!account) {
    await rpc(rpcUrl, "surfnet_setAccount", [
      marker.toBase58(),
      {
        lamports: 1_000_000,
        owner: programId.toBase58(),
        executable: false,
        data: randomBytes(32).toString("hex"),
      },
    ]);
    account = await readMarker();
  }

  if (!account)
    throw new Error("Surfpool fork generation marker was not persisted");
  if (account.owner !== programId.toBase58()) {
    throw new Error(
      `Surfpool fork generation marker has wrong owner ${account.owner}`,
    );
  }
  const data = Buffer.from(account.data[0], account.data[1]);
  if (data.length !== 32) {
    throw new Error(
      `Surfpool fork generation marker has invalid length ${data.length}`,
    );
  }

  console.log(`Surfpool fork generation marker ready: ${marker.toBase58()}`);
  return marker.toBase58();
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  await seedForkGeneration();
}
