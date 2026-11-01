import assert from 'node:assert/strict';
import test from 'node:test';

import {
  classifyDatabaseRows,
  extractProviderPoints,
  floorHour,
  planHistoricalRequests,
} from './historical_price_sidecar.mjs';

test('classifies historical rows before nonhistorical fallbacks', () => {
  assert.equal(classifyDatabaseRows([
    { provider: 'birdeye', quality: 'estimated', price_usd: '2' },
    { provider: 'birdeye', quality: 'historical', price_usd: '1.5' },
  ], 'birdeye'), 'configured_provider_historical_positive');
  assert.equal(classifyDatabaseRows([
    { provider: 'birdeye', quality: 'estimated', price_usd: '2' },
  ], 'birdeye'), 'configured_provider_estimated');
  assert.equal(classifyDatabaseRows([], 'birdeye'), 'absent');
});
test('extracts provider points while preserving the raw timestamp', () => {
  const timestamp = 1_788_000_123;
  assert.deepEqual(extractProviderPoints({
    success: true,
    data: { items: [{ unixTime: timestamp, value: 1.25 }] },
  }), [{
    raw_timestamp_unix_seconds: timestamp,
    bucket_unix_seconds: floorHour(timestamp),
    price_usd: '1.25',
  }]);
});

test('plans bounded per-mint ranges without dropping sparse keys', () => {
  const plan = planHistoricalRequests([
    { mint: 'A', bucket_unix_seconds: 0 },
    { mint: 'A', bucket_unix_seconds: 3_600 },
    { mint: 'A', bucket_unix_seconds: 10_800 },
    { mint: 'B', bucket_unix_seconds: 0 },
  ], 2);
  assert.deepEqual(plan, [
    { mint: 'A', from: 0, to_exclusive: 7_200, requestedBuckets: [0, 3_600] },
    { mint: 'A', from: 10_800, to_exclusive: 18_000, requestedBuckets: [10_800] },
    { mint: 'B', from: 0, to_exclusive: 7_200, requestedBuckets: [0] },
  ]);
});
