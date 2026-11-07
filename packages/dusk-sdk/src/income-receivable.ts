import type { IntegerLike } from "./parameters.js";
import type { Dusk } from "./dusk.js";
import { address, type AddressLike } from "./address.js";
import { deriveIncomeEpochAddress } from "./constants.js";

const U64 = (1n << 64n) - 1n;
const U128 = (1n << 128n) - 1n;
const U256 = (1n << 256n) - 1n;
const value = (input: IntegerLike, max = U128): bigint => {
  if (typeof input === "number" && !Number.isSafeInteger(input)) throw new Error("Income value lost integer precision");
  const text = String(input);
  if (!/^(0|[1-9]\d*)$/.test(text)) throw new Error("Invalid income value");
  const n = BigInt(text);
  if (n > max) throw new Error("Income value overflow");
  return n;
};
export interface IncomeGrowthInput { unitsQ64: IntegerLike; weightedPaidQ128: readonly IntegerLike[] }
export interface IncomeClaimInput {
  epoch: IntegerLike; unitsQ64: IntegerLike; paidCheckpointQ64: IntegerLike;
  growthCheckpoint: IncomeGrowthInput; paidFractionQ128: IntegerLike; claimable: IntegerLike;
}
export interface IncomeBookInput {
  epoch: IntegerLike; unpaid: IntegerLike; units: IntegerLike;
  paidPerUnitQ64: IntegerLike; growth: readonly IncomeGrowthInput[];
}
export interface IncomeEndInput { ended: boolean; epoch: IntegerLike; growth: IncomeGrowthInput }
export interface IncomeHolderInput {
  tokenKind: number; incomeInitialized: boolean; escrowBalance: IntegerLike;
  income: readonly IncomeClaimInput[];
}
export interface IncomeClaimProjection {
  epoch: bigint; unitsQ64: bigint; paidCheckpointQ64: bigint;
  growthCheckpoint: IncomeGrowthInput; paidFractionQ128: bigint; claimable: bigint;
}
export type IncomeArchiveMap = ReadonlyMap<string, IncomeBookInput>;

function weighted(growth: IncomeGrowthInput): bigint {
  if (growth.weightedPaidQ128.length !== 4) throw new Error("Invalid weighted income index");
  return growth.weightedPaidQ128.reduce<bigint>((sum, limb, i) => sum + (value(limb, U64) << BigInt(i * 64)), 0n);
}

/** Pure copy of IncomeReceivableClaim::checkpoint_exposure. Fractional cash
 * stays at Q128 precision; newly issued units cannot earn prior payments. */
export function projectIncomeClaim(input: {
  claim: IncomeClaimInput; book: IncomeBookInput; balance: IntegerLike; lane: number; end?: IncomeEndInput;
}): IncomeClaimProjection {
  const { claim, book, end } = input;
  if (end && typeof end.ended !== "boolean") throw new Error("Invalid income exposure end");
  const epoch = value(book.epoch, U64);
  if (value(claim.epoch, U64) !== epoch) throw new Error("IncomeHistoryRequired");
  if (!Number.isInteger(input.lane) || input.lane < 0 || input.lane > 3 || book.growth.length !== 4) throw new Error("Invalid income lane");
  const ended = end?.ended === true;
  const endEpoch = ended ? value(end!.epoch, U64) : 0n;
  const growth = ended && epoch === endEpoch ? end!.growth : book.growth[input.lane]!;
  const balance = ended && epoch > endEpoch ? 0n : value(input.balance, U64);
  const delta = value(growth.unitsQ64) - value(claim.growthCheckpoint.unitsQ64);
  const weightedDelta = weighted(growth) - weighted(claim.growthCheckpoint);
  const paid = value(book.paidPerUnitQ64);
  const paidDelta = paid - value(claim.paidCheckpointQ64);
  if (delta < 0n || weightedDelta < 0n || paidDelta < 0n) throw new Error("Income index regressed");
  const newCash = delta * paid - weightedDelta;
  if (newCash < 0n) throw new Error("Income weighted index exceeds payment");
  const scaled = newCash * balance + value(claim.unitsQ64) * paidDelta + value(claim.paidFractionQ128);
  if (scaled > U256) throw new Error("Income value overflow");
  const unitsQ64 = value(value(claim.unitsQ64) + balance * delta);
  return { epoch, unitsQ64, paidCheckpointQ64: paid, growthCheckpoint: growth,
    paidFractionQ128: scaled & U128, claimable: value(value(claim.claimable, U64) + (scaled >> 128n), U64) };
}

/** Archived denominations required by the selected asset's two income books. */
export function incomeArchiveRequests(holder: IncomeHolderInput, books: readonly IncomeBookInput[], asset: 0 | 1, maximumEpochs = 32) {
  if (asset !== 0 && asset !== 1) throw new Error("Invalid income asset");
  if (!Number.isInteger(maximumEpochs) || maximumEpochs < 1 || maximumEpochs > 64) throw new Error("Invalid income history bound");
  if (!holder.incomeInitialized || holder.tokenKind === 1) return [];
  if (![0, 2].includes(holder.tokenKind) || holder.income.length !== 2 || books.length !== 4) throw new Error("Invalid income holder");
  const requests: { bookIndex: number; epoch: bigint }[] = [];
  for (let source = 0; source < 2; source++) {
    const bookIndex = asset + source * 2;
    const first = value(holder.income[source]!.epoch, U64), current = value(books[bookIndex]!.epoch, U64);
    if (first > current || current - first > BigInt(maximumEpochs)) throw new Error("IncomeHistoryRequired");
    for (let epoch = first; epoch < current; epoch++) requests.push({ bookIndex, epoch });
  }
  return requests;
}

/** Additional funded interest Harvest materializes before ordinary growth.
 * Historical units remain with their owner after transfers, exits and wipes. */
export function projectHolderIncome(input: {
  holder: IncomeHolderInput; books: readonly IncomeBookInput[]; asset: 0 | 1;
  walletBalance: IntegerLike; ends?: readonly IncomeEndInput[]; archives?: IncomeArchiveMap;
}) {
  const { holder, books, asset } = input;
  if (input.ends && input.ends.length !== 4) throw new Error("Invalid income exposure ends");
  incomeArchiveRequests(holder, books, asset);
  if (holder.tokenKind === 1) return 0n;
  if (![0, 2].includes(holder.tokenKind) || holder.income.length !== 2 || books.length !== 4) throw new Error("Invalid income holder");
  const lane = holder.tokenKind === 2 ? 1 : 0;
  const balance = holder.incomeInitialized ? value(value(input.walletBalance, U64) + value(holder.escrowBalance, U64), U64) : 0n;
  let amount = 0n;
  for (let source = 0; source < 2; source++) {
    const bookIndex = asset + source * 2, book = books[bookIndex]!;
    let claim: IncomeClaimInput = { ...holder.income[source]!, epoch: holder.incomeInitialized ? holder.income[source]!.epoch : book.epoch };
    const end = lane === 1 ? input.ends?.[bookIndex] : undefined;
    while (value(claim.epoch, U64) < value(book.epoch, U64)) {
      const archived = input.archives?.get(`${bookIndex}:${claim.epoch}`);
      if (!archived) throw new Error("IncomeHistoryRequired");
      const projected = projectIncomeClaim({ claim, book: archived, balance, lane, end });
      const units = value(archived.units, U64), unpaid = value(archived.unpaid, U64);
      if (units === 0n && unpaid !== 0n) throw new Error("Invalid income book");
      claim = { ...projected, epoch: projected.epoch + 1n, unitsQ64: units === 0n ? 0n : value(projected.unitsQ64 * unpaid / units),
        paidCheckpointQ64: 0n, growthCheckpoint: { unitsQ64: 0n, weightedPaidQ128: [0n, 0n, 0n, 0n] } };
    }
    const projected = projectIncomeClaim({ claim, book, balance, lane, end });
    amount = value(amount + projected.claimable, U64);
  }
  return amount;
}

/** Read only immutable, PDA-authenticated archives. Callers pass the bank
 * floor that supplied the holder and current books; no observation is cached. */
export async function readIncomeArchives(input: {
  dusk: Dusk; market: AddressLike; holder: IncomeHolderInput; books: readonly IncomeBookInput[];
  asset: 0 | 1; minContextSlot: number;
}): Promise<IncomeArchiveMap> {
  if (!Number.isSafeInteger(input.minContextSlot) || input.minContextSlot < 0) throw new Error("Invalid income archive bank floor");
  const { dusk } = input, program = dusk.program.programId, market = address(input.market);
  const requests = incomeArchiveRequests(input.holder, input.books, input.asset);
  const result = new Map<string, IncomeBookInput>();
  for (let offset = 0; offset < requests.length; offset += 100) {
    const batch = requests.slice(offset, offset + 100).map(request => ({ ...request,
      derived: deriveIncomeEpochAddress(market, request.bookIndex, request.epoch, program) }));
    const read = await dusk.program.provider.connection.getMultipleAccountsInfoAndContext(batch.map(row => row.derived[0]),
      { commitment: "confirmed", minContextSlot: input.minContextSlot });
    if (read.context.slot < input.minContextSlot || read.value.length !== batch.length) throw new Error("Income archives are stale");
    batch.forEach((request, index) => {
      const account = read.value[index];
      if (!account || account.executable || !account.owner.equals(program)) throw new Error("IncomeHistoryRequired");
      const decoded = dusk.program.coder.accounts.decode("incomeEpoch", account.data);
      if (!decoded.market.equals(market) || decoded.bookIndex !== request.bookIndex || decoded.bump !== request.derived[1] ||
        String(decoded.book.epoch) !== request.epoch.toString()) throw new Error("Invalid income archive");
      result.set(`${request.bookIndex}:${request.epoch}`, decoded.book);
    });
  }
  return result;
}
