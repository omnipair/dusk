import { BorshCoder, type Idl, type IdlTypes } from "@coral-xyz/anchor";

import IDL from "./idl_v2.js";
import type { OmnipairV2 } from "./types_v2.js";

const coder = new BorshCoder(IDL as unknown as Idl);

type DuskPreviewTypes = IdlTypes<OmnipairV2>;

export type MarketPreview = DuskPreviewTypes["marketPreview"];
export type AddLiquidityPreview = DuskPreviewTypes["addLiquidityPreview"];
export type SwapPreview = DuskPreviewTypes["swapPreview"];
export type BorrowCapacityPreview = DuskPreviewTypes["borrowCapacityPreview"];
export type BorrowPositionPreview = DuskPreviewTypes["borrowPositionPreview"];

export const PREVIEW_RETURN_TYPES = {
  previewMarket: "MarketPreview",
  previewAddLiquidity: "AddLiquidityPreview",
  previewSwap: "SwapPreview",
  previewBorrowCapacity: "BorrowCapacityPreview",
  previewBorrowPosition: "BorrowPositionPreview",
} as const;

export type PreviewInstructionName = keyof typeof PREVIEW_RETURN_TYPES;
export type PreviewReturnTypeName = (typeof PREVIEW_RETURN_TYPES)[PreviewInstructionName];

type PreviewReturnByIdlType = {
  MarketPreview: MarketPreview;
  AddLiquidityPreview: AddLiquidityPreview;
  SwapPreview: SwapPreview;
  BorrowCapacityPreview: BorrowCapacityPreview;
  BorrowPositionPreview: BorrowPositionPreview;
};

export type PreviewReturnForType<T extends PreviewReturnTypeName> = PreviewReturnByIdlType[T];
export type PreviewReturnForInstruction<T extends PreviewInstructionName> =
  PreviewReturnByIdlType[(typeof PREVIEW_RETURN_TYPES)[T]];

export type PreviewReturnData =
  | Buffer
  | Uint8Array
  | string
  | readonly [string, BufferEncoding]
  | {
      data:
        | Buffer
        | Uint8Array
        | string
        | readonly [string, BufferEncoding];
    };

export function decodePreviewReturnData<T extends PreviewReturnTypeName>(
  typeName: T,
  returnData: PreviewReturnData
): PreviewReturnForType<T> {
  const decoded = coder.types.decode(typeName, previewReturnDataBytes(returnData));
  return camelizeKeys(decoded) as PreviewReturnForType<T>;
}

export function decodePreviewInstructionReturnData<T extends PreviewInstructionName>(
  instructionName: T,
  returnData: PreviewReturnData
): PreviewReturnForInstruction<T> {
  return decodePreviewReturnData(
    PREVIEW_RETURN_TYPES[instructionName],
    returnData
  ) as PreviewReturnForInstruction<T>;
}

export function decodePreviewMarketReturnData(returnData: PreviewReturnData): MarketPreview {
  return decodePreviewReturnData("MarketPreview", returnData);
}

export function decodePreviewAddLiquidityReturnData(
  returnData: PreviewReturnData
): AddLiquidityPreview {
  return decodePreviewReturnData("AddLiquidityPreview", returnData);
}

export function decodePreviewSwapReturnData(returnData: PreviewReturnData): SwapPreview {
  return decodePreviewReturnData("SwapPreview", returnData);
}

export function decodePreviewBorrowCapacityReturnData(
  returnData: PreviewReturnData
): BorrowCapacityPreview {
  return decodePreviewReturnData("BorrowCapacityPreview", returnData);
}

export function decodePreviewBorrowPositionReturnData(
  returnData: PreviewReturnData
): BorrowPositionPreview {
  return decodePreviewReturnData("BorrowPositionPreview", returnData);
}

export function previewReturnDataBytes(returnData: PreviewReturnData): Buffer {
  if (returnData instanceof Uint8Array) {
    return Buffer.from(returnData);
  }
  if (typeof returnData === "string") {
    return Buffer.from(returnData, "base64");
  }
  if (isEncodedReturnTuple(returnData)) {
    return Buffer.from(returnData[0], returnData[1]);
  }
  return previewReturnDataBytes(returnData.data);
}

function isEncodedReturnTuple(
  returnData: PreviewReturnData
): returnData is readonly [string, BufferEncoding] {
  return Array.isArray(returnData);
}

function camelizeKeys(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(camelizeKeys);
  }
  if (!isPlainRecord(value)) {
    return value;
  }

  return Object.fromEntries(
    Object.entries(value).map(([key, child]) => [camelizeKey(key), camelizeKeys(child)])
  );
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  if (value === null || typeof value !== "object") {
    return false;
  }
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function camelizeKey(key: string): string {
  const snakeToCamel = key.replace(/_([a-z0-9])/g, (_, char: string) => char.toUpperCase());
  return snakeToCamel.charAt(0).toLowerCase() + snakeToCamel.slice(1);
}
