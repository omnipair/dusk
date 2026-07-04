import { PublicKey } from "@solana/web3.js";

export type AddressLike = PublicKey | string;

export const DEFAULT_READONLY_PUBLIC_KEY = new PublicKey(
  "8tF4uYMBXqGhCUGRZL3AmPqRzbX8JJ1TpYnY3uJKN4kt"
);

export function address(value: AddressLike): PublicKey {
  return value instanceof PublicKey ? value : new PublicKey(value);
}

export function addressString(value: AddressLike): string {
  return address(value).toBase58();
}

export function normalizeAccountKeys<T>(value: T): T {
  if (typeof value === "string") {
    try {
      return new PublicKey(value) as T;
    } catch {
      return value;
    }
  }
  if (value instanceof PublicKey || value === null || typeof value !== "object") {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((child) => normalizeAccountKeys(child)) as T;
  }

  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>).map(([key, child]) => [
      key,
      normalizeAccountKeys(child),
    ])
  ) as T;
}
