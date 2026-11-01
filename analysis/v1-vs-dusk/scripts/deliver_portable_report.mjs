#!/usr/bin/env node

import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const CANONICAL_BUILDER_ENV = "V1_VS_DUSK_CANONICAL_REPORT_BUILDER";
const VALUE_OPTIONS = new Set([
  "input",
  "output",
  "ready-timeout-ms",
  "action-timeout-ms",
  "timeout-ms",
  "screenshot",
]);
const CONTAINMENT_STYLE =
  '<style data-v1-vs-dusk-overflow-containment>' +
  'html,body{overflow-x:hidden!important}' +
  '@media screen{.portable-page-header{' +
  'width:100%!important;margin-left:0!important;margin-right:0!important' +
  '}}</style>';

function usage() {
  return [
    "Usage: node deliver_portable_report.mjs --input <artifact.json> --output <report.html> [options]",
    `Set ${CANONICAL_BUILDER_ENV} to the official deliver_portable_artifact.mjs path.`,
  ].join("\n");
}

function parseArguments(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--help" || argument === "-h") return { help: true };
    if (!argument.startsWith("--")) throw new Error(`Unexpected argument: ${argument}\n${usage()}`);
    const key = argument.slice(2);
    if (!VALUE_OPTIONS.has(key)) throw new Error(`Unknown argument: ${argument}\n${usage()}`);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`Missing value for ${argument}.\n${usage()}`);
    if (options[key] !== undefined) throw new Error(`${argument} may only be specified once.`);
    options[key] = value;
    index += 1;
  }
  if (!options.input || !options.output) throw new Error(`--input and --output are required.\n${usage()}`);
  return options;
}

function positiveNumber(value, label) {
  if (value === undefined) return undefined;
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${label} must be a positive number.`);
  return parsed;
}

function containRootOverflow(html) {
  const marker = "</head>";
  if (!html.includes(marker)) throw new Error("Canonical portable HTML has no closing head element.");
  return html.replace(marker, `${CONTAINMENT_STYLE}${marker}`);
}

async function main(argv = process.argv.slice(2)) {
  const parsed = parseArguments(argv);
  if (parsed.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }

  const configured = process.env[CANONICAL_BUILDER_ENV]?.trim();
  if (!configured) throw new Error(`${CANONICAL_BUILDER_ENV} is required.`);
  const canonicalBuilder = resolve(configured);
  if (!existsSync(canonicalBuilder)) {
    throw new Error(`Canonical portable builder does not exist: ${canonicalBuilder}`);
  }
  const canonicalDirectory = dirname(canonicalBuilder);
  const buildModule = join(canonicalDirectory, "build_portable_artifact.mjs");
  if (!existsSync(buildModule)) {
    throw new Error(`Canonical portable build module does not exist: ${buildModule}`);
  }

  const { deliverPortableArtifact } = await import(pathToFileURL(canonicalBuilder).href);
  const { buildPortableArtifact } = await import(pathToFileURL(buildModule).href);
  const result = await deliverPortableArtifact(
    {
      actionTimeoutMs: positiveNumber(parsed["action-timeout-ms"], "--action-timeout-ms"),
      inputPath: parsed.input,
      outputPath: parsed.output,
      readyTimeoutMs: positiveNumber(parsed["ready-timeout-ms"], "--ready-timeout-ms"),
      screenshotPath: parsed.screenshot,
      timeoutMs: positiveNumber(parsed["timeout-ms"], "--timeout-ms"),
    },
    {
      build: (input, options) => containRootOverflow(buildPortableArtifact(input, options)),
    },
  );
  process.stdout.write(`${JSON.stringify(result)}\n`);
}

try {
  await main();
} catch (error) {
  const result = error?.deliveryResult ?? {
    ok: false,
    stage: "invocation",
    code: error?.code ?? "invalid_invocation",
    error: error?.message ?? String(error),
  };
  process.stderr.write(`${JSON.stringify(result)}\n`);
  process.exitCode = 1;
}
