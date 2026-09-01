import { Keypair, PublicKey, Transaction } from "@solana/web3.js";

import type { ProtocolTestHarness, ScenarioDefinition } from "../harness.js";

const substitutionPositionId = Keypair.generate().publicKey;
const ownershipPositionId = Keypair.generate().publicKey;
const ownershipLeveragePositionId = Keypair.generate().publicKey;

function duskInstruction(harness: ProtocolTestHarness, transaction: Transaction) {
  const programId = new PublicKey(harness.config.programId);
  const instruction = transaction.instructions.find((candidate) => candidate.programId.equals(programId));
  if (!instruction) throw new Error("Built transaction does not contain a Dusk instruction");
  return instruction;
}

function replaceAccount(
  harness: ProtocolTestHarness,
  transaction: Transaction,
  from: string,
  to: PublicKey
): void {
  const instruction = duskInstruction(harness, transaction);
  const meta = instruction.keys.find((candidate) => candidate.pubkey.equals(new PublicKey(from)));
  if (!meta) throw new Error(`Dusk transaction does not contain account ${from}`);
  meta.pubkey = to;
}

async function positionShares(
  harness: ProtocolTestHarness,
  wallet: string,
  positionId: PublicKey,
  key: "fixedBaseShares" | "fixedQuoteShares"
): Promise<bigint> {
  const position = (await harness.positions(wallet, positionId)).find(
    (entry) => entry.eventType === "borrow_position"
  );
  if (!position) throw new Error(`Borrow position ${positionId.toBase58()} was not found`);
  return BigInt(position.payload[key]);
}

export const SECURITY_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "security.account-substitution",
    async run(harness) {
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "create collateralized position for account substitution",
        body: {
          positionId: substitutionPositionId.toBase58(),
          marketAsset: "base",
          depositAmount: "50",
        },
      });

      const market = await harness.market();
      const substitutions: Array<[string, string, PublicKey]> = [
        ["reserve vault", market.quoteReserveVault, new PublicKey(market.baseReserveVault)],
        ["debt mint", market.quoteMint, new PublicKey(market.baseMint)],
        ["borrow position PDA", "position", harness.wallet("bob").publicKey],
        ["legacy token program", "11111111111111111111111111111111", harness.wallet("bob").publicKey],
      ];

      for (const [label, source, replacement] of substitutions) {
        const transaction = await harness.buildSignedTransaction(
          "alice",
          "/api/v2/fork/tx/borrow",
          {
            positionId: substitutionPositionId.toBase58(),
            borrowAsset: "quote",
            borrowAmount: "1",
            minDebtAmountOut: "1",
            minLiquidationCfBps: 0,
          }
        );
        if (source === "position") {
          const instruction = duskInstruction(harness, transaction);
          const marketKey = new PublicKey(harness.config.market);
          const [positionAddress] = PublicKey.findProgramAddressSync(
            [Buffer.from("borrow_position_v2"), marketKey.toBuffer(), substitutionPositionId.toBuffer()],
            new PublicKey(harness.config.programId)
          );
          const meta = instruction.keys.find((candidate) => candidate.pubkey.equals(positionAddress));
          if (!meta) throw new Error("Borrow transaction does not contain its position PDA");
          meta.pubkey = replacement;
        } else if (label === "legacy token program") {
          const instruction = duskInstruction(harness, transaction);
          const meta = instruction.keys.find((candidate) =>
            candidate.pubkey.equals(new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"))
          );
          if (!meta) throw new Error("Borrow transaction does not contain the legacy token program");
          meta.pubkey = replacement;
        } else {
          replaceAccount(harness, transaction, source, replacement);
        }
        transaction.sign(harness.wallet("alice"));
        await harness.executeBuiltTransaction({
          wallet: "alice",
          label: `reject substituted ${label}`,
          transaction,
          expected: "failure",
        });
        harness.assertEqual(
          `${label} substitution cannot create debt`,
          await positionShares(harness, "alice", substitutionPositionId, "fixedQuoteShares"),
          0n
        );
      }

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "execute borrow with canonical accounts after substitution probes",
        body: {
          positionId: substitutionPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/repay",
        label: "repay account-substitution fixture debt",
        body: {
          positionId: substitutionPositionId.toBase58(),
          repayAsset: "quote",
          repayAmount: "1",
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "close account-substitution fixture position",
        body: {
          positionId: substitutionPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: "50",
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });
    },
  },
  {
    id: "security.signer-and-ownership",
    async run(harness) {
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/deposit-collateral",
        label: "create Alice-owned position for authorization probes",
        body: {
          positionId: ownershipPositionId.toBase58(),
          marketAsset: "base",
          depositAmount: "10",
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/borrow",
        label: "reject Bob borrowing from Alice position",
        expected: "failure",
        body: {
          positionId: ownershipPositionId.toBase58(),
          borrowAsset: "quote",
          borrowAmount: "1",
          minDebtAmountOut: "1",
          minLiquidationCfBps: 0,
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "reject Bob withdrawing Alice collateral",
        expected: "failure",
        body: {
          positionId: ownershipPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: "1",
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });

      const readOnlyOwner = await harness.buildSignedTransaction(
        "alice",
        "/api/v2/fork/tx/withdraw-collateral",
        {
          positionId: ownershipPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: "1",
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        }
      );
      const ownerMeta = duskInstruction(harness, readOnlyOwner).keys.find((candidate) =>
        candidate.pubkey.equals(harness.wallet("alice").publicKey)
      );
      if (!ownerMeta) throw new Error("Withdraw transaction does not contain owner account");
      ownerMeta.isSigner = false;
      readOnlyOwner.feePayer = harness.wallet("bob").publicKey;
      readOnlyOwner.sign(harness.wallet("bob"));
      await harness.executeBuiltTransaction({
        wallet: "bob",
        label: "reject read-only owner account in signer position",
        transaction: readOnlyOwner,
        expected: "failure",
      });

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/withdraw-collateral",
        label: "Alice withdraws her authorization fixture collateral",
        body: {
          positionId: ownershipPositionId.toBase58(),
          marketAsset: "base",
          withdrawAmount: "10",
          minAssetAmountOut: "0",
          minLiquidationCfBps: 0,
        },
      });

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/open-leverage",
        label: "open Alice leverage for owner-binding probe",
        body: {
          positionId: ownershipLeveragePositionId.toBase58(),
          debtAsset: "quote",
          marginAmount: "2",
          multiplierBps: 20_000,
          minCollateralOut: "0",
        },
      });
      await harness.execute({
        wallet: "bob",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "reject Bob closing Alice leverage",
        expected: "failure",
        body: {
          positionId: ownershipLeveragePositionId.toBase58(),
          debtAsset: "quote",
          minAmountOut: "0",
        },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/close-leverage",
        label: "Alice closes owner-binding leverage fixture",
        body: {
          positionId: ownershipLeveragePositionId.toBase58(),
          debtAsset: "quote",
          minAmountOut: "0",
        },
      });
    },
  },
];
