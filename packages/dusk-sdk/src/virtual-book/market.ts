import { DEFAULT_READONLY_PUBLIC_KEY } from "../address.js";
import type { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { address as publicKey, type AddressLike } from "../address.js";
import { deriveMarketAddress } from "../constants.js";
import { decodePreviewMarketReturnData } from "../preview.js";
import type { Market } from "../type-aliases.js";
import type { Dusk } from "../types_v2.js";
import { simulatePreviewWithContext, type PreviewSimulationOptions } from "../simulation.js";

/** Accrued market and preview observed together in one simulation bank. */
export async function previewVirtualBookSnapshot(
  program: Program<Dusk>,
  marketAddress: AddressLike,
  options: PreviewSimulationOptions = {}
) {
  options.signal?.throwIfAborted();
  const market = publicKey(marketAddress).toBase58();
  const floor = options.minContextSlot ?? 0;
  const instruction = await program.methods
    .previewMarket()
    .accountsStrict({ market: publicKey(market) })
    .instruction();
  const result = await simulatePreviewWithContext(program, [instruction], {
    ...options,
    feePayer: options.feePayer ?? DEFAULT_READONLY_PUBLIC_KEY,
    commitment: "confirmed",
    accounts: [market],
  });
  const slot = result.context.slot;
  const data = result.value.returnData;
  const info = result.value.accounts?.[0];
  if (
    !Number.isSafeInteger(slot) ||
    slot < floor ||
    result.value.err ||
    !data ||
    data.programId !== program.programId.toBase58() ||
    data.data[1] !== "base64" ||
    result.value.accounts?.length !== 1 ||
    !info ||
    info.owner !== program.programId.toBase58() ||
    info.executable ||
    info.data[1] !== "base64"
  )
    throw new Error("Market depth snapshot unavailable");
  const accountBytes = Buffer.from(info.data[0], "base64");
  if (accountBytes.toString("base64") !== info.data[0])
    throw new Error("Invalid depth account encoding");
  const preview = decodePreviewMarketReturnData(data.data);
  const account = program.coder.accounts.decode<Market>("market", accountBytes);
  const [address, bump] = deriveMarketAddress(
    account.baseSide.assetMint,
    account.quoteSide.assetMint,
    account.paramsHash
  );
  if (
    preview.slot.toString() !== String(slot) ||
    account.version !== 1 ||
    !address.equals(new PublicKey(market)) ||
    account.bump !== bump ||
    !preview.amm.initialized ||
    account.amm.concentratedCurveCache.mathRevision !== 1
  )
    throw new Error("Incompatible depth snapshot");
  return {
    market,
    programId: program.programId.toBase58(),
    slot,
    observedAt: result.observedAt,
    account,
    preview,
  };
}
export type DuskVirtualBookSnapshot = Awaited<ReturnType<typeof previewVirtualBookSnapshot>>;
