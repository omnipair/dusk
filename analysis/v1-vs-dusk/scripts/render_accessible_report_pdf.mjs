#!/usr/bin/env node

import { createRequire } from "node:module";
import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [inputArgument, outputArgument] = process.argv.slice(2);
if (!inputArgument || !outputArgument) {
  throw new Error("usage: render_accessible_report_pdf.mjs <input.html> <output.pdf>");
}
const moduleDirectory = process.env.CODEX_NODE_MODULES;
if (!moduleDirectory) throw new Error("CODEX_NODE_MODULES is required");
const require = createRequire(`${resolve(moduleDirectory)}/`);
const { chromium } = require("playwright");
const executablePath = process.env.CODEX_CHROMIUM_EXECUTABLE;
const input = resolve(inputArgument);
const output = resolve(outputArgument);
await mkdir(dirname(output), { recursive: true });
const browser = await chromium.launch({
  headless: true,
  ...(executablePath ? { executablePath } : {}),
});
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  await page.goto(pathToFileURL(input).href, { waitUntil: "load" });
  await page.emulateMedia({ media: "print" });
  await page.pdf({
    path: output,
    format: "A4",
    printBackground: true,
    preferCSSPageSize: false,
    margin: { top: "10mm", right: "10mm", bottom: "10mm", left: "10mm" },
  });
  process.stdout.write(`${output}\n`);
} finally {
  await browser.close();
}
