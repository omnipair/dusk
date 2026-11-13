import assert from "node:assert/strict";
import { test } from "node:test";
import { PROTOCOL_AUCTION_SLOTS_PER_DAY as day, quoteProtocolAuction } from "../dist/protocol-auctions.js";
const auction = { virtualSoldReserve: 1000n, virtualPaymentReserve: 1000n, soldRatePerDay: 1000n, lastUpdateSlot: 10n, streamRemainder: 0n };
const quote = (overrides = {}) => quoteProtocolAuction({ auction, soldNetAmount: 100n, currentSlot: 10n, ...overrides });

test("Inverse curve ages before its first purchase and buys raise the price", () => {
  assert.equal(quote().paymentAmount, 112n);
  const aged = quote({ currentSlot: 10n + day });
  assert.equal(aged.paymentAmount, 27n);
  assert.equal(aged.nextState.virtualSoldReserve, 1900n);
  assert.equal(aged.nextState.virtualPaymentReserve, 527n);
  assert.equal(quote({ auction: aged.nextState, currentSlot: 10n + day }).paymentAmount, 30n);
  assert.equal(auction.lastUpdateSlot, 10n);
});
test("raw mint units round payment up and preserve treasury/staking totals", () => {
  for (const [sold, payment, output, expected] of [[1_000_000_000n, 1_000_000n, 1n, 1n], [1_000_000n, 1_000_000_000n, 1n, 1001n], [200n, 101n, 100n, 101n]]) {
    const result = quote({ auction: { ...auction, virtualSoldReserve: sold, virtualPaymentReserve: payment, soldRatePerDay: 0n }, soldNetAmount: output, stakingVaultBps: 3333 });
    assert.equal(result.paymentAmount, expected);
    assert.equal(result.treasuryAmount + result.stakingVaultAmount, expected);
  }
});
test("fractional emission survives purchases and large idle intervals stay finite", () => {
  const initial = { ...auction, virtualSoldReserve: 10_000n, virtualPaymentReserve: 10_000n, soldRatePerDay: 1n };
  const first = quote({ auction: initial, soldNetAmount: 1n, currentSlot: 10n + day - 1n });
  const second = quote({ auction: first.nextState, soldNetAmount: 1n, currentSlot: 10n + day });
  assert.equal(first.nextState.streamRemainder, day - 1n);
  assert.equal(second.nextState.streamRemainder, 0n);
  assert.equal(second.nextState.virtualSoldReserve, 9999n);
  const max = (1n << 64n) - 1n;
  assert.equal(quote({ auction: { ...auction, virtualSoldReserve: max, virtualPaymentReserve: max, soldRatePerDay: max }, currentSlot: max, soldNetAmount: 1n }).paymentAmount, 1n);
});
test("rejects invalid amounts, regressed clocks, exhausted reserves and integer overflow", () => {
  for (const bad of [0n, 1000n, 1001n, -1n, 1n << 64n, Number.MAX_SAFE_INTEGER + 1]) assert.throws(() => quote({ soldNetAmount: bad }));
  assert.throws(() => quote({ currentSlot: 9n }));
  assert.throws(() => quote({ stakingVaultBps: 10_001 }));
  assert.throws(() => quote({ auction: { ...auction, virtualPaymentReserve: 0n } }));
  assert.throws(() => quote({ auction: { ...auction, streamRemainder: day } }));
  assert.throws(() => quote({ auction: { ...auction, virtualPaymentReserve: (1n << 128n) - 1n } }));
});
