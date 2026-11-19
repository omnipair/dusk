/** Scoped layout-17 retirement -> production successor. Preview by default. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
import { deflateSync } from 'node:zlib';
import { Connection, PublicKey, SystemProgram } from '@solana/web3.js';
import anchor from '@coral-xyz/anchor';
import { CONFIG, ROOT, LOADER, git, readBundle, assertProgramBytes, decodeIdl, idlHash, sha256, verifyOnChain } from './artifacts.mjs';
import { localSigners } from './local.mjs';
import { readRetirementBundle } from '../retirement/bundle.mjs';

const key = value => new PublicKey(value);
const scope = JSON.parse(readFileSync(new URL('../retirement/scope.json', import.meta.url)));
const retirementIdl = JSON.parse(readFileSync(new URL('../retirement/idl.json', import.meta.url)));
// Temporary Dusk rejects normal calls while delegate is upgraded. Activating
// Dusk last makes the common replay boundary include its first possible writes.
export const SUCCESSOR_UPGRADE_ORDER = ['leverage_delegate', 'dusk'];
export function assertSuccessorActivationOrder(programs) {
  assert.ok(!programs.find(row => row.name === 'dusk').alreadyUpgraded || programs.find(row => row.name === 'leverage_delegate').alreadyUpgraded,
    'Dusk activated before delegate; reconcile the activation gap before resuming');
}
const coder = new anchor.BorshCoder(retirementIdl);
const state = PublicKey.findProgramAddressSync([Buffer.from('legacy_retirement'), key(scope.market).toBuffer()], key(scope.program))[0];
function matches(data, bytes) { try { assertProgramBytes(data, bytes); return true; } catch { return false; } }

export function assertRetirementComplete(market, retirement) {
  assert.ok(market && retirement, 'Legacy retirement and permanent tombstone are required');
  assert.ok(!market.executable && market.owner.equals(key(scope.program)));
  assert.ok(!retirement.executable && retirement.owner.equals(key(scope.program)));
  const retired = coder.accounts.decode('RetiredLegacyMarket', market.data);
  assert.ok(retired.retirement.equals(state)); assert.equal(retired.version, 1);
  const paid = coder.accounts.decode('LegacyRetirement', retirement.data);
  assert.equal(paid.version, 1); assert.equal(paid.completed, true, 'Retirement is not complete');
  assert.equal(paid.cleaned_mask, (1 << scope.owners.length) - 1, 'Personal claims/rent remain');
  assert.equal(paid.paid_mask, (1 << scope.owners.length) - 1, 'User payout claims remain');
  assert.deepEqual(paid.remaining.map(String), ['0', '0'], 'Retirement budget remains unpaid');
  return paid;
}

export async function upgradePreflight(connection, successor, previous) {
  assert.equal(await connection.getGenesisHash(), CONFIG.genesisHash, 'RPC is not mainnet');
  assert.equal(previous.manifest.schema, 'dusk-retirement-bundle.v1');
  const programs = []; let extensionRent = 0, peakBufferRent = 0, idlRent = 0;
  for (const [name, row] of Object.entries(successor.manifest.programs)) {
    const program = key(row.id), dataKey = PublicKey.findProgramAddressSync([program.toBuffer()], LOADER)[0];
    const base = PublicKey.findProgramAddressSync([], program)[0], idlKey = await PublicKey.createWithSeed(base, 'anchor:idl', program);
    const read = await connection.getMultipleAccountsInfoAndContext([program, dataKey, idlKey], { commitment: 'finalized' });
    const [account, data, idlAccount] = read.value;
    assert.ok(account?.executable && account.owner.equals(LOADER) && account.data.readUInt32LE(0) === 2);
    assert.ok(key(account.data.subarray(4, 36)).equals(dataKey));
    assert.ok(data?.owner.equals(LOADER) && !data.executable && data.data.length >= 45 && data.data.readUInt32LE(0) === 3);
    assert.equal(data.data[12], 1); assert.equal(key(data.data.subarray(13, 45)).toBase58(), CONFIG.deployer);
    const candidate = readFileSync(resolve(successor.directory, row.binary.file));
    const alreadyUpgraded = matches(data.data, candidate);
    let priorIdlHash;
    if (name === 'dusk') {
      assert.ok(alreadyUpgraded || matches(data.data, readFileSync(resolve(previous.directory, 'dusk.so'))), 'Dusk is neither the reviewed retirement build nor the successor');
      priorIdlHash = idlHash(JSON.parse(readFileSync(resolve(previous.directory, 'dusk.json'))));
    } else {
      assert.ok(alreadyUpgraded || sha256(data.data.subarray(45)) === scope.legacyIdentity.delegate.binarySha256, 'Unreviewed delegate executable');
      priorIdlHash = scope.legacyIdentity.delegate.idlCanonicalSha256;
    }
    assert.ok(idlAccount?.owner.equals(program) && !idlAccount.executable);
    const currentIdl = decodeIdl(idlAccount.data); assert.equal(currentIdl.authority, CONFIG.idlAuthority);
    const newIdl = JSON.parse(readFileSync(resolve(successor.directory, row.idl.file))), observedIdl = idlHash(currentIdl.idl);
    assert.ok(observedIdl === priorIdlHash || alreadyUpgraded && observedIdl === idlHash(newIdl), 'Unreviewed on-chain IDL');
    if (!alreadyUpgraded) {
      peakBufferRent = Math.max(peakBufferRent, await connection.getMinimumBalanceForRentExemption(37 + candidate.length));
      if (45 + candidate.length > data.data.length)
        extensionRent += Math.max(0, await connection.getMinimumBalanceForRentExemption(45 + candidate.length) - data.lamports);
    }
    idlRent += await connection.getMinimumBalanceForRentExemption(44 + 2 * deflateSync(Buffer.from(JSON.stringify(newIdl))).length);
    programs.push({ name, program: row.id, alreadyUpgraded, checkedSlot: read.context.slot, allocationBytes: data.data.length - 45, binaryBytes: candidate.length });
  }
  assertSuccessorActivationOrder(programs);
  const claims = await connection.getMultipleAccountsInfoAndContext([key(scope.market), state, ...scope.personal.flat().map(row => key(row.address))], { commitment: 'finalized' });
  const paid = assertRetirementComplete(claims.value[0], claims.value[1]);
  for (const account of claims.value.slice(2))
    assert.ok(!account || account.owner.equals(SystemProgram.programId) && account.data.length === 0, 'A personal legacy account has not been closed');
  if (!programs.find(row => row.name === 'dusk').alreadyUpgraded) {
    const frozen = PublicKey.findProgramAddressSync([Buffer.from('legacy_freeze'), key(scope.market).toBuffer()], key(scope.program))[0];
    const known = new Set([...scope.legacyProgramAccounts, state.toBase58(), frozen.toBase58()]);
    const idlDiscriminator = Buffer.from(sha256(Buffer.from('internal:IdlAccount')), 'hex').subarray(0, 8);
    for (const row of await connection.getProgramAccounts(key(scope.program), { commitment: 'finalized' }))
      assert.ok(known.has(row.pubkey.toBase58()) || row.account.data.every(byte => byte === 0)
        || row.account.data.subarray(0, 8).equals(idlDiscriminator), 'Unreconciled legacy account outside the reviewed retirement catalog');
  }
  const balanceLamports = await connection.getBalance(key(CONFIG.deployer), 'finalized');
  const requiredLamports = peakBufferRent + extensionRent + idlRent + 1_000_000_000;
  return { sourceCommit: successor.manifest.sourceCommit, previousCommit: previous.manifest.sourceCommit, programs,
    retirementManifestSha256: Buffer.from(paid.manifest_sha256).toString('hex'), retirementCheckedSlot: claims.context.slot,
    balanceLamports, requiredLamports, peakBufferRent, extensionRent, idlRent,
    shortfallLamports: Math.max(0, requiredLamports - balanceLamports), sdkPublication: 'separate protected workflow after both IDLs verify' };
}

function run(command, args, env = {}) {
  const result = spawnSync(command, args, { cwd: ROOT, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024, env: { ...process.env, ...env } });
  for (const output of [result.stdout, result.stderr]) if (output) process.stdout.write(output.replace(/https?:\/\/\S+/g, '<rpc-url>'));
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${command} failed; preserve any upload buffer address for resumption`);
}
async function main() {
  const [command, nextPath, previousPath, ...options] = process.argv.slice(2);
  assert.ok(['plan', 'preflight', 'upgrade', 'verify'].includes(command) && nextPath && previousPath,
    'Usage: node scripts/release/upgrade.mjs <plan|preflight|upgrade|verify> SUCCESSOR_BUNDLE RETIREMENT_BUNDLE [--execute] [--signers]');
  assert.ok(options.every(option => ['--execute', '--signers'].includes(option)));
  assert.ok(!options.includes('--execute') || command === 'upgrade');
  const successor = readBundle(resolve(nextPath)), previous = readRetirementBundle(resolve(previousPath));
  const execute = options.includes('--execute');
  if (command === 'plan' || command === 'upgrade' && !execute) {
    console.log(JSON.stringify({ mode: 'preview; no signing or network writes', successor: successor.manifest.sourceCommit,
      retirement: previous.manifest.sourceCommit, programs: CONFIG.programs, upgradeOrder: SUCCESSOR_UPGRADE_ORDER,
      requires: ['permanent legacy tombstone', 'all user claims paid and personal rent returned', 'exact approved prior code/IDLs', 'funded encrypted local signer'],
      sdkPublication: false }, null, 2)); return;
  }
  assert.ok(process.env.MAINNET_RPC_URL, 'MAINNET_RPC_URL is required');
  const connection = new Connection(process.env.MAINNET_RPC_URL, 'finalized');
  if (command === 'verify') { console.log(JSON.stringify(await verifyOnChain(connection, successor, true), null, 2)); return; }
  const report = await upgradePreflight(connection, successor, previous); console.log(JSON.stringify(report, null, 2));
  assert.equal(report.shortfallLamports, 0, 'Fund the exact reported upgrade shortfall first');
  const files = execute || options.includes('--signers') ? localSigners(process.env.DUSK_DEPLOY_VOLUME ?? '/Volumes/Dusk Deploy') : undefined;
  if (!execute) return;
  assert.equal(git('rev-parse', 'HEAD'), successor.manifest.sourceCommit);
  assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Upgrade checkout has tracked changes');
  git('merge-base', '--is-ancestor', successor.manifest.sourceCommit, 'origin/main');
  for (const name of SUCCESSOR_UPGRADE_ORDER) {
    const row = successor.manifest.programs[name];
    if (!report.programs.find(program => program.name === name).alreadyUpgraded)
      run('solana', ['program', 'deploy', resolve(successor.directory, row.binary.file), '--program-id', row.id,
        '--keypair', files.deployer, '--fee-payer', files.deployer, '--upgrade-authority', files.deployer,
        '--with-compute-unit-price', '50000', '--max-sign-attempts', '50', '--use-rpc', '--url', process.env.MAINNET_RPC_URL]);
    run('scripts/v2/upload_idl.sh', [resolve(successor.directory, row.idl.file)], { ANCHOR_PROVIDER_URL: process.env.MAINNET_RPC_URL, ANCHOR_WALLET: files.deployer });
  }
  const uploaded = await connection.getSlot('confirmed');
  for (let attempt = 0; await connection.getSlot('finalized') < uploaded; attempt++) {
    assert.ok(attempt < 20, 'Finalization pending; rerun read-only verify');
    await new Promise(done => setTimeout(done, 3000));
  }
  console.log(JSON.stringify(await verifyOnChain(connection, successor, true), null, 2));
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch(error => { console.error(String(error.message).replace(/https?:\/\/\S+/g, '<rpc-url>')); process.exitCode = 1; });
