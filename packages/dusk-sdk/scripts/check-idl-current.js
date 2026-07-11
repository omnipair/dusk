#!/usr/bin/env node
/**
 * Verifies that committed Dusk SDK files match the latest Anchor build
 * output. Run this after `anchor build -p omnipair-v2`.
 */

import { existsSync, readFileSync } from "fs";
import { dirname, relative, resolve } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const packageRoot = resolve(__dirname, "..");
const repoRoot = resolve(packageRoot, "../..");

const pairs = [
  {
    generated: resolve(repoRoot, "target/idl/omnipair_v2.json"),
    committed: resolve(packageRoot, "src/idl_v2.json"),
  },
  {
    generated: resolve(repoRoot, "target/types/omnipair_v2.ts"),
    committed: resolve(packageRoot, "src/types_v2.ts"),
  },
];

let failed = false;

for (const { generated, committed } of pairs) {
  if (!existsSync(generated)) {
    console.error(`Missing build artifact: ${relative(repoRoot, generated)}`);
    failed = true;
    continue;
  }
  if (!existsSync(committed)) {
    console.error(`Missing committed interface file: ${relative(repoRoot, committed)}`);
    failed = true;
    continue;
  }

  const generatedContents = readFileSync(generated, "utf8");
  const committedContents = readFileSync(committed, "utf8");

  if (generatedContents !== committedContents) {
    console.error(
      `Program interface drift: ${relative(repoRoot, committed)} does not match ${relative(
        repoRoot,
        generated
      )}`
    );
    failed = true;
  }
}

if (failed) {
  console.error(
    "\nRun `anchor build -p omnipair-v2` and `npm run prepare-idl --prefix packages/dusk-sdk`, then commit the updated interface files."
  );
  process.exit(1);
}

console.log("Dusk SDK interface files match the latest Anchor build artifacts.");
