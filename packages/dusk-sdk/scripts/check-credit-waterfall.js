import assert from "node:assert/strict";
import test from "node:test";
import IDL from "../src/idl_v2.json" with { type: "json" };
import { liquidationFeeAllocation, liquidationLossAllocation } from "../dist/liquidation.js";
import { JUNIOR_EXIT_REQUEST_FEE_BPS, LIQUIDATION_FEE_BPS, trancheParameterUpdate } from "../dist/parameters.js";

test("market creation and settlement expose no retired coverage vaults or donation instruction", () => {
  assert.ok(!IDL.instructions.some(ix => ix.name === "fortify_market"));
  for (const type of IDL.types) {
    assert.ok(!/insurance/i.test(type.name), type.name);
    for (const field of type.type.fields ?? []) assert.ok(!/insurance/i.test(field.name), `${type.name}.${field.name}`);
  }
  const check = accounts => accounts.forEach(account => {
    assert.ok(!/insurance/i.test(account.name), account.name);
    if (account.accounts) check(account.accounts);
  });
  IDL.instructions.forEach(ix => check(ix.accounts));
});

test("a principal shortfall reaches the tranche ledger in full while unpaid interest is canceled", () => {
  assert.deepEqual(liquidationLossAllocation(100n, 110n, 40n), {
    principalRepaid: 40n, interestPaid: 0n, principalWrittenOff: 60n,
    interestCanceled: 10n, surplus: 0n,
  });
  assert.deepEqual(liquidationFeeAllocation(100_000n), { total: 3_000n, protocol: 600n, junior: 2_400n });
});

test("exit and liquidation fees are fixed and cannot be encoded as tranche updates", () => {
  assert.equal(JUNIOR_EXIT_REQUEST_FEE_BPS, 50);
  assert.equal(LIQUIDATION_FEE_BPS, 300);
  assert.throws(() => trancheParameterUpdate({ juniorIncomeShareBps: 5_000, requestFeeBps: 1 }), /Unknown/);
  assert.throws(() => trancheParameterUpdate({ juniorIncomeShareBps: 5_000, liquidationFeeBps: 100 }), /Unknown/);
});
