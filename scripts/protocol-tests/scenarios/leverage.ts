import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import { Keypair, PublicKey } from "@solana/web3.js";

import { formatUnits, type ProtocolTestHarness, type ScenarioDefinition } from "../harness.js";

const quoteDebtPositionId = Keypair.generate().publicKey;
const baseDebtPositionId = Keypair.generate().publicKey;
const delegationPositionId = Keypair.generate().publicKey;
const delegatedClosePositionId = Keypair.generate().publicKey;
const liquidationPositionId = Keypair.generate().publicKey;
const multiplierBoundaryPositionIds = {
  base: Keypair.generate().publicKey,
  quote: Keypair.generate().publicKey,
};
const marginBoundaryPositionIds = {
  base: Keypair.generate().publicKey,
  quote: Keypair.generate().publicKey,
};
const increaseBoundaryPositionIds = {
  base: Keypair.generate().publicKey,
  quote: Keypair.generate().publicKey,
};
const leverageDelegateProgramId = new PublicKey("EPGF9iFrbGnhWgC3To9rC9vxinEYuDHaz4RXgLPvuRkp");

function raw(uiAmount: number, decimals: number): bigint {
  return BigInt(uiAmount) * 10n ** BigInt(decimals);
}

function marketState(
  market: Awaited<ReturnType<ProtocolTestHarness["market"]>>,
  key: string
): bigint {
  const value = market.state[key];
  if (value === undefined) throw new Error(`Market state does not expose ${key}`);
  return BigInt(value);
}

async function leveragePosition(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey
): Promise<any> {
  const positions = await harness.positions(wallet, positionId);
  const position = positions.find((entry) => entry.eventType === "leverage_position");
  if (!position) throw new Error(`Leverage position ${positionId.toBase58()} was not found`);
  return position.payload;
}

async function assertLeveragePositionClosed(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey,
  label: string
): Promise<void> {
  const positions = await harness.positions(wallet, positionId);
  harness.assertEqual(
    label,
    positions.filter((entry) => entry.eventType === "leverage_position").length,
    0
  );
}

async function leverageDelegation(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey
): Promise<any | null> {
  const positions = await harness.positions(wallet, positionId);
  return positions.find((entry) => entry.eventType === "leverage_delegation")?.payload ?? null;
}

function u64Le(value: bigint): Buffer {
  const buffer = Buffer.alloc(8);
  buffer.writeBigUInt64LE(value);
  return buffer;
}

async function largestPassingMultiplier(
  harness: ProtocolTestHarness,
  debtAsset: "base" | "quote",
  positionId: PublicKey
): Promise<bigint> {
  let passing = 11_000n;
  let failing = 200_000n;
  const body = (multiplierBps: bigint) => ({
    positionId: positionId.toBase58(),
    debtAsset,
    marginAmount: "1",
    multiplierBps: multiplierBps.toString(),
    minCollateralOut: "0",
  });
  harness.assertTrue(
    `${debtAsset} 1.1x leverage lower bound succeeds`,
    (await harness.probe("alice", "/api/v2/fork/tx/open-leverage", body(passing))).succeeds
  );
  harness.assertTrue(
    `${debtAsset} 20x leverage is rejected by effective risk checks`,
    !(await harness.probe("alice", "/api/v2/fork/tx/open-leverage", body(failing))).succeeds
  );
  while (passing + 1n < failing) {
    const middle = (passing + failing) / 2n;
    if ((await harness.probe("alice", "/api/v2/fork/tx/open-leverage", body(middle))).succeeds) {
      passing = middle;
    } else {
      failing = middle;
    }
  }
  return passing;
}

async function largestPassingPositionMutation(
  harness: ProtocolTestHarness,
  endpoint: string,
  debtAsset: "base" | "quote",
  positionId: PublicKey,
  decimals: number,
  bodyForAmount: (amount: string) => Record<string, unknown>,
  lowerBound = 1n
): Promise<bigint> {
  let passing = lowerBound;
  let failing = raw(10_000, decimals);
  const body = (amount: bigint) => ({
    positionId: positionId.toBase58(),
    debtAsset,
    ...bodyForAmount(formatUnits(amount, decimals)),
  });
  harness.assertTrue(
    `${debtAsset} ${endpoint} lower search bound succeeds`,
    (await harness.probe("alice", endpoint, body(passing))).succeeds
  );
  harness.assertTrue(
    `${debtAsset} ${endpoint} upper bound fails`,
    !(await harness.probe("alice", endpoint, body(failing))).succeeds
  );
  while (passing + 1n < failing) {
    const middle = (passing + failing) / 2n;
    if ((await harness.probe("alice", endpoint, body(middle))).succeeds) passing = middle;
    else failing = middle;
  }
  return passing;
}

export const LEVERAGE_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "leverage.owner-lifecycle",
    async run(harness) {
      const marketBefore = await harness.market();
      const quoteSharesBefore = marketState(marketBefore, "isolatedQuoteDebt");
      const quotePrincipalBefore = marketState(marketBefore, "isolatedQuotePrincipal");
      const aliceQuoteBefore = await harness.tokenBalance(
        "alice",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open 2x quote-debt leverage",
        body: {
          positionId: quoteDebtPositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "10",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      let position = await leveragePosition(harness, "alice", quoteDebtPositionId);
      harness.assertEqual("quote-debt position records debt side", position.debtAsset, 1);
      harness.assertEqual(
        "open debits the requested quote margin",
        aliceQuoteBefore - await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram),
        raw(10, harness.config.quoteDecimals)
      );
      harness.assertEqual("2x open records requested principal", BigInt(position.debtPrincipal), raw(10, harness.config.quoteDecimals));
      harness.assertTrue("quote-debt open records debt shares", BigInt(position.debtShares) > 0n, position.debtShares);
      harness.assertTrue("quote-debt open acquires base collateral", BigInt(position.collateralAmount) > 0n, position.collateralAmount);
      harness.assertEqual("quote-debt open records multiplier", BigInt(position.multiplierBps), 20_000n);

      const sharesAfterOpen = BigInt(position.debtShares);
      const principalAfterOpen = BigInt(position.debtPrincipal);
      const quoteBeforeAddMargin = await harness.tokenBalance(
        "alice",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/add-leverage-margin",
        label: "repay quote leverage debt with added margin",
        body: { positionId: quoteDebtPositionId.toBase58(), debtAsset: "quote", amount: "1" },
      });
      position = await leveragePosition(harness, "alice", quoteDebtPositionId);
      harness.assertEqual(
        "add margin debits owner exactly",
        quoteBeforeAddMargin - await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram),
        raw(1, harness.config.quoteDecimals)
      );
      harness.assertTrue("add margin reduces debt shares", BigInt(position.debtShares) < sharesAfterOpen, position.debtShares);
      harness.assertTrue("add margin reduces principal", BigInt(position.debtPrincipal) < principalAfterOpen, position.debtPrincipal);

      const sharesAfterAddMargin = BigInt(position.debtShares);
      const principalAfterAddMargin = BigInt(position.debtPrincipal);
      const quoteBeforeRemoveMargin = await harness.tokenBalance(
        "alice",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/remove-leverage-margin",
        label: "borrow quote out of leverage margin",
        body: {
          positionId: quoteDebtPositionId.toBase58(),
          debtAsset: "quote",
          amount: "0.5",
          minAmountOut: "0.5",
        },
      });
      position = await leveragePosition(harness, "alice", quoteDebtPositionId);
      harness.assertEqual(
        "remove margin credits owner exactly",
        await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram) - quoteBeforeRemoveMargin,
        raw(1, harness.config.quoteDecimals) / 2n
      );
      harness.assertTrue("remove margin increases debt shares", BigInt(position.debtShares) > sharesAfterAddMargin, position.debtShares);
      harness.assertTrue("remove margin increases principal", BigInt(position.debtPrincipal) > principalAfterAddMargin, position.debtPrincipal);

      const collateralBeforeIncrease = BigInt(position.collateralAmount);
      const principalBeforeIncrease = BigInt(position.debtPrincipal);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/increase-leverage",
        label: "increase quote-debt leverage",
        body: {
          positionId: quoteDebtPositionId.toBase58(),
          debtAsset: "quote",
          debtAmount: "1",
          minCollateralOut: "0",
        },
      });
      position = await leveragePosition(harness, "alice", quoteDebtPositionId);
      harness.assertEqual(
        "increase adds requested principal",
        BigInt(position.debtPrincipal) - principalBeforeIncrease,
        raw(1, harness.config.quoteDecimals)
      );
      harness.assertTrue("increase acquires more collateral", BigInt(position.collateralAmount) > collateralBeforeIncrease, position.collateralAmount);

      const collateralBeforeRejectedDecrease = BigInt(position.collateralAmount);
      const sharesBeforeRejectedDecrease = BigInt(position.debtShares);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/decrease-leverage",
        label: "reject impossible leverage repay output",
        expected: "failure",
        body: {
          positionId: quoteDebtPositionId.toBase58(),
          debtAsset: "quote",
          collateralAmount: "0.25",
          minRepayOut: "1000000",
        },
      });
      position = await leveragePosition(harness, "alice", quoteDebtPositionId);
      harness.assertEqual("failed decrease preserves collateral", BigInt(position.collateralAmount), collateralBeforeRejectedDecrease);
      harness.assertEqual("failed decrease preserves debt shares", BigInt(position.debtShares), sharesBeforeRejectedDecrease);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/decrease-leverage",
        label: "sell collateral to decrease quote leverage",
        body: {
          positionId: quoteDebtPositionId.toBase58(),
          debtAsset: "quote",
          collateralAmount: "0.25",
          minRepayOut: "0",
        },
      });
      position = await leveragePosition(harness, "alice", quoteDebtPositionId);
      harness.assertEqual(
        "decrease debits exact collateral amount",
        collateralBeforeRejectedDecrease - BigInt(position.collateralAmount),
        raw(1, harness.config.baseDecimals) / 4n
      );
      harness.assertTrue("decrease reduces debt shares", BigInt(position.debtShares) < sharesBeforeRejectedDecrease, position.debtShares);

      const quoteBeforeClose = await harness.tokenBalance(
        "alice",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "close owner-controlled quote leverage",
        body: { positionId: quoteDebtPositionId.toBase58(), debtAsset: "quote", minAmountOut: "0" },
      });
      await assertLeveragePositionClosed(harness, "alice", quoteDebtPositionId, "quote leverage account closes");
      harness.assertTrue(
        "quote leverage close returns residual to owner",
        await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram) > quoteBeforeClose
      );
      let marketAfter = await harness.market();
      harness.assertEqual("quote isolated debt shares return to baseline", marketState(marketAfter, "isolatedQuoteDebt"), quoteSharesBefore);
      harness.assertEqual("quote isolated principal returns to baseline", marketState(marketAfter, "isolatedQuotePrincipal"), quotePrincipalBefore);

      const baseSharesBefore = marketState(marketAfter, "isolatedBaseDebt");
      const basePrincipalBefore = marketState(marketAfter, "isolatedBasePrincipal");
      const aliceBaseBefore = await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open mirrored 2x base-debt leverage",
        body: {
          positionId: baseDebtPositionId.toBase58(),
          debtAsset: "base",
          marginAmount: "5",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      position = await leveragePosition(harness, "alice", baseDebtPositionId);
      harness.assertEqual("base-debt position records debt side", position.debtAsset, 0);
      harness.assertEqual(
        "base-debt open debits requested margin",
        aliceBaseBefore - await harness.tokenBalance("alice", harness.config.baseMint, harness.config.baseTokenProgram),
        raw(5, harness.config.baseDecimals)
      );
      harness.assertEqual("base-debt open records requested principal", BigInt(position.debtPrincipal), raw(5, harness.config.baseDecimals));
      harness.assertTrue("base-debt open acquires quote collateral", BigInt(position.collateralAmount) > 0n, position.collateralAmount);
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "close mirrored base-debt leverage",
        body: { positionId: baseDebtPositionId.toBase58(), debtAsset: "base", minAmountOut: "0" },
      });
      await assertLeveragePositionClosed(harness, "alice", baseDebtPositionId, "base leverage account closes");
      marketAfter = await harness.market();
      harness.assertEqual("base isolated debt shares return to baseline", marketState(marketAfter, "isolatedBaseDebt"), baseSharesBefore);
      harness.assertEqual("base isolated principal returns to baseline", marketState(marketAfter, "isolatedBasePrincipal"), basePrincipalBefore);
    },
  },
  {
    id: "leverage.boundary-search",
    async run(harness) {
      for (const debtAsset of ["quote", "base"] as const) {
        const decimals = debtAsset === "base" ? harness.config.baseDecimals : harness.config.quoteDecimals;
        const multiplierPositionId = multiplierBoundaryPositionIds[debtAsset];
        const atOne = await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/open-leverage",
          label: `reject ${debtAsset} leverage at 1x`,
          expected: "failure",
          body: {
            positionId: multiplierPositionId.toBase58(),
            debtAsset,
            marginAmount: "1",
            multiplierBps: 10_000,
            minCollateralOut: "0",
          },
        });
        harness.assertEqual(`${debtAsset} 1x has deterministic zero-debt error`, atOne.errorCode, "AmountZero");
        const aboveHardCap = await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/open-leverage",
          label: `reject ${debtAsset} leverage above 20x hard cap`,
          expected: "failure",
          body: {
            positionId: multiplierPositionId.toBase58(),
            debtAsset,
            marginAmount: "1",
            multiplierBps: 200_001,
            minCollateralOut: "0",
          },
        });
        harness.assertEqual(
          `${debtAsset} hard multiplier cap has deterministic error`,
          aboveHardCap.errorCode,
          "LeverageMultiplierTooHigh"
        );

        const maximumMultiplier = await largestPassingMultiplier(
          harness,
          debtAsset,
          multiplierPositionId
        );
        harness.observe(`${debtAsset} maximum effective leverage multiplier bps`, maximumMultiplier);
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/open-leverage",
          label: `reject ${debtAsset} leverage one bps above effective maximum`,
          expected: "failure",
          body: {
            positionId: multiplierPositionId.toBase58(),
            debtAsset,
            marginAmount: "1",
            multiplierBps: (maximumMultiplier + 1n).toString(),
            minCollateralOut: "0",
          },
        });
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/open-leverage",
          label: `open ${debtAsset} leverage at effective maximum`,
          body: {
            positionId: multiplierPositionId.toBase58(),
            debtAsset,
            marginAmount: "1",
            multiplierBps: maximumMultiplier.toString(),
            minCollateralOut: "0",
          },
        });
        harness.assertEqual(
          `${debtAsset} position stores exact maximum multiplier`,
          BigInt((await leveragePosition(harness, "alice", multiplierPositionId)).multiplierBps),
          maximumMultiplier
        );
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/close-leverage",
          label: `close ${debtAsset} maximum-multiplier position`,
          body: { positionId: multiplierPositionId.toBase58(), debtAsset, minAmountOut: "0" },
        });
        await harness.timeTravel(0, 216_010);

        const marginPositionId = marginBoundaryPositionIds[debtAsset];
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/open-leverage",
          label: `open ${debtAsset} position for margin-removal boundary`,
          body: {
            positionId: marginPositionId.toBase58(),
            debtAsset,
            marginAmount: "50",
            multiplierBps: 20_000,
            minCollateralOut: "0",
          },
        });
        const maximumMarginRemoval = await largestPassingPositionMutation(
          harness,
          "/api/v2/fork/tx/remove-leverage-margin",
          debtAsset,
          marginPositionId,
          decimals,
          (amount) => ({ amount, minAmountOut: amount })
        );
        harness.observe(`${debtAsset} maximum removable leverage margin`, {
          raw: maximumMarginRemoval,
          ui: formatUnits(maximumMarginRemoval, decimals),
        });
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/remove-leverage-margin",
          label: `reject ${debtAsset} margin removal one raw unit above maximum`,
          expected: "failure",
          body: {
            positionId: marginPositionId.toBase58(),
            debtAsset,
            amount: formatUnits(maximumMarginRemoval + 1n, decimals),
            minAmountOut: formatUnits(maximumMarginRemoval + 1n, decimals),
          },
        });
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/remove-leverage-margin",
          label: `remove maximum safe ${debtAsset} leverage margin`,
          body: {
            positionId: marginPositionId.toBase58(),
            debtAsset,
            amount: formatUnits(maximumMarginRemoval, decimals),
            minAmountOut: formatUnits(maximumMarginRemoval, decimals),
          },
        });
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/close-leverage",
          label: `close ${debtAsset} margin-boundary position`,
          body: { positionId: marginPositionId.toBase58(), debtAsset, minAmountOut: "0" },
        });
        await harness.timeTravel(0, 216_010);

        const increasePositionId = increaseBoundaryPositionIds[debtAsset];
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/open-leverage",
          label: `open ${debtAsset} position for increase boundary`,
          body: {
            positionId: increasePositionId.toBase58(),
            debtAsset,
            marginAmount: "50",
            multiplierBps: 20_000,
            minCollateralOut: "0",
          },
        });
        const maximumIncrease = await largestPassingPositionMutation(
          harness,
          "/api/v2/fork/tx/increase-leverage",
          debtAsset,
          increasePositionId,
          decimals,
          (amount) => ({ debtAmount: amount, minCollateralOut: "0" }),
          raw(1, decimals)
        );
        harness.observe(`${debtAsset} maximum leverage debt increase`, {
          raw: maximumIncrease,
          ui: formatUnits(maximumIncrease, decimals),
        });
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/increase-leverage",
          label: `reject ${debtAsset} leverage increase one raw unit above maximum`,
          expected: "failure",
          body: {
            positionId: increasePositionId.toBase58(),
            debtAsset,
            debtAmount: formatUnits(maximumIncrease + 1n, decimals),
            minCollateralOut: "0",
          },
        });
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/increase-leverage",
          label: `execute maximum safe ${debtAsset} leverage increase`,
          body: {
            positionId: increasePositionId.toBase58(),
            debtAsset,
            debtAmount: formatUnits(maximumIncrease, decimals),
            minCollateralOut: "0",
          },
        });
        await harness.execute({
          wallet: "alice",
          endpoint: "/api/v2/fork/tx/close-leverage",
          label: `close ${debtAsset} increase-boundary position`,
          body: { positionId: increasePositionId.toBase58(), debtAsset, minAmountOut: "0" },
        });
        await harness.timeTravel(0, 216_010);
      }
    },
  },
  {
    id: "leverage.delegation-management",
    async run(harness) {
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open position for delegation management",
        body: {
          positionId: delegationPositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "5",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });

      const firstProgram = harness.wallet("bidder").publicKey;
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/create-leverage-delegation",
        label: "create close-only leverage delegation",
        body: {
          positionId: delegationPositionId.toBase58(),
          debtAsset: "quote",
          delegatedProgram: firstProgram.toBase58(),
          approvedActions: 1,
        },
      });
      let delegation = await leverageDelegation(harness, "alice", delegationPositionId);
      harness.assertTrue("delegation account is created", delegation !== null, delegation);
      harness.assertEqual("delegation records debt side", delegation.debtAsset, 1);
      harness.assertEqual("delegation records delegated program", delegation.delegatedProgram, firstProgram.toBase58());
      harness.assertEqual("delegation records close-only permission", delegation.approvedActions, 1);

      const updatedProgram = harness.wallet("liquidator").publicKey;
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/update-leverage-delegation",
        label: "reject delegation update from non-owner",
        expected: "failure",
        body: {
          positionId: delegationPositionId.toBase58(),
          debtAsset: "quote",
          delegatedProgram: updatedProgram.toBase58(),
          approvedActions: 7,
        },
      });
      delegation = await leverageDelegation(harness, "alice", delegationPositionId);
      harness.assertEqual("unauthorized update preserves delegated program", delegation.delegatedProgram, firstProgram.toBase58());
      harness.assertEqual("unauthorized update preserves permissions", delegation.approvedActions, 1);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-leverage-delegation",
        label: "update leverage delegation program and permissions",
        body: {
          positionId: delegationPositionId.toBase58(),
          debtAsset: "quote",
          delegatedProgram: updatedProgram.toBase58(),
          approvedActions: 7,
        },
      });
      delegation = await leverageDelegation(harness, "alice", delegationPositionId);
      harness.assertEqual("owner update changes delegated program", delegation.delegatedProgram, updatedProgram.toBase58());
      harness.assertEqual("owner update changes permission mask", delegation.approvedActions, 7);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage-delegation",
        label: "revoke leverage delegation",
        body: { positionId: delegationPositionId.toBase58() },
      });
      harness.assertEqual(
        "revocation closes delegation account",
        await leverageDelegation(harness, "alice", delegationPositionId),
        null
      );
      harness.assertTrue(
        "revoking delegation leaves leverage position open",
        (await leveragePosition(harness, "alice", delegationPositionId)) !== null
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "owner closes position after delegation revocation",
        body: { positionId: delegationPositionId.toBase58(), debtAsset: "quote", minAmountOut: "0" },
      });
      await assertLeveragePositionClosed(
        harness,
        "alice",
        delegationPositionId,
        "delegated position closes after revocation"
      );
    },
  },
  {
    id: "leverage.delegated-close",
    async run(harness) {
      const owner = harness.wallet("alice").publicKey;
      const executor = harness.wallet("bidder").publicKey;
      const duskProgram = new PublicKey(harness.config.programId);
      const market = new PublicKey(harness.config.market);
      const [leveragePositionAddress] = PublicKey.findProgramAddressSync(
        [Buffer.from("leverage_position_v2"), market.toBuffer(), delegatedClosePositionId.toBuffer()],
        duskProgram
      );
      const [leverageDelegationAddress] = PublicKey.findProgramAddressSync(
        [Buffer.from("leverage_delegation_v2"), leveragePositionAddress.toBuffer()],
        duskProgram
      );
      const orderId = 1n;
      const [orderAddress] = PublicKey.findProgramAddressSync(
        [Buffer.from("leverage_order"), leveragePositionAddress.toBuffer(), owner.toBuffer(), u64Le(orderId)],
        leverageDelegateProgramId
      );
      const [custodyAuthority] = PublicKey.findProgramAddressSync(
        [Buffer.from("leverage_delegate_authority"), orderAddress.toBuffer()],
        leverageDelegateProgramId
      );
      const custodyTokenAccount = getAssociatedTokenAddressSync(
        new PublicKey(harness.config.quoteMint),
        custodyAuthority,
        true,
        new PublicKey(harness.config.quoteTokenProgram)
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open position for delegated callback close",
        body: {
          positionId: delegatedClosePositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "5",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/create-leverage-delegation",
        label: "delegate close permission to callback program",
        body: {
          positionId: delegatedClosePositionId.toBase58(),
          debtAsset: "quote",
          delegatedProgram: leverageDelegateProgramId.toBase58(),
          approvedActions: 1,
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/create-leverage-order",
        label: "create initially untriggered take-profit order",
        body: {
          positionId: delegatedClosePositionId.toBase58(),
          orderId: orderId.toString(),
          kind: 1,
          triggerCloseoutPriceNad: "18446744073709551615",
        },
      });
      harness.assertTrue(
        "external leverage order account is created",
        (await harness.connection.getAccountInfo(orderAddress, "confirmed")) !== null
      );

      await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/delegated-close-leverage",
        label: "reject delegated close before trigger",
        expected: "failure",
        body: {
          positionOwner: owner.toBase58(),
          positionId: delegatedClosePositionId.toBase58(),
          debtAsset: "quote",
          orderId: orderId.toString(),
          minAmountOut: "0",
        },
      });
      harness.assertTrue(
        "failed trigger leaves leverage position open",
        (await leveragePosition(harness, "alice", delegatedClosePositionId)) !== null
      );
      harness.assertTrue(
        "failed trigger leaves order open",
        (await harness.connection.getAccountInfo(orderAddress, "confirmed")) !== null
      );

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/update-leverage-order",
        label: "lower take-profit trigger into executable range",
        body: {
          positionId: delegatedClosePositionId.toBase58(),
          orderId: orderId.toString(),
          kind: 1,
          triggerCloseoutPriceNad: "1",
        },
      });
      const ownerQuoteBefore = await harness.tokenBalance(
        "alice",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      const executorQuoteBefore = await harness.tokenBalance(
        "bidder",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/delegated-close-leverage",
        label: "execute delegated before-close-after callback settlement",
        body: {
          positionOwner: owner.toBase58(),
          positionId: delegatedClosePositionId.toBase58(),
          debtAsset: "quote",
          orderId: orderId.toString(),
          minAmountOut: "0",
        },
      });
      await assertLeveragePositionClosed(
        harness,
        "alice",
        delegatedClosePositionId,
        "delegated close removes leverage position"
      );
      harness.assertEqual(
        "after callback closes leverage order",
        await harness.connection.getAccountInfo(orderAddress, "confirmed"),
        null
      );
      harness.assertEqual(
        "after callback drains custody account",
        await harness.tokenAccountBalance(custodyTokenAccount, harness.config.quoteTokenProgram),
        0n
      );
      harness.assertTrue(
        "delegated close pays residual to position owner",
        await harness.tokenBalance("alice", harness.config.quoteMint, harness.config.quoteTokenProgram) > ownerQuoteBefore
      );
      harness.assertTrue(
        "delegated close pays executor incentive",
        await harness.tokenBalance("bidder", harness.config.quoteMint, harness.config.quoteTokenProgram) > executorQuoteBefore
      );
      harness.assertTrue(
        "delegation remains until owner revokes it",
        (await harness.connection.getAccountInfo(leverageDelegationAddress, "confirmed")) !== null
      );

      await harness.execute({
        wallet: "bidder",
        endpoint: "/api/v2/fork/tx/delegated-close-leverage",
        label: "reject stale delegated close after settlement",
        expected: "failure",
        body: {
          positionOwner: owner.toBase58(),
          positionId: delegatedClosePositionId.toBase58(),
          debtAsset: "quote",
          orderId: orderId.toString(),
          minAmountOut: "0",
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage-delegation",
        label: "revoke delegation after callback settlement",
        body: { positionId: delegatedClosePositionId.toBase58() },
      });
      harness.assertEqual(
        "post-settlement delegation account closes",
        await harness.connection.getAccountInfo(leverageDelegationAddress, "confirmed"),
        null
      );
    },
  },
  {
    id: "leverage.liquidation",
    async run(harness) {
      const positionOwner = harness.wallet("bob").publicKey;
      const marketBefore = await harness.market();
      const isolatedSharesBefore = marketState(marketBefore, "isolatedQuoteDebt");
      const isolatedPrincipalBefore = marketState(marketBefore, "isolatedQuotePrincipal");

      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open healthy quote-debt position for liquidation",
        body: {
          positionId: liquidationPositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "10",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/liquidate-leverage",
        label: "reject liquidation of healthy leverage",
        expected: "failure",
        body: {
          positionOwner: positionOwner.toBase58(),
          positionId: liquidationPositionId.toBase58(),
          debtAsset: "quote",
        },
      });
      harness.assertTrue(
        "healthy liquidation rejection leaves position open",
        (await leveragePosition(harness, "bob", liquidationPositionId)) !== null
      );

      await harness.fundWallet("trader", "100000", "100000");
      const stateBeforeShock = await harness.market();
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "move base price down through a real large swap",
        body: { assetIn: "base", exactAssetIn: "80000", minAssetOut: "0" },
      });
      const stateAfterShock = await harness.market();
      harness.assertTrue(
        "price shock increases base inventory",
        marketState(stateAfterShock, "baseReserve") > marketState(stateBeforeShock, "baseReserve")
      );
      harness.assertTrue(
        "price shock drains quote inventory",
        marketState(stateAfterShock, "quoteReserve") < marketState(stateBeforeShock, "quoteReserve")
      );

      const liquidatorQuoteBefore = await harness.tokenBalance(
        "liquidator",
        harness.config.quoteMint,
        harness.config.quoteTokenProgram
      );
      await harness.execute({
        wallet: "liquidator",
        endpoint: "/api/v2/fork/tx/liquidate-leverage",
        label: "liquidate leverage after collateral price shock",
        body: {
          positionOwner: positionOwner.toBase58(),
          positionId: liquidationPositionId.toBase58(),
          debtAsset: "quote",
        },
      });
      await assertLeveragePositionClosed(
        harness,
        "bob",
        liquidationPositionId,
        "liquidation closes unhealthy leverage position"
      );
      harness.assertTrue(
        "liquidator debt-token balance does not decrease",
        await harness.tokenBalance("liquidator", harness.config.quoteMint, harness.config.quoteTokenProgram) >= liquidatorQuoteBefore
      );
      const stateAfterLiquidation = await harness.market();
      harness.assertEqual(
        "liquidation clears isolated quote debt shares",
        marketState(stateAfterLiquidation, "isolatedQuoteDebt"),
        isolatedSharesBefore
      );
      harness.assertEqual(
        "liquidation clears isolated quote principal",
        marketState(stateAfterLiquidation, "isolatedQuotePrincipal"),
        isolatedPrincipalBefore
      );

      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "partially restore pool ratio after liquidation test",
        body: { assetIn: "quote", exactAssetIn: "44000", minAssetOut: "0" },
      });
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "checkpoint restored spot price for EMA recovery",
        body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      await harness.timeTravel(1, 1_000);
      await harness.execute({
        wallet: "trader",
        endpoint: "/api/v2/fork/tx/swap",
        label: "persist restored EMA after slot advancement",
        body: { assetIn: "quote", exactAssetIn: "0.001", minAssetOut: "0" },
      });
      const stateAfterRestore = await harness.market();
      harness.assertTrue(
        "offsetting swap reduces base inventory",
        marketState(stateAfterRestore, "baseReserve") < marketState(stateAfterLiquidation, "baseReserve")
      );
      harness.assertTrue(
        "offsetting swap restores quote inventory",
        marketState(stateAfterRestore, "quoteReserve") > marketState(stateAfterLiquidation, "quoteReserve")
      );
    },
  },
];
