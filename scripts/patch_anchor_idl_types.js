#!/usr/bin/env node
/**
 * Makes Anchor's IdlTypes decoder lazily recursive.
 *
 * Anchor decodes every IDL type eagerly, up to four passes deep, before it
 * can answer a single lookup. TypeScript gives up after five million type
 * instantiations, and the Dusk IDL crosses that line at roughly 155 types.
 * Past it, Program<Dusk>, IdlAccounts<Dusk>, and IdlEvents<Dusk> all fail
 * with "Type instantiation is excessively deep" for the SDK, the test
 * harness, and every downstream consumer.
 *
 * A self-referential mapped type decodes only the types a caller touches,
 * which keeps the same lookups near ten thousand instantiations. Runs from
 * the root postinstall; safe to re-run.
 */
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";

const require = createRequire(import.meta.url);
const anchorRoot = dirname(require.resolve("@coral-xyz/anchor/package.json"));
const targets = [
  "dist/cjs/program/namespace/types.d.ts",
  "dist/esm/program/namespace/types.d.ts",
  "dist/browser/src/program/namespace/types.d.ts",
]
  .map((file) => resolve(anchorRoot, file))
  .filter((file) => existsSync(file));

const MARKER = "Patched by scripts/patch_anchor_idl_types.js";
const EAGER = /type RecursiveDepth2<[\s\S]*?type RecursiveTypes<[^\n]*\n/;
const LAZY = [
  `// ${MARKER}: decode IDL types lazily so large IDLs`,
  "// stay within TypeScript's instantiation budget.",
  "type RecursiveTypes<T extends IdlTypeDef[]> = {",
  '    [D in T[number] as D["name"]]: TypeDef<D, RecursiveTypes<T>>;',
  "};",
  "",
].join("\n");

if (targets.length === 0) {
  console.error("patch_anchor_idl_types: no Anchor type declarations found");
  process.exit(1);
}

for (const file of targets) {
  const source = readFileSync(file, "utf8");
  if (source.includes(MARKER)) {
    console.log(`patch_anchor_idl_types: already patched ${file}`);
    continue;
  }
  if (!EAGER.test(source)) {
    console.error(`patch_anchor_idl_types: unexpected Anchor type declarations in ${file}`);
    process.exit(1);
  }
  writeFileSync(file, source.replace(EAGER, LAZY));
  console.log(`patch_anchor_idl_types: patched ${file}`);
}
