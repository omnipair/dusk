#!/usr/bin/env node
/**
 * Verifies that committed Dusk SDK files match the latest Anchor build
 * output. Run this after `anchor build -p dusk`.
 */

import { existsSync, readFileSync } from "fs";
import { dirname, relative, resolve } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const packageRoot = resolve(__dirname, "..");
const repoRoot = resolve(packageRoot, "../..");

const pairs = [
  {
    generated: resolve(repoRoot, "target/idl/dusk.json"),
    committed: resolve(packageRoot, "src/idl_v2.json"),
    normalize: (contents) => contents,
  },
  {
    generated: resolve(repoRoot, "target/types/dusk.ts"),
    committed: resolve(packageRoot, "src/types_v2.ts"),
    normalize: (contents) => contents,
  },
  {
    generated: resolve(repoRoot, "target/idl/leverage_delegate.json"),
    committed: resolve(packageRoot, "src/idl_delegate.json"),
    normalize: (contents) => contents,
  },
  {
    generated: resolve(repoRoot, "target/types/leverage_delegate.ts"),
    committed: resolve(packageRoot, "src/types_delegate.ts"),
    normalize: (contents) => contents,
  },
];

let failed = false;

for (const { generated, committed, normalize } of pairs) {
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

  const generatedContents = normalize(readFileSync(generated, "utf8"));
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

// Anchor's full type dictionary exceeds TypeScript's recursion limit. The SDK
// decodes only definitions reachable from its account, event, and preview
// roots; keep those three lists in sync whenever an IDL dependency changes.
const idl = JSON.parse(readFileSync(resolve(packageRoot, "src/idl_v2.json"), "utf8"));
const aliases = readFileSync(resolve(packageRoot, "src/type-aliases.ts"), "utf8");
const previews = readFileSync(resolve(packageRoot, "src/preview.ts"), "utf8");
const typeByName = new Map(idl.types.map((type) => [type.name, type]));
const camel = (name) => name[0].toLowerCase() + name.slice(1);

function collectDefined(value, names) {
  if (Array.isArray(value)) {
    for (const item of value) collectDefined(item, names);
  } else if (value && typeof value === "object") {
    if (value.defined?.name) names.add(value.defined.name);
    for (const item of Object.values(value)) collectDefined(item, names);
  }
}

function reachableTypes(roots) {
  const names = new Set(roots);
  const pending = [...names];
  while (pending.length) {
    const name = pending.pop();
    const definition = typeByName.get(name);
    if (!definition) {
      console.error(`Missing IDL type definition: ${name}`);
      failed = true;
      continue;
    }
    const dependencies = new Set();
    collectDefined(definition.type, dependencies);
    for (const dependency of dependencies) {
      if (!names.has(dependency)) {
        names.add(dependency);
        pending.push(dependency);
      }
    }
  }
  return [...names].map(camel).sort();
}

function listedTypes(source, alias, marker = "TypeNamed") {
  const block = alias === "DuskPreviewIdl"
    ? source.match(/type DuskPreviewIdl = \{[\s\S]*?types: \[([\s\S]*?)\];/)?.[1]
    : source.match(new RegExp(`type ${alias} = \\[([\\s\\S]*?)\\];`))?.[1];
  if (!block) return [];
  return [...block.matchAll(new RegExp(`${marker}<"([^"]+)">`, "g"))].map((match) => match[1]).sort();
}

for (const [label, expected, actual] of [
  ["accounts", reachableTypes(idl.accounts.map((account) => account.name)), listedTypes(aliases, "DuskAccountTypes")],
  ["events", reachableTypes(idl.events.map((event) => event.name)), listedTypes(aliases, "DuskEventTypes")],
  [
    "previews",
    reachableTypes([...previews.matchAll(/export type \w+ = DuskPreviewTypes\["(\w+)"\]/g)].map((match) =>
      match[1][0].toUpperCase() + match[1].slice(1)
    )),
    listedTypes(previews, "DuskPreviewIdl", "PreviewTypeNamed"),
  ],
]) {
  if (expected.join() !== actual.join()) {
    console.error(`SDK ${label} type closure is stale. Expected: ${expected.join(", ")}; found: ${actual.join(", ")}`);
    failed = true;
  }
}

if (failed) {
  console.error(
    "\nRun `anchor build -p dusk`, `anchor build -p leverage_delegate`, and " +
      "`npm run prepare-idl --prefix packages/dusk-sdk`, then commit the updated interface files."
  );
  process.exit(1);
}

console.log("Dusk SDK interface files match the latest Anchor build artifacts.");
