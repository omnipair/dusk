import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync, realpathSync, statSync } from 'node:fs';
import { resolve, join, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { deflateSync } from 'node:zlib';
import { setTimeout } from 'node:timers/promises';
import { Connection, PublicKey } from '@solana/web3.js';
import { CONFIG, ROOT, LOADER, git, readBundle, assertProgramBytes, verifyOnChain } from './artifacts.mjs';

export function assertEncryptedMount(images, mount, imageInfo) {
  const image = images.images.find(image => image['system-entities']?.some(entity => entity['mount-point'] === mount));
  assert.ok(image, 'Mount the encrypted deployment disk image first');
  assert.equal(imageInfo.Properties?.Encrypted, true, 'Deployment disk image must be encrypted');
  return image;
}
function plist(command, args) {
  return JSON.parse(execFileSync('plutil', ['-convert', 'json', '-o', '-', '-'], { input: execFileSync(command, args) }));
}
function localSigners(mount) {
  assert.ok(!process.env.CI, 'Signing is local only');
  assert.equal(process.platform, 'darwin', 'Local signer requires the encrypted macOS disk image');
  mount = realpathSync(mount);
  const images = plist('hdiutil', ['info', '-plist']);
  const image = images.images.find(image => image['system-entities']?.some(entity => entity['mount-point'] === mount));
  assert.ok(image, 'Mount the encrypted deployment disk image first');
  assertEncryptedMount(images, mount, plist('hdiutil', ['imageinfo', '-plist', image['image-path']]));
  const files = { deployer: join(mount, 'private/deployer-keypair.json'),
    ...Object.fromEntries(Object.keys(CONFIG.programs).map(name => [name, join(mount, `private/programs/${name}-keypair.json`)])) };
  for (const [name, file] of Object.entries(files)) {
    assert.ok(realpathSync(file).startsWith(mount + sep), 'Signer resolves outside the encrypted image');
    assert.equal(statSync(file).mode & 0o077, 0, 'Signer must be readable only by its owner');
    const publicKey = execFileSync('solana-keygen', ['pubkey', file], { encoding: 'utf8' }).trim();
    assert.equal(publicKey, name === 'deployer' ? CONFIG.deployer : CONFIG.programs[name], `${name} signer ID differs`);
  }
  return files;
}
function run(command, args, env = {}) {
  const result = spawnSync(command, args, { cwd: ROOT, stdio: 'inherit', env: { ...process.env, ...env } });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${command} failed; retain any reported buffer address for resumption`);
}
export async function preflight(connection, bundle) {
  assert.equal(await connection.getGenesisHash(), CONFIG.genesisHash, 'RPC is not mainnet');
  let requiredLamports = 1_000_000_000; // Explicit headroom for upload fees, IDLs and retries.
  const programs = [];
  for (const [name, row] of Object.entries(bundle.manifest.programs)) {
    const program = new PublicKey(row.id);
    const existing = await connection.getAccountInfo(program, 'finalized');
    if (existing) {
      assert.ok(existing.executable && existing.owner.equals(LOADER), `${name} ID is occupied`);
      assert.equal(existing.data.readUInt32LE(0), 2);
      const dataKey = PublicKey.findProgramAddressSync([program.toBuffer()], LOADER)[0];
      assert.ok(new PublicKey(existing.data.subarray(4, 36)).equals(dataKey));
      const data = await connection.getAccountInfo(dataKey, 'finalized');
      assert.ok(data?.owner.equals(LOADER) && data.data.length >= 45 && data.data.readUInt32LE(0) === 3 && data.data[12] === 1);
      assert.equal(new PublicKey(data.data.subarray(13, 45)).toBase58(), CONFIG.deployer, 'Initial release must remain deployer-controlled');
      assertProgramBytes(data.data, readFileSync(resolve(bundle.directory, row.binary.file)));
    } else {
      requiredLamports += await connection.getMinimumBalanceForRentExemption(36);
      requiredLamports += await connection.getMinimumBalanceForRentExemption(45 + row.maxLength);
    }
    const compressed = deflateSync(readFileSync(resolve(bundle.directory, row.idl.file)));
    requiredLamports += await connection.getMinimumBalanceForRentExemption(44 + 2 * compressed.length);
    programs.push({ name, program: row.id, binaryBytes: row.binary.size, allocationBytes: row.maxLength, alreadyDeployed: Boolean(existing) });
  }
  const balanceLamports = await connection.getBalance(new PublicKey(CONFIG.deployer), 'finalized');
  return { sourceCommit: bundle.manifest.sourceCommit, programs, balanceLamports, requiredLamports,
    shortfallLamports: Math.max(0, requiredLamports - balanceLamports) };
}
async function main() {
  const args = process.argv.slice(2);
  const command = args.shift();
  assert.ok(['plan', 'preflight', 'deploy', 'verify', 'attest'].includes(command),
    'Usage: node scripts/release/local.mjs <plan|preflight|deploy|verify|attest> <bundle> [--execute] [--signers]');
  const bundle = readBundle(resolve(args.shift() ?? 'target/mainnet-release'));
  assert.ok(args.every(arg => ['--execute', '--signers'].includes(arg)), 'Unknown release option');
  const execute = args.includes('--execute');
  assert.ok(!execute || ['deploy', 'attest'].includes(command), '--execute is only for deploy or attest');
  if (command === 'plan' || (['deploy', 'attest'].includes(command) && !execute)) {
    console.log(JSON.stringify({ mode: 'preview; no signing or network writes', sourceCommit: bundle.manifest.sourceCommit,
      programs: CONFIG.programs, upgradeAuthority: CONFIG.deployer, idlAuthority: CONFIG.idlAuthority,
      actions: command === 'attest' ? ['verify release bytes and IDLs', 'upload build metadata using the local upgrade authority', 'request independent remote source verification']
        : ['check mainnet, both identities, local encrypted signers and funding', 'deploy each exact binary and immediately upload its IDL', 'verify both deployed binaries, IDLs and authorities'],
      sdk: `@omnipair/dusk-sdk@${bundle.manifest.version}; published separately by publish-sdk.yaml only after verification` }, null, 2));
    return;
  }
  const rpc = process.env.MAINNET_RPC_URL;
  assert.ok(rpc, 'MAINNET_RPC_URL is required');
  const connection = new Connection(rpc, 'finalized');
  if (command === 'verify') {
    console.log(JSON.stringify(await verifyOnChain(connection, bundle), null, 2));
    return;
  }
  const report = await preflight(connection, bundle);
  console.log(JSON.stringify(report, null, 2));
  const files = execute || args.includes('--signers') ? localSigners(process.env.DUSK_DEPLOY_VOLUME ?? '/Volumes/Dusk Deploy') : undefined;
  if (command === 'preflight') {
    assert.equal(report.shortfallLamports, 0, 'Fund the deployer for the reported shortfall before deployment');
    return;
  }
  assert.equal(git('rev-parse', 'HEAD'), bundle.manifest.sourceCommit, 'Checkout the exact release commit');
  assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Release checkout has tracked changes');
  git('merge-base', '--is-ancestor', bundle.manifest.sourceCommit, 'origin/main');
  if (command === 'deploy') {
    assert.equal(report.shortfallLamports, 0, 'Insufficient deployment funding');
    for (const [name, row] of Object.entries(bundle.manifest.programs)) {
      if (!report.programs.find(program => program.name === name).alreadyDeployed) {
        run('solana', ['program', 'deploy', resolve(bundle.directory, row.binary.file), '--program-id', files[name],
          '--keypair', files.deployer, '--fee-payer', files.deployer, '--upgrade-authority', files.deployer,
          '--max-len', String(row.maxLength), '--with-compute-unit-price', '50000', '--max-sign-attempts', '50', '--use-rpc', '--url', rpc]);
      }
      run('scripts/v2/upload_idl.sh', [resolve(bundle.directory, row.idl.file)], { ANCHOR_PROVIDER_URL: rpc, ANCHOR_WALLET: files.deployer });
    }
    // Anchor confirms writes before finalization. Wait for those writes to be
    // visible at the commitment used by the independent readback below.
    const uploadedSlot = await connection.getSlot('confirmed');
    for (let attempt = 0; await connection.getSlot('finalized') < uploadedSlot; attempt++) {
      assert.ok(attempt < 20, 'Finalization is taking longer than expected; rerun the read-only verify command');
      await setTimeout(3000);
    }
    console.log(JSON.stringify(await verifyOnChain(connection, bundle, true), null, 2));
  } else {
    // Remote verification records metadata on chain. It is a separate explicit signing step.
    await verifyOnChain(connection, bundle, true);
    assert.equal(execFileSync('solana-verify', ['--version'], { encoding: 'utf8' }).trim(), `solana-verify ${CONFIG.verifierVersion}`);
    for (const [name, row] of Object.entries(bundle.manifest.programs)) {
      run('solana-verify', ['verify-from-repo', '--remote', '--skip-prompt', '--program-id', row.id,
        '--commit-hash', bundle.manifest.sourceCommit, '--base-image', CONFIG.builderImage, '--arch', 'v0',
        '--library-name', name, '--keypair', files.deployer, '--url', rpc, CONFIG.repository, '--', ...bundle.manifest.cargoArgs]);
    }
    console.log('Remote verification requested for both programs. Check both solana-verify remote get-status results before public launch.');
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch(error => { console.error(String(error.message).replace(/https?:\/\/\S+/g, '<rpc-url>')); process.exitCode = 1; });
}
