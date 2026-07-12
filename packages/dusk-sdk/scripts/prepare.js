#!/usr/bin/env node
/**
 * Copies IDL and types from anchor build output to src/
 * Run this before building the package
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = resolve(__dirname, "..");
const repoRoot = resolve(rootDir, "../..");

const files = [
  {
    src: resolve(repoRoot, "target/idl/dusk.json"),
    dest: resolve(rootDir, "src/idl_v2.json"),
    prepare: (contents) => contents,
  },
  {
    src: resolve(repoRoot, "target/types/dusk.ts"),
    dest: resolve(rootDir, "src/types_v2.ts"),
    prepare: (contents) => contents,
  },
];

console.log("Preparing @omnipair/dusk-sdk...\n");

for (const { src, dest, prepare } of files) {
  if (!existsSync(src)) {
    console.error(`ERROR: Source file not found: ${src}`);
    console.error("Run 'anchor build' first to generate IDL and types.");
    process.exit(1);
  }

  // Ensure destination directory exists
  mkdirSync(dirname(dest), { recursive: true });

  const contents = prepare(readFileSync(src, "utf8"));
  writeFileSync(dest, contents);
  console.log(`✓ Copied ${src.split("/").pop()} -> src/`);
}

console.log("\nDone! Ready to build.");
