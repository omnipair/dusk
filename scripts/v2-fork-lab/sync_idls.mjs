import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

const expectedLeverageDelegateProgramId =
  "EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp";
const source = resolve("target/idl/leverage_delegate.json");
const destination = resolve("scripts/v2-fork-lab/idl/leverage_delegate.json");
const duskSource = resolve("target/idl/dusk.json");
const duskPackaged = resolve("packages/dusk-sdk/src/idl_v2.json");
const duskTypesSource = resolve("target/types/dusk.ts");
const duskTypesPackaged = resolve("packages/dusk-sdk/src/types_v2.ts");
const raw = readFileSync(source, "utf8");
const idl = JSON.parse(raw);

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function canonicalSha256(path) {
  return createHash("sha256")
    .update(canonicalJson(JSON.parse(readFileSync(path, "utf8"))))
    .digest("hex");
}

if (idl.address !== expectedLeverageDelegateProgramId) {
  throw new Error(
    `Refusing to vendor leverage_delegate IDL for ${idl.address}; expected ${expectedLeverageDelegateProgramId}`,
  );
}

const sourceDuskDigest = canonicalSha256(duskSource);
const packagedDuskDigest = canonicalSha256(duskPackaged);
if (sourceDuskDigest !== packagedDuskDigest) {
  throw new Error(
    `Dusk source/package canonical IDL mismatch: ${sourceDuskDigest} != ${packagedDuskDigest}`,
  );
}
if (readFileSync(duskTypesSource, "utf8") !== readFileSync(duskTypesPackaged, "utf8")) {
  throw new Error(
    "Dusk generated/package TypeScript IDL mismatch; rebuild Dusk and update packages/dusk-sdk/src/types_v2.ts",
  );
}

if (process.argv.includes("--check")) {
  const vendored = readFileSync(destination, "utf8");
  if (vendored !== raw) {
    throw new Error(
      "Vendored leverage_delegate IDL is stale; run npm run v2-fork:sync-idls after anchor build",
    );
  }
  console.log("Dusk packaged IDL/types and leverage-delegate vendored IDL are current");
} else {
  mkdirSync(dirname(destination), { recursive: true });
  writeFileSync(destination, raw);
  console.log(`Synced ${source} -> ${destination}`);
}
