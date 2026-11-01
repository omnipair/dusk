#!/usr/bin/env node

// Use the packaged Data Analytics renderer and verifier with one scoped fix for
// its viewport-width header, which otherwise includes the vertical scrollbar.
import { writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const pluginRoot = process.argv[2];
if (!pluginRoot) {
  throw new Error('Usage: node deliver_report.mjs <data-analytics-plugin-root>');
}
const directory = dirname(fileURLToPath(import.meta.url));
const scripts = resolve(pluginRoot, 'skills/build-report/scripts');
const { buildPortableArtifact, readPackagedReaderRuntime } = await import(
  pathToFileURL(resolve(scripts, 'build_portable_artifact.mjs')).href
);
const { deliverPortableArtifact } = await import(
  pathToFileURL(resolve(scripts, 'deliver_portable_artifact.mjs')).href
);
const packaged = readPackagedReaderRuntime();
const style = `<style data-dusk-portable-header-fix>
.dashboard-shell > .analytics-top-bar {
  width: calc(100% + 2 * var(--ds-gutter));
  margin-left: calc(-1 * var(--ds-gutter));
  margin-right: calc(-1 * var(--ds-gutter));
}
</style>`;
// The bundled script contains HTML string literals; insert at the document's
// final closing head rather than an earlier literal inside JavaScript.
const headEnd = packaged.html.lastIndexOf('</head>');
if (headEnd < 0) throw new Error('Reader head missing');
const runtimeHtml = packaged.html.slice(0, headEnd) + style + packaged.html.slice(headEnd);
let receipt;
try {
  receipt = await deliverPortableArtifact({
    inputPath: resolve(directory, 'artifact.json'),
    outputPath: resolve(directory, 'report.html'),
    screenshotPath: resolve(directory, 'report-verification-failure.png'),
  }, {
    build: (input, options = {}) => buildPortableArtifact(input, { ...options, runtimeHtml }),
  });
} catch (error) {
  if (!error.deliveryResult) throw error;
  receipt = error.deliveryResult;
}
writeFileSync(resolve(directory, 'report_delivery.json'), `${JSON.stringify(receipt, null, 2)}\n`);
console.log(JSON.stringify(receipt));
if (!receipt.ok) process.exitCode = 1;
