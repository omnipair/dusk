import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { Connection } from '@solana/web3.js';
import { ROOT, SDK_FILES, idlHash, readBundle, verifyOnChain } from './artifacts.mjs';

async function main() {
  const [directory, expectedCommit] = process.argv.slice(2);
  assert.ok(directory, 'Usage: node scripts/release/verify.mjs <bundle> [expected-commit]');
  const rpc = process.env.MAINNET_RPC_URL;
  assert.ok(rpc, 'MAINNET_RPC_URL is required');
  const bundle = readBundle(directory, expectedCommit);
  assert.equal(bundle.manifest.version, JSON.parse(readFileSync(resolve(ROOT, 'packages/dusk-sdk/package.json'))).version);
  for (const [name, file] of Object.entries(SDK_FILES)) {
    assert.equal(idlHash(JSON.parse(readFileSync(resolve(directory, bundle.manifest.programs[name].idl.file)))),
      idlHash(JSON.parse(readFileSync(resolve(ROOT, `packages/dusk-sdk/src/${file}`)))), `${name} SDK differs from release`);
  }
  console.log(JSON.stringify(await verifyOnChain(new Connection(rpc, 'finalized'), bundle), null, 2));
}
main().catch(error => { console.error(String(error.message).replace(/https?:\/\/\S+/g, '<rpc-url>')); process.exitCode = 1; });
