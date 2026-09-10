/**
 * SHA-256 that works in a browser and in Node.
 *
 * WebCrypto is the only digest a browser bundle can rely on and it is async,
 * so every caller of this is async too. `node:crypto` is imported lazily so a
 * bundler targeting the browser never has to resolve it.
 */
export async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  if (globalThis.crypto?.subtle) {
    const stableBytes = Uint8Array.from(bytes);
    return new Uint8Array(await globalThis.crypto.subtle.digest("SHA-256", stableBytes));
  }
  const { createHash } = await import("node:crypto");
  return Uint8Array.from(createHash("sha256").update(bytes).digest());
}
