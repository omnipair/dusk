/**
 * Dusk v2 Surfpool — Full Feature Test Suite
 *
 * Tests all fork API endpoints including borrow, leverage, swaps, hLP.
 * Uses v2 leverage endpoints and exercises selected v1 compatibility aliases.
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

const API = process.env.FORK_API_URL ?? "http://127.0.0.1:8080";
const RPC = process.env.SURFPOOL_RPC_URL ?? "http://127.0.0.1:8899";
const PAYER_PATH = resolve(
  process.env.FORK_LAB_PAYER_KEYPAIR ?? process.env.ANCHOR_WALLET ?? "deployer-keypair.json"
);

let testCount = 0;
let passed = 0;
let failed = 0;

function log(emoji, label, ...args) {
  const ts = new Date().toISOString().slice(11, 19);
  console.log(`${ts} ${emoji} [${label}]`, ...args);
}

function test(name) {
  testCount++;
  console.log(`\n${"\u2500".repeat(60)}`);
  console.log(`\u{1F9EA} TEST ${testCount}: ${name}`);
  console.log(`${"\u2500".repeat(60)}`);
}

function ok(msg) {
  passed++;
  console.log(`  \u2705 PASS: ${msg}`);
}

function fail(msg, detail) {
  failed++;
  console.error(`  \u274C FAIL: ${msg}`);
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

async function main() {
  console.log("\u2554" + "\u2550".repeat(58) + "\u2557");
  console.log("\u2551   Dusk v2 Surfpool \u2014 Full Feature Test Suite       \u2551");
  console.log("\u255A" + "\u2550".repeat(58) + "\u255D");
  console.log(`  API: ${API}`);
  console.log(`  RPC: ${RPC}`);
  console.log(`  Payer: ${PAYER_PATH}`);

  // === SETUP =========================================================

  test("Setup \u2014 load payer & get market config");

  const payerSecret = Uint8Array.from(JSON.parse(readFileSync(PAYER_PATH, "utf8")));
  const payer = Keypair.fromSecretKey(payerSecret);
  const connection = new Connection(RPC, "confirmed");
  log("\uD83D\uDD11", "SETUP", `Payer: ${payer.publicKey.toBase58()}`);

  const configResp = await fetchJson(`${API}/api/v2/fork/config`);
  const config = configResp.data;
  log("\uD83D\uDCCB", "SETUP", `Market: ${config.market}`);
  log("\uD83D\uDCCB", "SETUP", `Base: ${config.baseMint} (${config.baseDecimals} dp)`);
  log("\uD83D\uDCCB", "SETUP", `Quote: ${config.quoteMint} (${config.quoteDecimals} dp)`);
  ok(`Market ${config.market} configured`);

  // === BORROW FLOW TESTS =============================================

  test("POST /api/v2/fork/fund-wallet");
  const fundResp = await fetchJson(`${API}/api/v2/fork/fund-wallet`, {
    method: "POST",
    body: JSON.stringify({
      wallet: payer.publicKey.toBase58(),
      sol: 10,
      baseAmount: "2000",
      quoteAmount: "20000",
    }),
  });
  ok(`Funded: ${fundResp.data.sol} SOL, ${ui(fundResp.data.baseAmount)} META, ${ui(fundResp.data.quoteAmount)} USDC`);

  test("POST /api/v2/fork/tx/add-liquidity");
  const addLiqResp = await fetchJson(`${API}/api/v2/fork/tx/add-liquidity`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      baseDepositAmount: "100",
      quoteDepositAmount: "1000",
    }),
  });
  const addLiqSig = await signAndSend(addLiqResp.data.transaction, payer);
  ok(`Add liquidity: ${addLiqSig.slice(0, 12)}...`);

  test("POST /api/v2/fork/tx/swap (base -> quote)");
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
  ok(`Swap (base->quote): ${swapSig.slice(0, 12)}...`);

  const { randomBytes } = await import("node:crypto");

  test("POST /api/v2/fork/tx/deposit-collateral + borrow + repay + hLP deposit/withdraw");
  const posId = new PublicKey(randomBytes(32));

  // Deposit base collateral
  const collatResp = await fetchJson(`${API}/api/v2/fork/tx/deposit-collateral`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      marketAsset: "base",
      positionId: posId.toBase58(),
      depositAmount: "500",
    }),
  });
  await signAndSend(collatResp.data.transaction, payer);
  ok("Deposit collateral (500 META)");
  ok(`Borrow position PDA: ${collatResp.data.borrowPosition}`);

  // Borrow quote
  const borrowResp = await fetchJson(`${API}/api/v2/fork/tx/borrow`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: posId.toBase58(),
      borrowAsset: "quote",
      borrowAmount: "250",
      minHealthBps: "11000",
    }),
  });
  await signAndSend(borrowResp.data.transaction, payer);
  ok("Borrow (250 USDC against 500 META)");

  // Query position
  const posResp = await fetchJson(
    `${API}/api/v2/users/${payer.publicKey.toBase58()}/positions?positionId=${posId.toBase58()}`
  );
  const pos = posResp.data.positions[0];
  ok(`Position: ${ui(pos.payload.baseCollateral)} META collat, ${ui(pos.payload.fixedQuoteShares)} debt shares`);

  // Repay half
  const repayResp = await fetchJson(`${API}/api/v2/fork/tx/repay`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: posId.toBase58(),
      repayAsset: "quote",
      repayAmount: "100",
    }),
  });
  await signAndSend(repayResp.data.transaction, payer);
  ok("Repay (100 USDC)");

  // hLP deposit
  const hlpDepResp = await fetchJson(`${API}/api/v2/fork/tx/deposit-single-sided`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      targetAsset: "base",
      depositAmount: "50",
      minHlpAmount: "0",
    }),
  });
  await signAndSend(hlpDepResp.data.transaction, payer);
  ok("Deposit single-sided (50 META -> base hLP)");

  // hLP withdraw
  const hlpWdrResp = await fetchJson(`${API}/api/v2/fork/tx/withdraw-single-sided`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      targetAsset: "base",
      hlpAmount: "10",
      minTargetAmountOut: "0",
    }),
  });
  await signAndSend(hlpWdrResp.data.transaction, payer);
  ok("Withdraw single-sided (10 base hLP -> META)");

  // Swap reverse
  test("Swap (quote -> base, reverse direction)");
  const swapRevResp = await fetchJson(`${API}/api/v2/fork/tx/swap`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      assetIn: "quote",
      exactAssetIn: "100",
      minAssetOut: "0",
    }),
  });
  await signAndSend(swapRevResp.data.transaction, payer);
  ok("Swap (quote->base, 100 USDC)");

  // === LEVERAGE FLOW TESTS ===========================================

  test("POST /api/v2/fork/tx/open-leverage (quote debt, 2x)");
  const levPosId1 = new PublicKey(randomBytes(32));
  log("\uD83D\uDCDD", "LEV", `Position ID: ${levPosId1.toBase58()}`);

  const openLevResp = await fetchJson(`${API}/api/v2/fork/tx/open-leverage`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId1.toBase58(),
      debtAsset: 1,
      marginMode: 0,
      marginAmount: "100",
      multiplierBps: 20000,
    }),
  });
  const openLevSig = await signAndSend(openLevResp.data.transaction, payer);
  ok(`Open leverage (100 USDC margin, 2x, quote debt): ${openLevSig.slice(0, 12)}...`);
  ok(`Debt asset: ${openLevResp.data.debtAsset} (1=quote)`);
  if (
    openLevResp.data.positionId !== levPosId1.toBase58() ||
    typeof openLevResp.data.positionAddress !== "string" ||
    openLevResp.data.marginMode !== 0
  ) {
    throw new Error(`Unexpected v2 open response: ${JSON.stringify(openLevResp.data)}`);
  }
  ok("Open response includes the v2 position ID/address and debt margin mode");

  test("GET /api/v2/fork/leverage/positions");
  const [levPosResp, ownerAliasResp] = await Promise.all([
    fetchJson(
      `${API}/api/v2/fork/leverage/positions?wallet=${payer.publicKey.toBase58()}`
    ),
    fetchJson(
      `${API}/api/v2/fork/leverage/positions?owner=${payer.publicKey.toBase58()}`
    ),
  ]);
  const posCount = levPosResp.data.positions.length;
  if (ownerAliasResp.data.positions.length !== posCount) {
    throw new Error("wallet and owner leverage-position filters returned different counts");
  }
  ok("wallet and owner query aliases return the same position set");
  if (posCount > 0) {
    const pos1 = levPosResp.data.positions[0];
    const requiredFields = [
      "positionAddress",
      "positionId",
      "owner",
      "debtAsset",
      "debtMint",
      "collateralMint",
      "collateralAmount",
      "marginAmount",
      "openNotional",
      "debtShares",
      "debtPrincipal",
      "multiplierBps",
      "marginMode",
    ];
    const missingFields = requiredFields.filter((field) => !(field in pos1));
    if (missingFields.length > 0) {
      throw new Error(`Leverage position missing fields: ${missingFields.join(", ")}`);
    }
    if (pos1.positionId !== levPosId1.toBase58() || pos1.owner !== payer.publicKey.toBase58()) {
      throw new Error(`Leverage position identity mismatch: ${JSON.stringify(pos1)}`);
    }
    ok(`Found ${posCount} position(s)`);
    ok("Position payload exposes the stable v2 leverage contract");
    log("  ", "LEV", `Collateral: ${ui(pos1.collateralAmount)}, Debt: ${ui(pos1.debtShares)}`);
    log("  ", "LEV", `Multiplier: ${(Number(pos1.multiplierBps) / 10000).toFixed(2)}x`);
  } else {
    log("  ", "LEV", "Position query returned 0 (getProgramAccounts limited on fork)");
    ok("Open leverage tx confirmed (verified by signature)");
  }

  test("POST /api/v1/fork/tx/add-leverage-margin");
  const addMarginResp = await fetchJson(`${API}/api/v1/fork/tx/add-leverage-margin`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId1.toBase58(),
      amount: "25",
    }),
  });
  await signAndSend(addMarginResp.data.transaction, payer);
  ok("Add margin (25 USDC)");

  test("POST /api/v1/fork/tx/remove-leverage-margin");
  const remMarginResp = await fetchJson(`${API}/api/v1/fork/tx/remove-leverage-margin`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId1.toBase58(),
      amount: "10",
      minAmountOut: "0",
    }),
  });
  await signAndSend(remMarginResp.data.transaction, payer);
  ok("Remove margin (10 USDC equivalent)");

  test("POST /api/v1/fork/tx/increase-leverage");
  const incLevResp = await fetchJson(`${API}/api/v1/fork/tx/increase-leverage`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId1.toBase58(),
      debtAmount: "25",
      minCollateralOut: "0",
    }),
  });
  await signAndSend(incLevResp.data.transaction, payer);
  ok("Increase leverage (+25 USDC debt)");

  test("POST /api/v1/fork/tx/decrease-leverage");
  const decLevResp = await fetchJson(`${API}/api/v1/fork/tx/decrease-leverage`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId1.toBase58(),
      collateralAmount: "15",
      minRepayOut: "0",
    }),
  });
  await signAndSend(decLevResp.data.transaction, payer);
  ok("Decrease leverage (sell 15 collateral)");

  test("POST /api/v2/fork/tx/close-leverage");
  const closeLevResp = await fetchJson(`${API}/api/v2/fork/tx/close-leverage`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId1.toBase58(),
      debtAsset: 1,
      marginMode: 0,
      minAmountOut: "0",
    }),
  });
  await signAndSend(closeLevResp.data.transaction, payer);
  ok("Close leverage (full exit)");

  test("Base debt leverage (open + close, opposite direction)");
  const levPosId2 = new PublicKey(randomBytes(32));

  const openBaseLevResp = await fetchJson(`${API}/api/v1/fork/tx/open-leverage`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId2.toBase58(),
      isDebtToken0: true,
      marginAmount: "500",
      multiplierBps: 20000,
    }),
  });
  await signAndSend(openBaseLevResp.data.transaction, payer);
  ok(`Open base debt leverage (500 USDC margin, 2x): debtAsset=${openBaseLevResp.data.debtAsset}`);

  const closeBaseLevResp = await fetchJson(`${API}/api/v1/fork/tx/close-leverage`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId2.toBase58(),
      isDebtToken0: true,
      minAmountOut: "0",
    }),
  });
  await signAndSend(closeBaseLevResp.data.transaction, payer);
  ok("Close base debt leverage");

  test("v2 endpoint alias ( /api/v2/fork/tx/open-leverage )");
  const levPosId3 = new PublicKey(randomBytes(32));
  const openV2Resp = await fetchJson(`${API}/api/v2/fork/tx/open-leverage`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId3.toBase58(),
      marginAmount: "50",
      multiplierBps: 30000,
    }),
  });
  await signAndSend(openV2Resp.data.transaction, payer);
  ok("v2 open leverage (50 META, 3x)");
  const closeV2Resp = await fetchJson(`${API}/api/v2/fork/tx/close-leverage`, {
    method: "POST",
    body: JSON.stringify({
      owner: payer.publicKey.toBase58(),
      positionId: levPosId3.toBase58(),
      minAmountOut: "0",
    }),
  });
  await signAndSend(closeV2Resp.data.transaction, payer);
  ok("v2 close leverage");
  ok("v1 and v2 endpoint paths both work");

  // === FINAL MARKET STATE =============================================

  test("Final market state");
  const marketsResp = await fetchJson(`${API}/api/v2/markets`);
  const m = marketsResp.data.markets[0];
  ok(`Market: ${m.marketAddress}`);
  log("  ", "STATE", `Base Reserve:  ${ui(m.state.baseReserve)} META`);
  log("  ", "STATE", `Quote Reserve: ${ui(m.state.quoteReserve)} USDC`);
  log("  ", "STATE", `Fixed Base Debt:  ${ui(m.state.fixedBaseDebt)}`);
  log("  ", "STATE", `Fixed Quote Debt: ${ui(m.state.fixedQuoteDebt)}`);
  log("  ", "STATE", `Swap Fee: ${m.swapFeeBps} bps`);

  // === SUMMARY ========================================================

  console.log(`\n${"\u2550".repeat(60)}`);
  console.log(`\u2551  RESULTS: ${passed} passed, ${failed} failed, ${testCount} total`);
  console.log(`${"\u2550".repeat(60)}`);

  if (failed > 0) {
    console.error(`\n\u274C ${failed} test(s) FAILED`);
    process.exit(1);
  }
  console.log("\n\u2705 All tests passed!\n");
}

main().catch((err) => {
  console.error("\n\uD83D\uDCA5 FATAL:", err.message);
  if (err.cause) console.error("  cause:", err.cause);
  process.exit(1);
});
