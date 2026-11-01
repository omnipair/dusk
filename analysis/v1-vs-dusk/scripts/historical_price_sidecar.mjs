#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import {
  access,
  mkdir,
  readFile,
  rename,
  writeFile,
} from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import process from 'node:process';
import readline from 'node:readline';
import { fileURLToPath } from 'node:url';

const HOUR_SECONDS = 3_600;
const DEFAULT_PROVIDER = 'birdeye';
const DEFAULT_BIRDEYE_URL = 'https://public-api.birdeye.so';
const SCRIPT_PATH = fileURLToPath(import.meta.url);
const SCRIPT_DIR = path.dirname(SCRIPT_PATH);
const DEFAULT_ENV_PATH = path.resolve(
  SCRIPT_DIR,
  '../../../../omnipair-indexer/api/.env',
);
const DEFAULT_INDEXER_PACKAGE = path.resolve(
  SCRIPT_DIR,
  '../../../../omnipair-indexer/api/package.json',
);

const CLASSIFICATION_SQL = `
WITH requested AS (
  SELECT *
  FROM unnest($1::text[], $2::bigint[])
    AS requested(mint, bucket_unix_seconds)
)
SELECT
  requested.mint,
  requested.bucket_unix_seconds::text,
  snapshots.provider,
  snapshots.quality,
  snapshots.price_usd::text,
  snapshots.decimals,
  snapshots.created_at,
  snapshots.updated_at
FROM requested
LEFT JOIN token_price_snapshots AS snapshots
  ON snapshots.mint = requested.mint
 AND snapshots.bucket = to_timestamp(requested.bucket_unix_seconds)
ORDER BY
  requested.bucket_unix_seconds,
  requested.mint,
  snapshots.provider
`;

function fail(message) {
  throw new Error(message);
}

export function floorHour(unixSeconds) {
  return Math.floor(unixSeconds / HOUR_SECONDS) * HOUR_SECONDS;
}

function keyFor(mint, bucket) {
  return `${mint}@${bucket}`;
}

function parsePositiveInteger(value, name) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    fail(`${name} must be a positive safe integer`);
  }
  return parsed;
}

function parseNonNegativeInteger(value, name) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0) {
    fail(`${name} must be a non-negative safe integer`);
  }
  return parsed;
}

export function parseArgs(argv) {
  const [command, ...rest] = argv;
  if (!command || command === '--help' || command === '-h') {
    return { command: 'help', options: new Map(), flags: new Set() };
  }
  const options = new Map();
  const flags = new Set();
  for (let index = 0; index < rest.length; index += 1) {
    const name = rest[index];
    if (!name.startsWith('--')) {
      fail(`unexpected positional argument: ${name}`);
    }
    const next = rest[index + 1];
    if (!next || next.startsWith('--')) {
      flags.add(name);
      continue;
    }
    options.set(name, next);
    index += 1;
  }
  return { command, options, flags };
}

function requiredOption(options, name) {
  const value = options.get(name);
  if (!value) {
    fail(`missing required option ${name}`);
  }
  return path.resolve(value);
}

async function* jsonLines(filePath) {
  const input = createReadStream(filePath, { encoding: 'utf8' });
  const lines = readline.createInterface({ input, crlfDelay: Infinity });
  let lineNumber = 0;
  for await (const line of lines) {
    lineNumber += 1;
    if (!line.trim()) {
      continue;
    }
    try {
      yield JSON.parse(line);
    } catch (error) {
      throw new Error(`invalid JSON at ${filePath}:${lineNumber}: ${error.message}`);
    }
  }
}

async function sha256File(filePath) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(filePath)) {
    hash.update(chunk);
  }
  return hash.digest('hex');
}

function sha256Text(text) {
  return createHash('sha256').update(text).digest('hex');
}

function stableJson(value) {
  if (Array.isArray(value)) {
    return `[${value.map(stableJson).join(',')}]`;
  }
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map((key) => (
      `${JSON.stringify(key)}:${stableJson(value[key])}`
    )).join(',')}}`;
  }
  return JSON.stringify(value);
}

async function assertFreshOutput(filePath, force) {
  try {
    await access(filePath);
    if (!force) {
      fail(`refusing to replace existing artifact without --force: ${filePath}`);
    }
  } catch (error) {
    if (error?.code !== 'ENOENT') {
      throw error;
    }
  }
}

async function writeAtomic(filePath, contents, force = false) {
  await mkdir(path.dirname(filePath), { recursive: true });
  await assertFreshOutput(filePath, force);
  const temporary = `${filePath}.partial-${process.pid}`;
  await writeFile(temporary, contents, { encoding: 'utf8', mode: 0o600 });
  await rename(temporary, filePath);
}

async function writeJson(filePath, value, force = false) {
  await writeAtomic(filePath, `${JSON.stringify(value, null, 2)}\n`, force);
}

async function writeJsonl(filePath, rows, force = false) {
  const contents = rows.map((row) => JSON.stringify(row)).join('\n');
  await writeAtomic(filePath, contents ? `${contents}\n` : '', force);
}

export async function collectPriceGaps(operationsPath, pricesPath, provider) {
  const marketTokens = new Map();
  const expected = new Map();
  let expectedReferences = 0;
  let operationRows = 0;

  for await (const operation of jsonLines(operationsPath)) {
    operationRows += 1;
    if (operation?.payload?.kind === 'initialize_market') {
      const payload = operation.payload.payload;
      const tokens = [
        { mint: payload.token0, token_decimals: payload.token0_decimals },
        { mint: payload.token1, token_decimals: payload.token1_decimals },
      ];
      const previous = marketTokens.get(operation.pair);
      if (previous && stableJson(previous) !== stableJson(tokens)) {
        fail(`conflicting token identity for market ${operation.pair}`);
      }
      marketTokens.set(operation.pair, tokens);
    }
    if (operation?.payload?.kind === 'update_config' || operation?.pair === '__global__') {
      continue;
    }
    const tokens = marketTokens.get(operation.pair);
    if (!tokens) {
      fail(`market token identity missing before operation ${operation.operation_id}`);
    }
    const bucket = floorHour(operation.block_time_unix_seconds);
    for (const token of tokens) {
      const key = keyFor(token.mint, bucket);
      let row = expected.get(key);
      if (!row) {
        row = {
          schema_version: 1,
          mint: token.mint,
          bucket_unix_seconds: bucket,
          token_decimals: token.token_decimals,
          operation_references: 0,
          markets: new Set(),
        };
        expected.set(key, row);
      }
      if (row.token_decimals !== token.token_decimals) {
        fail(`conflicting canonical decimals for ${token.mint}`);
      }
      row.operation_references += 1;
      row.markets.add(operation.pair);
      expectedReferences += 1;
    }
  }

  const observed = new Map();
  let priceRows = 0;
  for await (const mark of jsonLines(pricesPath)) {
    priceRows += 1;
    if (mark.provider !== provider) {
      continue;
    }
    if (
      typeof mark.mint !== 'string'
      || !mark.mint
      || !Number.isSafeInteger(mark.bucket_unix_seconds)
      || mark.bucket_unix_seconds % HOUR_SECONDS !== 0
      || typeof mark.price_usd !== 'string'
      || !(Number(mark.price_usd) > 0)
      || mark.quality !== 'historical'
    ) {
      fail(`invalid configured-provider historical price mark at row ${priceRows}`);
    }
    const key = keyFor(mark.mint, mark.bucket_unix_seconds);
    if (observed.has(key)) {
      fail(`duplicate configured-provider historical price mark ${key}`);
    }
    observed.set(key, mark);
  }

  const missing = [];
  let observedExpectedKeys = 0;
  for (const [key, value] of expected.entries()) {
    if (observed.has(key)) {
      observedExpectedKeys += 1;
      continue;
    }
    missing.push({
      ...value,
      markets: [...value.markets].sort(),
    });
  }
  missing.sort((left, right) => (
    left.bucket_unix_seconds - right.bucket_unix_seconds
    || left.mint.localeCompare(right.mint)
  ));

  return {
    operationRows,
    priceRows,
    expectedReferences,
    expectedUniqueKeys: expected.size,
    observedExpectedKeys,
    missing,
  };
}

function parseEnv(contents) {
  const result = new Map();
  for (const rawLine of contents.split(/\r?\n/u)) {
    const line = rawLine.trim();
    if (!line || line.startsWith('#')) {
      continue;
    }
    const normalized = line.startsWith('export ') ? line.slice(7) : line;
    const separator = normalized.indexOf('=');
    if (separator <= 0) {
      continue;
    }
    const name = normalized.slice(0, separator).trim();
    let value = normalized.slice(separator + 1).trim();
    if (
      value.length >= 2
      && ((value.startsWith('"') && value.endsWith('"'))
        || (value.startsWith("'") && value.endsWith("'")))
    ) {
      value = value.slice(1, -1);
    }
    result.set(name, value);
  }
  return result;
}

async function loadEnvironment(envPath) {
  const parsed = parseEnv(await readFile(envPath, 'utf8'));
  for (const [name, value] of parsed) {
    if (!process.env[name]) {
      process.env[name] = value;
    }
  }
}

function canonicalDbRow(row) {
  return {
    mint: row.mint,
    bucket_unix_seconds: Number(row.bucket_unix_seconds),
    provider: row.provider,
    quality: row.quality,
    price_usd: row.price_usd,
    decimals: row.decimals,
    created_at: row.created_at instanceof Date
      ? row.created_at.toISOString()
      : row.created_at,
    updated_at: row.updated_at instanceof Date
      ? row.updated_at.toISOString()
      : row.updated_at,
  };
}

async function queryMissingRowsReadOnly(missing, databaseUrl, indexerPackage) {
  if (!databaseUrl) {
    fail('DATABASE_URL is not configured');
  }
  const requireFromIndexer = createRequire(indexerPackage);
  const { Client } = requireFromIndexer('pg');
  const client = new Client({ connectionString: databaseUrl });
  await client.connect();
  try {
    await client.query(
      'BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY',
    );
    const snapshot = await client.query(`
      SELECT
        current_setting('transaction_read_only') AS transaction_read_only,
        transaction_timestamp() AS transaction_timestamp,
        txid_current_snapshot()::text AS transaction_snapshot
    `);
    if (snapshot.rows[0]?.transaction_read_only !== 'on') {
      fail('database classification transaction is not read-only');
    }
    const schema = await client.query(`
      SELECT column_name, data_type, is_nullable, column_default
      FROM information_schema.columns
      WHERE table_schema = current_schema()
        AND table_name = 'token_price_snapshots'
      ORDER BY ordinal_position
    `);
    const result = await client.query(CLASSIFICATION_SQL, [
      missing.map((row) => row.mint),
      missing.map((row) => row.bucket_unix_seconds),
    ]);
    await client.query('COMMIT');
    return {
      rows: result.rows.filter((row) => row.provider !== null).map(canonicalDbRow),
      snapshot: {
        transaction_read_only: snapshot.rows[0].transaction_read_only,
        transaction_timestamp: snapshot.rows[0].transaction_timestamp instanceof Date
          ? snapshot.rows[0].transaction_timestamp.toISOString()
          : snapshot.rows[0].transaction_timestamp,
        transaction_snapshot: snapshot.rows[0].transaction_snapshot,
        schema_sha256: sha256Text(stableJson(schema.rows)),
      },
    };
  } catch (error) {
    try {
      await client.query('ROLLBACK');
    } catch {
      // Preserve the original error.
    }
    throw error;
  } finally {
    await client.end();
  }
}

function positiveDecimal(value) {
  return typeof value === 'string' && Number(value) > 0;
}

export function classifyDatabaseRows(rows, provider) {
  if (rows.length === 0) {
    return 'absent';
  }
  if (rows.some((row) => (
    row.provider === provider
    && row.quality === 'historical'
    && positiveDecimal(row.price_usd)
  ))) {
    return 'configured_provider_historical_positive';
  }
  if (rows.some((row) => row.provider === provider && row.quality === 'estimated')) {
    return 'configured_provider_estimated';
  }
  if (rows.some((row) => row.provider === provider && row.quality === 'current')) {
    return 'configured_provider_current';
  }
  if (rows.some((row) => row.provider === provider && row.quality === 'missing')) {
    return 'configured_provider_missing_marker';
  }
  if (rows.some((row) => (
    row.provider !== provider
    && row.quality === 'historical'
    && positiveDecimal(row.price_usd)
  ))) {
    return 'other_provider_historical_positive';
  }
  return 'stored_nonhistorical_or_invalid';
}

function increment(map, key, amount = 1) {
  map[key] = (map[key] ?? 0) + amount;
}

async function classifyCommand(options, flags) {
  const operationsPath = requiredOption(options, '--operations');
  const pricesPath = requiredOption(options, '--prices');
  const outputDir = requiredOption(options, '--output-dir');
  const provider = options.get('--provider') ?? DEFAULT_PROVIDER;
  const envPath = path.resolve(options.get('--env') ?? DEFAULT_ENV_PATH);
  const indexerPackage = path.resolve(
    options.get('--indexer-package') ?? DEFAULT_INDEXER_PACKAGE,
  );
  const force = flags.has('--force');
  await loadEnvironment(envPath);

  const gaps = await collectPriceGaps(operationsPath, pricesPath, provider);
  const database = await queryMissingRowsReadOnly(
    gaps.missing,
    process.env.DATABASE_URL,
    indexerPackage,
  );
  database.rows.sort((left, right) => (
    left.bucket_unix_seconds - right.bucket_unix_seconds
    || left.mint.localeCompare(right.mint)
    || left.provider.localeCompare(right.provider)
  ));

  const rowsByKey = new Map();
  for (const row of database.rows) {
    const key = keyFor(row.mint, row.bucket_unix_seconds);
    const values = rowsByKey.get(key) ?? [];
    values.push(row);
    rowsByKey.set(key, values);
  }
  const classificationCounts = {};
  const referenceCounts = {};
  const byMint = {};
  const missingRows = gaps.missing.map((row) => {
    const dbRows = rowsByKey.get(keyFor(row.mint, row.bucket_unix_seconds)) ?? [];
    const classification = classifyDatabaseRows(dbRows, provider);
    increment(classificationCounts, classification);
    increment(referenceCounts, classification, row.operation_references);
    const mintSummary = byMint[row.mint] ?? {
      keys: 0,
      operation_references: 0,
      classifications: {},
    };
    mintSummary.keys += 1;
    mintSummary.operation_references += row.operation_references;
    increment(mintSummary.classifications, classification);
    byMint[row.mint] = mintSummary;
    return {
      ...row,
      db_classification: classification,
      db_row_count: dbRows.length,
    };
  });

  const missingPath = path.join(outputDir, 'missing-price-keys.jsonl');
  const dbRowsPath = path.join(outputDir, 'database-price-rows.jsonl');
  const summaryPath = path.join(outputDir, 'classification-summary.json');
  await writeJsonl(missingPath, missingRows, force);
  await writeJsonl(dbRowsPath, database.rows, force);
  const summary = {
    schema_version: 1,
    created_at: new Date().toISOString(),
    purpose: 'read_only_historical_price_gap_classification',
    provider,
    source_inputs: {
      canonical_operations: {
        path: operationsPath,
        sha256: await sha256File(operationsPath),
        rows: gaps.operationRows,
      },
      exported_historical_prices: {
        path: pricesPath,
        sha256: await sha256File(pricesPath),
        rows: gaps.priceRows,
      },
    },
    audit_grain: {
      expected_operation_references: gaps.expectedReferences,
      expected_unique_mint_hour_keys: gaps.expectedUniqueKeys,
      exported_expected_unique_mint_hour_keys: gaps.observedExpectedKeys,
      missing_operation_references: missingRows.reduce(
        (sum, row) => sum + row.operation_references,
        0,
      ),
      missing_unique_mint_hour_keys: missingRows.length,
    },
    database_query: {
      table: 'token_price_snapshots',
      quality_filter_applied: false,
      provider_filter_applied: false,
      transaction: database.snapshot,
      sql_sha256: sha256Text(CLASSIFICATION_SQL),
      matched_rows: database.rows.length,
      rows_sha256: sha256Text(
        database.rows.map((row) => JSON.stringify(row)).join('\n')
          + (database.rows.length ? '\n' : ''),
      ),
    },
    classification_counts_by_unique_key: classificationCounts,
    classification_counts_by_operation_reference: referenceCounts,
    by_mint: byMint,
    artifacts: {
      missing_price_keys: {
        path: missingPath,
        sha256: await sha256File(missingPath),
      },
      database_price_rows: {
        path: dbRowsPath,
        sha256: await sha256File(dbRowsPath),
      },
    },
  };
  await writeJson(summaryPath, summary, force);
  console.log(JSON.stringify({
    summary: summaryPath,
    missing_unique_keys: missingRows.length,
    missing_operation_references: summary.audit_grain.missing_operation_references,
    classifications: classificationCounts,
  }, null, 2));
}

function parseProviderTimestamp(value) {
  if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
    return Math.floor(value > 10_000_000_000 ? value / 1_000 : value);
  }
  if (typeof value === 'string') {
    const numeric = Number(value);
    if (Number.isFinite(numeric) && numeric > 0) {
      return Math.floor(numeric > 10_000_000_000 ? numeric / 1_000 : numeric);
    }
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) {
      return Math.floor(parsed / 1_000);
    }
  }
  return null;
}

function parseProviderPrice(value) {
  if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
    return String(value);
  }
  if (typeof value === 'string' && Number.isFinite(Number(value)) && Number(value) > 0) {
    return value;
  }
  return null;
}

export function extractProviderPoints(responseBody) {
  const data = responseBody?.data ?? responseBody;
  const items = Array.isArray(data?.items)
    ? data.items
    : Array.isArray(data?.history)
      ? data.history
      : Array.isArray(data)
        ? data
        : [];
  const points = [];
  for (const item of items) {
    const timestamp = parseProviderTimestamp(
      item?.unixTime ?? item?.unix_time ?? item?.timestamp ?? item?.time ?? item?.t,
    );
    const price = parseProviderPrice(item?.value ?? item?.price ?? item?.usdPrice);
    if (timestamp !== null && price !== null) {
      points.push({
        raw_timestamp_unix_seconds: timestamp,
        bucket_unix_seconds: floorHour(timestamp),
        price_usd: price,
      });
    }
  }
  return points;
}

export function planHistoricalRequests(keys, chunkHours = 168) {
  if (!Number.isSafeInteger(chunkHours) || chunkHours <= 0) {
    fail('chunkHours must be a positive safe integer');
  }
  const byMint = new Map();
  for (const key of keys) {
    const rows = byMint.get(key.mint) ?? [];
    rows.push(key.bucket_unix_seconds);
    byMint.set(key.mint, rows);
  }
  const requests = [];
  for (const [mint, unsorted] of [...byMint].sort(([left], [right]) => (
    left.localeCompare(right)
  ))) {
    const buckets = [...new Set(unsorted)].sort((left, right) => left - right);
    let index = 0;
    while (index < buckets.length) {
      const from = buckets[index];
      const toExclusive = from + chunkHours * HOUR_SECONDS;
      const requestedBuckets = [];
      while (index < buckets.length && buckets[index] < toExclusive) {
        requestedBuckets.push(buckets[index]);
        index += 1;
      }
      requests.push({ mint, from, to_exclusive: toExclusive, requestedBuckets });
    }
  }
  return requests;
}

function safeBaseUrl(value) {
  const url = new URL(value);
  if (url.username || url.password || url.search || url.hash) {
    fail('historical provider base URL must not contain credentials, query, or fragment');
  }
  return url.toString().replace(/\/$/u, '');
}

function historyUrl(baseUrl, request) {
  const url = new URL('/defi/history_price', `${baseUrl}/`);
  url.searchParams.set('address', request.mint);
  url.searchParams.set('address_type', 'token');
  url.searchParams.set('type', '1H');
  url.searchParams.set('time_from', String(request.from));
  url.searchParams.set('time_to', String(request.to_exclusive));
  url.searchParams.set('ui_amount_mode', 'raw');
  return url.toString();
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function fetchWithProof(url, apiKey, timeoutMs, retries) {
  const attempts = [];
  for (let attempt = 0; attempt <= retries; attempt += 1) {
    const startedAt = new Date().toISOString();
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const response = await fetch(url, {
        method: 'GET',
        headers: {
          'X-API-KEY': apiKey,
          'x-chain': 'solana',
        },
        signal: controller.signal,
      });
      const bodyText = await response.text();
      let body = null;
      try {
        body = JSON.parse(bodyText);
      } catch {
        // The raw response remains hash-bound below.
      }
      const record = {
        started_at: startedAt,
        completed_at: new Date().toISOString(),
        status: response.status,
        ok: response.ok,
        response_body_sha256: sha256Text(bodyText),
        body_text: bodyText,
        body,
      };
      attempts.push(record);
      if (response.ok) {
        return { attempts, response: record };
      }
      if (![429, 500, 502, 503, 504].includes(response.status) || attempt === retries) {
        return { attempts, response: record };
      }
      const retryAfter = Number(response.headers.get('retry-after'));
      await sleep(Number.isFinite(retryAfter) ? retryAfter * 1_000 : 500 * (2 ** attempt));
    } catch (error) {
      const record = {
        started_at: startedAt,
        completed_at: new Date().toISOString(),
        status: null,
        ok: false,
        error: error?.name === 'AbortError' ? 'timeout' : String(error?.message ?? error),
      };
      attempts.push(record);
      if (attempt === retries) {
        return { attempts, response: record };
      }
      await sleep(500 * (2 ** attempt));
    } finally {
      clearTimeout(timer);
    }
  }
  fail('unreachable provider retry state');
}

function providerResponseSucceeded(record) {
  return record.ok && record.body !== null && record.body?.success !== false;
}

async function fetchCommand(options, flags) {
  const classificationDir = requiredOption(options, '--classification-dir');
  const outputDir = requiredOption(options, '--output-dir');
  const envPath = path.resolve(options.get('--env') ?? DEFAULT_ENV_PATH);
  const force = flags.has('--force');
  const chunkHours = parsePositiveInteger(
    options.get('--chunk-hours') ?? '168',
    '--chunk-hours',
  );
  const delayMs = parseNonNegativeInteger(options.get('--delay-ms') ?? '50', '--delay-ms');
  const timeoutMs = parsePositiveInteger(
    options.get('--timeout-ms') ?? '15000',
    '--timeout-ms',
  );
  const retries = parseNonNegativeInteger(options.get('--retries') ?? '2', '--retries');
  const maxRequests = options.has('--max-requests')
    ? parsePositiveInteger(options.get('--max-requests'), '--max-requests')
    : Number.POSITIVE_INFINITY;
  await loadEnvironment(envPath);
  const apiKey = process.env.BIRDEYE_API_KEY;
  if (!apiKey) {
    fail('BIRDEYE_API_KEY is not configured');
  }
  const baseUrl = safeBaseUrl(process.env.BIRDEYE_API_URL ?? DEFAULT_BIRDEYE_URL);
  const summaryPath = path.join(classificationDir, 'classification-summary.json');
  const missingPath = path.join(classificationDir, 'missing-price-keys.jsonl');
  const classification = JSON.parse(await readFile(summaryPath, 'utf8'));
  const expectedMissingSha = classification?.artifacts?.missing_price_keys?.sha256;
  const actualMissingSha = await sha256File(missingPath);
  if (!expectedMissingSha || expectedMissingSha !== actualMissingSha) {
    fail('classification missing-key artifact hash mismatch');
  }
  const keys = [];
  for await (const row of jsonLines(missingPath)) {
    if (row.db_classification === 'configured_provider_historical_positive') {
      continue;
    }
    keys.push(row);
  }
  const provider = classification.provider;
  if (provider !== DEFAULT_PROVIDER) {
    fail(`unsupported historical provider sidecar: ${provider}`);
  }
  const completePlan = planHistoricalRequests(keys, chunkHours);
  const plan = completePlan.slice(0, maxRequests);
  const responseDir = path.join(outputDir, 'provider-responses');
  await mkdir(responseDir, { recursive: true });
  const pointsByKey = new Map();
  const requestByKey = new Map();
  const responseManifest = [];

  for (let index = 0; index < plan.length; index += 1) {
    const request = plan[index];
    const url = historyUrl(baseUrl, request);
    const fetched = await fetchWithProof(url, apiKey, timeoutMs, retries);
    const responseRecord = {
      schema_version: 1,
      request_index: index,
      request: {
        method: 'GET',
        url,
        provider,
        chain: 'solana',
        interval: '1H',
        allow_current_fallback: false,
        requested_bucket_count: request.requestedBuckets.length,
      },
      attempts: fetched.attempts,
    };
    const responsePath = path.join(
      responseDir,
      `${String(index).padStart(4, '0')}-${request.mint}-${request.from}.json`,
    );
    await writeJson(responsePath, responseRecord, force);
    const responseSha = await sha256File(responsePath);
    responseManifest.push({
      request_index: index,
      path: responsePath,
      sha256: responseSha,
      ok: providerResponseSucceeded(fetched.response),
    });
    for (const bucket of request.requestedBuckets) {
      requestByKey.set(keyFor(request.mint, bucket), {
        ok: providerResponseSucceeded(fetched.response),
        request_index: index,
        response_sha256: responseSha,
      });
    }
    if (providerResponseSucceeded(fetched.response)) {
      for (const point of extractProviderPoints(fetched.response.body)) {
        const key = keyFor(request.mint, point.bucket_unix_seconds);
        if (!request.requestedBuckets.includes(point.bucket_unix_seconds)) {
          continue;
        }
        const values = pointsByKey.get(key) ?? [];
        values.push({ ...point, response_sha256: responseSha, request_index: index });
        pointsByKey.set(key, values);
      }
    }
    if (delayMs > 0 && index + 1 < plan.length) {
      await sleep(delayMs);
    }
  }

  const processedKeys = keys.filter((key) => requestByKey.has(
    keyFor(key.mint, key.bucket_unix_seconds),
  ));
  const recovered = [];
  const recoveredProvenance = [];
  const unresolved = [];
  for (const key of processedKeys) {
    const identity = keyFor(key.mint, key.bucket_unix_seconds);
    const points = pointsByKey.get(identity) ?? [];
    const request = requestByKey.get(identity);
    if (points.length === 1) {
      recovered.push({
        mint: key.mint,
        bucket_unix_seconds: key.bucket_unix_seconds,
        price_usd: points[0].price_usd,
        token_decimals: key.token_decimals,
        provider,
        quality: 'historical',
      });
      recoveredProvenance.push({
        schema_version: 1,
        mint: key.mint,
        bucket_unix_seconds: key.bucket_unix_seconds,
        raw_provider_timestamp_unix_seconds: points[0].raw_timestamp_unix_seconds,
        price_usd: points[0].price_usd,
        request_index: points[0].request_index,
        provider_response_sha256: points[0].response_sha256,
      });
      continue;
    }
    unresolved.push({
      schema_version: 1,
      mint: key.mint,
      bucket_unix_seconds: key.bucket_unix_seconds,
      operation_references: key.operation_references,
      reason: !request.ok
        ? 'provider_request_failed'
        : points.length === 0
          ? 'provider_no_exact_hour_point'
          : 'ambiguous_provider_hour_points',
      provider_point_count: points.length,
      request_index: request.request_index,
      response_sha256: request.response_sha256,
    });
  }
  recovered.sort((left, right) => (
    left.bucket_unix_seconds - right.bucket_unix_seconds
    || left.mint.localeCompare(right.mint)
  ));
  recoveredProvenance.sort((left, right) => (
    left.bucket_unix_seconds - right.bucket_unix_seconds
    || left.mint.localeCompare(right.mint)
  ));
  unresolved.sort((left, right) => (
    left.bucket_unix_seconds - right.bucket_unix_seconds
    || left.mint.localeCompare(right.mint)
  ));

  const marksPath = path.join(outputDir, 'historical-price-marks.jsonl');
  const provenancePath = path.join(outputDir, 'historical-price-mark-provenance.jsonl');
  const unresolvedPath = path.join(outputDir, 'unresolved-price-keys.jsonl');
  const manifestPath = path.join(outputDir, 'fetch-manifest.json');
  await writeJsonl(marksPath, recovered, force);
  await writeJsonl(provenancePath, recoveredProvenance, force);
  await writeJsonl(unresolvedPath, unresolved, force);
  const unresolvedByReason = {};
  let recoveredReferences = 0;
  let unresolvedReferences = 0;
  for (const key of processedKeys) {
    const identity = keyFor(key.mint, key.bucket_unix_seconds);
    if ((pointsByKey.get(identity) ?? []).length === 1) {
      recoveredReferences += key.operation_references;
    }
  }
  for (const row of unresolved) {
    increment(unresolvedByReason, row.reason);
    unresolvedReferences += row.operation_references;
  }
  const manifest = {
    schema_version: 1,
    created_at: new Date().toISOString(),
    purpose: 'proof_bound_read_only_historical_provider_sidecar',
    provider,
    chain: 'solana',
    endpoint: '/defi/history_price',
    interval: '1H',
    allow_current_fallback: false,
    interpolation: false,
    forward_fill: false,
    source_inputs: {
      classification_summary: {
        path: summaryPath,
        sha256: await sha256File(summaryPath),
      },
      missing_price_keys: {
        path: missingPath,
        sha256: actualMissingSha,
      },
      sidecar_script: {
        path: SCRIPT_PATH,
        sha256: await sha256File(SCRIPT_PATH),
      },
    },
    request_plan: {
      chunk_hours: chunkHours,
      complete_request_count: completePlan.length,
      executed_request_count: plan.length,
      complete: plan.length === completePlan.length,
      timeout_ms: timeoutMs,
      retries,
      delay_ms: delayMs,
    },
    outcome: {
      processed_unique_keys: processedKeys.length,
      recovered_unique_keys: recovered.length,
      recovered_operation_references: recoveredReferences,
      unresolved_unique_keys: unresolved.length,
      unresolved_operation_references: unresolvedReferences,
      unresolved_by_reason: unresolvedByReason,
    },
    artifacts: {
      historical_price_marks: {
        path: marksPath,
        sha256: await sha256File(marksPath),
      },
      historical_price_mark_provenance: {
        path: provenancePath,
        sha256: await sha256File(provenancePath),
      },
      unresolved_price_keys: {
        path: unresolvedPath,
        sha256: await sha256File(unresolvedPath),
      },
      provider_responses: responseManifest,
      provider_responses_manifest_sha256: sha256Text(stableJson(responseManifest)),
    },
  };
  await writeJson(manifestPath, manifest, force);
  console.log(JSON.stringify({
    manifest: manifestPath,
    executed_requests: plan.length,
    recovered_unique_keys: recovered.length,
    unresolved_unique_keys: unresolved.length,
    unresolved_by_reason: unresolvedByReason,
  }, null, 2));
}

function help() {
  console.log(`Historical price sidecar (read-only DB and provider access)

Classify benchmark gaps against token_price_snapshots without a quality filter:
  node scripts/historical_price_sidecar.mjs classify \\
    --operations PATH/canonical-operations.jsonl \\
    --prices PATH/historical-prices.jsonl \\
    --output-dir PATH/price-classification \\
    [--env PATH/.env] [--provider birdeye] [--force]

Fetch historical-only provider marks into proof-bound local artifacts:
  node scripts/historical_price_sidecar.mjs fetch \\
    --classification-dir PATH/price-classification \\
    --output-dir PATH/price-provider-fetch \\
    [--env PATH/.env] [--chunk-hours 168] [--delay-ms 50] \\
    [--timeout-ms 15000] [--retries 2] [--max-requests N] [--force]

The fetch command never calls a current-price endpoint, interpolates, or writes
to PostgreSQL. API credentials are read from the environment and never saved.
`);
}

async function main() {
  const { command, options, flags } = parseArgs(process.argv.slice(2));
  if (command === 'help') {
    help();
    return;
  }
  if (command === 'classify') {
    await classifyCommand(options, flags);
    return;
  }
  if (command === 'fetch') {
    await fetchCommand(options, flags);
    return;
  }
  fail(`unknown command: ${command}`);
}

if (path.resolve(process.argv[1] ?? '') === SCRIPT_PATH) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
