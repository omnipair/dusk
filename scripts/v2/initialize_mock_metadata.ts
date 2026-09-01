import anchor from "@coral-xyz/anchor";
import { Metadata } from "@metaplex-foundation/mpl-token-metadata";
import { PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY, SystemProgram } from "@solana/web3.js";
import {
  TOKEN_METADATA_PROGRAM_ID,
  defaultMockMetadata,
  deriveFaucetAuthorityAddress,
  deriveTokenMetadataAddress,
  explorerTx,
  faucetProgram,
  faucetProgramId,
  payerFromProvider,
  providerFromEnv,
  readState,
  writeState,
} from "./common.ts";

function clean(value: string): string {
  return value.replace(/\0/g, "").trim();
}

async function ensureMetadata(params: {
  provider: anchor.AnchorProvider;
  payer: ReturnType<typeof payerFromProvider>;
  mint: PublicKey;
  tokenProgram: PublicKey;
  kind: "base" | "quote";
}) {
  const expected = defaultMockMetadata(params.kind);
  const address = deriveTokenMetadataAddress(params.mint);
  const existing = await params.provider.connection.getAccountInfo(address, "confirmed");

  if (!existing) {
    const faucet = faucetProgram(params.provider.connection, params.payer);
    const signature = await faucet.methods
      .initializeMintMetadata(expected)
      .accounts({
        payer: params.payer.publicKey,
        faucetAuthority: deriveFaucetAuthorityAddress(faucetProgramId()),
        mint: params.mint,
        metadata: address,
        systemProgram: SystemProgram.programId,
        sysvarInstructions: SYSVAR_INSTRUCTIONS_PUBKEY,
        tokenProgram: params.tokenProgram,
        tokenMetadataProgram: TOKEN_METADATA_PROGRAM_ID,
      })
      .rpc();
    console.log(`${expected.symbol} metadata tx: ${explorerTx(signature)}`);
  }

  const metadata = await Metadata.fromAccountAddress(
    params.provider.connection,
    address,
    "confirmed"
  );
  const actual = {
    name: clean(metadata.data.name),
    symbol: clean(metadata.data.symbol),
    uri: clean(metadata.data.uri),
  };
  if (
    actual.name !== expected.name ||
    actual.symbol !== expected.symbol ||
    actual.uri !== expected.uri
  ) {
    throw new Error(
      `Metadata mismatch for ${params.mint.toBase58()}: expected ` +
        `${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`
    );
  }

  return { address: address.toBase58(), ...actual };
}

async function main() {
  const provider = providerFromEnv();
  const payer = payerFromProvider(provider);
  const state = readState();
  const base = state.mockMints.base;
  const quote = state.mockMints.quote;
  if (!base || !quote) {
    throw new Error("Base and quote mock mints are missing. Run yarn v2:create-mock-tokens first.");
  }

  base.metadata = await ensureMetadata({
    provider,
    payer,
    mint: new PublicKey(base.mint),
    tokenProgram: new PublicKey(base.tokenProgram),
    kind: "base",
  });
  quote.metadata = await ensureMetadata({
    provider,
    payer,
    mint: new PublicKey(quote.mint),
    tokenProgram: new PublicKey(quote.tokenProgram),
    kind: "quote",
  });
  writeState(state);

  console.log(`META: ${base.mint} (${base.decimals} decimals)`);
  console.log(`USDC: ${quote.mint} (${quote.decimals} decimals)`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
