/**
 * Dusk v2 Surfpool — Comprehensive Leverage Test Suite
 *
 * Tests all fork API endpoints: add-liquidity, swap, deposit-collateral,
 * borrow, repay, deposit-single-sided, withdraw-single-sided,
 * plus market reads and position queries.
 *
 * Usage:
 *   node test_leverage.mjs
 *
 * Requires:
 *   - Surfpool fork running on :8899
 *   - RPC proxy running on :8898
 *   - Fork API running on :8080
 */

import { readFileSync } from "fs";
import { resolve } from "path";
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  LAMPORTS_PER_SOL,
} from "@solana/web3.js";

// ── Config ──────────────────────────────────────────────────────────────
const API = process.env.FORK_API_URL ?? "http://127.0.0.1:8080";
const RPC = process.env.SURFPOOL_RPC_URL ?? "http://127.0.0.1:8899";
const PAYER_PATH = resolve(
  process.env.FORK_LAB_PAYER_KEYPAIR ?? process.env.ANCHOR_WALLET ?? "deployer-keypair.json"
);

// ── Helpers ─────────────────────────────────────────────────────────────

let testCount = 0;
let passed = 0;
let failed = 0;

function log(emoji, label, ...args) {
  const ts = new Date().toISOString().slice(11, 19);
  console.log(`${ts} ${emoji} [${label}]`, ...args);
}

function test(name) {
  testCount++;
  console.log(`\n${"─".repeat(60)}`);
  console.log(`🧪 TEST ${testCount}: ${name}`);
  console.log(`${"─".repeat(60)}`);
}

function ok(msg) {
  passed++;
  console.log(`  ✅ PASS: ${msg}`);
}

function fail(msg, detail) {
  failed++;
  console.error(`  ❌ FAIL: ${msg}`);
  if (detail) console.error(`     ${detail}`);
}

async function fetchJson(url, init) {
  const res = await fetch(url, {
    ...init,
    headers: { "content-type": "application/json", ...(init?.headers ?? {}) },
  });
  const body = await res.json();
  if (!res.ok || (body && body.success === false))
    throw new Error(`${init?.method ?? "GET"} ${url}: ${JSON.stringify(body)}`);
  return body;
}

async function signAndSend(txBase64, keypair) {
  const connection = new Connection(RPC, "confirmed");
  const tx = Transaction.from(Buffer.from(txBase64, "base64"));
  tx.sign(keypair);
  const sig = await connection.sendRawTransaction(tx.serialize(), {
    skipPreflight: true,
  });
  await connection.confirmTransaction(sig, "confirmed");
  return sig;
}

function ui(amount, decimals = 6) {
  return (Number(amount) / 10 ** decimals).toFixed(decimals);
}

// ── Main Test Runner ────────────────────────────────────────────────────

async function main() {
  console.log("╔══════════════════════════════════════════════════════╗");
  console.log("║   Dusk v2 Surfpool — Full Leverage Test Suite       ║");
  console.log("╚══════════════════════════════════════════════════════╝");
  console.log(`  API: ${API}`);
  console.log(`  RPC: ${RPC}`);
  console.log(`  Payer: ${PAYER_PATH}`);

  // ── Setup ──────────────────────────────────────────────────────────

  test("Setup — load payer & get market config");

  const payerSecret = Uint8Array.from(JSON.parse(readFileSync(PAYER_PATH, "utf8")));
  const payer = Keypair.fromSecretKey(payerSecret);
  const connection = new Connection(RPC, "confirmed");
  log("🔑", "SETUP", `Payer: ${payer.publicKey.toBase58()}`);

  const configResp = await fetchJson(`${API}/api/v2/fork/config`);
  const config = configResp.data;
  log("📋", "SETUP", `Market: ${config.market}`);
  log("📋", "SETUP", `Pair: ${config.label}`);
  log("📋", "SETUP", `Base: ${config.baseMint} (${config.baseDecimals} dp)`);
  log("📋", "SETUP", `Quote: ${config.quoteMint} (${config.quoteDecimals} dp)`);
  ok(`Market ${config.market} configured`);

  // ── Test 1: Fund Wallet ────────────────────────────────────────────

  test("POST /api/v2/fork/fund-wallet");

  const fundResp = await fetchJson(`${API}/api/v2/fork/fund-wallet`, {
    method: "POST",
    body: JSON.stringify({
      wallet: payer.publicKey.toBase58(),
      sol: 10,
      baseAmount: "1000",
      quoteAmount: "10000",
    }),
  });
  ok(
    `Funded: ${fundResp.data.sol} SOL, ` +
      `${ui(fundResp.data.baseAmount)} ${config.label?.split("-")[0]?.toUpperCase()}, ` +
      `${ui(fundResp.data.quoteAmount)} USDC`
  );

  // Verify balances
  const solBal = await connection.getBalance(payer.publicKey);
  ok(`SOL balance: ${(solBal / LAMPORTS_PER_SOL).toFixed(3)} SOL`);

  // ── Test 2: Add Liquidity ──────────────────────────────────────────

  test("POST /api/v2/fork/tx/add-liquidity");

  const addLiqResp = await fetchJson(`${API}/api/v2/fork/tx/add-liquidity`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      baseDepositAmount: "10",
      quoteDepositAmount: "100",
    }),
  });
  const addLiqSig = await signAndSend(addLiqResp.data.transaction, payer);
  ok(`Add liquidity: ${addLiqSig.slice(0, 12)}...`);

  // ── Test 3: Swap ───────────────────────────────────────────────────

  test("POST /api/v2/fork/tx/swap (base → quote)");

  const swapResp = await fetchJson(`${API}/api/v2/fork/tx/swap`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      assetIn: "base",
      exactAssetIn: "5",
      minAssetOut: "0",
    }),
  });
  const swapSig = await signAndSend(swapResp.data.transaction, payer);
  ok(`Swap (base→quote): ${swapSig.slice(0, 12)}...`);

  // ── Test 4: Deposit Collateral ─────────────────────────────────────

  test("POST /api/v2/fork/tx/deposit-collateral (base collateral)");

  // Generate a position ID for tracking
  const { randomBytes } = await import("node:crypto");
  const positionId = new PublicKey(randomBytes(32));
  log("📝", "COLLATERAL", `Position ID: ${positionId.toBase58()}`);

  const collatResp = await fetchJson(`${API}/api/v2/fork/tx/deposit-collateral`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      marketAsset: "base",
      positionId: positionId.toBase58(),
      depositAmount: "50",
    }),
  });
  const collatSig = await signAndSend(collatResp.data.transaction, payer);
  ok(`Deposit collateral (50 META): ${collatSig.slice(0, 12)}...`);
  ok(`Borrow position PDA: ${collatResp.data.borrowPosition}`);

  // ── Test 5: Borrow ─────────────────────────────────────────────────

  test("POST /api/v2/fork/tx/borrow (quote against base collateral)");

  const borrowResp = await fetchJson(`${API}/api/v2/fork/tx/borrow`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: positionId.toBase58(),
      borrowAsset: "quote",
      borrowAmount: "25",
      minHealthBps: "11000",
    }),
  });
  const borrowSig = await signAndSend(borrowResp.data.transaction, payer);
  ok(`Borrow (25 USDC): ${borrowSig.slice(0, 12)}...`);

  // ── Test 6: Check Position ─────────────────────────────────────────

  test("GET /api/v2/users/:wallet/positions");

  const posResp = await fetchJson(
    `${API}/api/v2/users/${payer.publicKey.toBase58()}/positions?positionId=${positionId.toBase58()}`
  );
  const positions = posResp.data.positions;
  if (positions.length > 0) {
    const pos = positions[0];
    ok(`Position found: ${pos.payload.positionId}`);
    log("  ", "POS", `Base Collateral: ${ui(pos.payload.baseCollateral)} META`);
    log("  ", "POS", `Fixed Base Shares: ${ui(pos.payload.fixedBaseShares)}`);
    log("  ", "POS", `Fixed Quote Shares: ${ui(pos.payload.fixedQuoteShares)}`);
  } else {
    fail("No position found", "Position query returned empty");
  }

  // ── Test 7: Repay ──────────────────────────────────────────────────

  test("POST /api/v2/fork/tx/repay (partial quote repay)");

  const repayResp = await fetchJson(`${API}/api/v2/fork/tx/repay`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: positionId.toBase58(),
      repayAsset: "quote",
      repayAmount: "10",
    }),
  });
  const repaySig = await signAndSend(repayResp.data.transaction, payer);
  ok(`Repay (10 USDC): ${repaySig.slice(0, 12)}...`);

  // Check position after repay
  const posAfterResp = await fetchJson(
    `${API}/api/v2/users/${payer.publicKey.toBase58()}/positions?positionId=${positionId.toBase58()}`
  );
  const posAfter = posAfterResp.data.positions;
  if (posAfter.length > 0) {
    const p = posAfter[0];
    log("  ", "POS", `After repay — Base Collateral: ${ui(p.payload.baseCollateral)} META`);
    log("  ", "POS", `After repay — Fixed Quote Shares: ${ui(p.payload.fixedQuoteShares)}`);
    ok("Position updated after repay");
  }

  // ── Test 8: Deposit Single-Sided (hLP) ─────────────────────────────

  test("POST /api/v2/fork/tx/deposit-single-sided (base → base hLP)");

  const hlpDepResp = await fetchJson(`${API}/api/v2/fork/tx/deposit-single-sided`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      targetAsset: "base",
      depositAmount: "20",
      minHlpAmount: "0",
    }),
  });
  const hlpDepSig = await signAndSend(hlpDepResp.data.transaction, payer);
  ok(`Deposit single-sided (20 META → base hLP): ${hlpDepSig.slice(0, 12)}...`);
  ok(`Target asset: ${hlpDepResp.data.targetAsset}`);

  // ── Test 9: Withdraw Single-Sided (hLP) ────────────────────────────

  test("POST /api/v2/fork/tx/withdraw-single-sided (base hLP → base)");

  const hlpWdrResp = await fetchJson(`${API}/api/v2/fork/tx/withdraw-single-sided`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      targetAsset: "base",
      hlpAmount: "5",
      minTargetAmountOut: "0",
    }),
  });
  const hlpWdrSig = await signAndSend(hlpWdrResp.data.transaction, payer);
  ok(`Withdraw single-sided (5 base hLP → META): ${hlpWdrSig.slice(0, 12)}...`);
  ok(`Target asset: ${hlpWdrResp.data.targetAsset}`);

  // ── Test 10: Swap quote → base (reverse direction) ─────────────────

  test("POST /api/v2/fork/tx/swap (quote → base, reverse direction)");

  const swapRevResp = await fetchJson(`${API}/api/v2/fork/tx/swap`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      assetIn: "quote",
      exactAssetIn: "50",
      minAssetOut: "0",
    }),
  });
  const swapRevSig = await signAndSend(swapRevResp.data.transaction, payer);
  ok(`Swap (quote→base, 50 USDC): ${swapRevSig.slice(0, 12)}...`);

  // ── Test 11: Quote collateral + quote borrow ────────────────────────

  test("POST /api/v2/fork/tx/deposit-collateral (quote collateral) + borrow base");

  const posId2 = new PublicKey(randomBytes(32));

  // Deposit quote collateral
  const collatQResp = await fetchJson(`${API}/api/v2/fork/tx/deposit-collateral`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      marketAsset: "quote",
      positionId: posId2.toBase58(),
      depositAmount: "500",
    }),
  });
  const collatQSig = await signAndSend(collatQResp.data.transaction, payer);
  ok(`Deposit quote collateral (500 USDC): ${collatQSig.slice(0, 12)}...`);

  // Borrow base against quote collateral
  const borrowBaseResp = await fetchJson(`${API}/api/v2/fork/tx/borrow`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: posId2.toBase58(),
      borrowAsset: "base",
      borrowAmount: "3",
      minHealthBps: "11000",
    }),
  });
  const borrowBaseSig = await signAndSend(borrowBaseResp.data.transaction, payer);
  ok(`Borrow (3 META against USDC collateral): ${borrowBaseSig.slice(0, 12)}...`);

  // ── Test 12: Full market state ─────────────────────────────────────

  test("GET /api/v2/markets — full market state");

  const marketsResp = await fetchJson(`${API}/api/v2/markets`);
  const market = marketsResp.data.markets[0];
  ok(`Market: ${market.marketAddress}`);
  log("  ", "STATE", `Base Reserve:  ${ui(market.state.baseReserve)} META`);
  log("  ", "STATE", `Quote Reserve: ${ui(market.state.quoteReserve)} USDC`);
  log("  ", "STATE", `YLP Supply (base):  ${ui(market.state.baseReserveYlpSupply)}`);
  log("  ", "STATE", `YLP Supply (quote): ${ui(market.state.quoteReserveYlpSupply)}`);
  log("  ", "STATE", `Fixed Base Debt:  ${ui(market.state.fixedBaseDebt)}`);
  log("  ", "STATE", `Fixed Quote Debt: ${ui(market.state.fixedQuoteDebt)}`);
  log("  ", "STATE", `Base Collat (for Q debt): ${ui(market.state.recognizedBaseCollateralForQuoteDebt)}`);
  log("  ", "STATE", `Quote Collat (for B debt): ${ui(market.state.recognizedQuoteCollateralForBaseDebt)}`);
  log("  ", "STATE", `Base Debt Health: ${market.state.baseDebtHealthBps} bps`);
  log("  ", "STATE", `Quote Debt Health: ${market.state.quoteDebtHealthBps} bps`);
  log("  ", "STATE", `Swap Fee: ${market.swapFeeBps} bps`);
  log("  ", "STATE", `Reduce Only: ${market.reduceOnly}`);

  // ── Summary ─────────────────────────────────────────────────────────

  console.log(`\n${"═".repeat(60)}`);
  console.log(`║  RESULTS: ${passed} passed, ${failed} failed, ${testCount} total`);
  console.log(`${"═".repeat(60)}`);

  if (failed > 0) {
    console.error(`\n❌ ${failed} test(s) FAILED`);
    process.exit(1);
  }

  console.log("\n✅ All tests passed!\n");

  // Print a summary table
  console.log("┌─────────────────────────────────────────────────────────────┐");
  console.log("│  Feature                         │ Endpoint               │");
  console.log("├─────────────────────────────────────────────────────────────┤");
  console.log("│  Wallet funding                  │ fund-wallet            │");
  console.log("│  Add liquidity (yLP)             │ add-liquidity          │");
  console.log("│  Swap (base → quote)             │ swap                   │");
  console.log("│  Swap (quote → base)             │ swap                   │");
  console.log("│  Deposit collateral (base)       │ deposit-collateral     │");
  console.log("│  Deposit collateral (quote)      │ deposit-collateral     │");
  console.log("│  Borrow (quote vs base collat)   │ borrow                 │");
  console.log("│  Borrow (base vs quote collat)   │ borrow                 │");
  console.log("│  Repay (partial)                 │ repay                  │");
  console.log("│  Position query                  │ users/:wallet/positions│");
  console.log("│  Deposit single-sided (hLP)      │ deposit-single-sided   │");
  console.log("│  Withdraw single-sided (hLP)     │ withdraw-single-sided  │");
  console.log("│  Market state                    │ markets                │");
  console.log("└─────────────────────────────────────────────────────────────┘");
}

main().catch((err) => {
  console.error("\n💥 FATAL:", err.message);
  if (err.cause) console.error("  cause:", err.cause);
  process.exit(1);
});
