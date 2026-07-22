import { PublicKey } from "@solana/web3.js";

import type { ProtocolTestHarness, ScenarioDefinition } from "../harness.js";

async function recordBootstrapInstructions(
  harness: ProtocolTestHarness,
  instructionName: string
): Promise<number> {
  const transactions = (await harness.bootstrapEvidence()).filter((entry) =>
    entry.instructions.includes(instructionName)
  );
  for (const transaction of transactions) {
    const evidence = await harness.recordConfirmedSignature(transaction.label, transaction.signature);
    harness.assertTrue(
      `${transaction.label} contains ${instructionName}`,
      evidence.instructions.includes(instructionName),
      evidence.instructions
    );
  }
  return transactions.length;
}

export const BOOTSTRAP_SCENARIOS: ScenarioDefinition[] = [
  {
    id: "bootstrap.authority-and-market",
    fatal: true,
    async run(harness) {
      harness.assertEqual(
        "one futarchy authority initialization transaction is captured",
        await recordBootstrapInstructions(harness, "init_futarchy_authority"),
        1
      );
      harness.assertEqual(
        "one market initialization transaction is captured",
        await recordBootstrapInstructions(harness, "initialize"),
        1
      );
      const market = await harness.market();
      const futarchy = await harness.futarchy();
      harness.assertEqual("initialized market matches fork config", market.marketAddress, harness.config.market);
      harness.assertEqual("initialized authority account has current version", futarchy.version, 3);

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/bootstrap-rejection",
        label: "reject duplicate futarchy authority initialization",
        expected: "failure",
        apiSigned: true,
        body: { kind: "futarchy-duplicate" },
      });
      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/bootstrap-rejection",
        label: "reject duplicate market initialization",
        expected: "failure",
        apiSigned: true,
        body: { kind: "market-duplicate" },
      });
      const invalidConfig = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/bootstrap-rejection",
        label: "reject fresh market initialization with invalid config",
        expected: "failure",
        apiSigned: true,
        body: { kind: "market-invalid-config" },
      });
      harness.assertEqual(
        "invalid bootstrap market config reaches protocol validation",
        invalidConfig.errorCode,
        "InvalidSwapFeeBps"
      );
      harness.assertEqual("bootstrap rejections preserve primary market", (await harness.market()).marketAddress, harness.config.market);
    },
  },
  {
    id: "bootstrap.lp-metadata",
    fatal: true,
    async run(harness) {
      harness.assertEqual(
        "all three LP metadata initialization transactions are captured",
        await recordBootstrapInstructions(harness, "initialize_lp_metadata"),
        3
      );
      const market = await harness.market();
      const metadataAddresses = [
        market.ylpMint,
        market.baseHlpMint,
        market.quoteHlpMint,
      ].map((mint) => PublicKey.findProgramAddressSync(
        [
          Buffer.from("metadata"),
          new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s").toBuffer(),
          new PublicKey(mint).toBuffer(),
        ],
        new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s")
      )[0]);
      for (const address of metadataAddresses) {
        harness.assertTrue(
          `LP metadata account ${address.toBase58()} exists`,
          (await harness.connection.getAccountInfo(address, "confirmed")) !== null
        );
      }

      await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/bootstrap-rejection",
        label: "reject duplicate yLP metadata initialization",
        expected: "failure",
        apiSigned: true,
        body: { kind: "metadata-duplicate" },
      });
      const invalidName = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/bootstrap-rejection",
        label: "reject overlong LP metadata name",
        expected: "failure",
        apiSigned: true,
        body: { kind: "metadata-invalid-name" },
      });
      harness.assertEqual("invalid metadata name is rejected deterministically", invalidName.errorCode, "InvalidLpName");
      const mismatchedMint = await harness.execute({
        wallet: "alice",
        endpoint: "/api/v2/fork/tx/bootstrap-rejection",
        label: "reject pool asset mint as LP metadata mint",
        expected: "failure",
        apiSigned: true,
        body: { kind: "metadata-mismatched-mint" },
      });
      harness.assertEqual(
        "mismatched metadata mint is rejected deterministically",
        mismatchedMint.errorCode,
        "InvalidLpMintKey"
      );
    },
  },
];
