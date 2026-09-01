import {
  TOKEN_PROGRAMS,
  createMintIfMissing,
  deriveFaucetAuthorityAddress,
  duskEnv,
  faucetProgramId,
  providerFromEnv,
  payerFromProvider,
  readState,
  writeState,
} from "./common.ts";

async function main() {
  const provider = providerFromEnv();
  const payer = payerFromProvider(provider);
  const state = readState();
  const tokenProgram =
    duskEnv("TOKEN_PROGRAM") === "token2022"
      ? TOKEN_PROGRAMS.token2022
      : TOKEN_PROGRAMS.token;
  const decimals = Number(duskEnv("MOCK_DECIMALS") ?? "6");
  const baseLabel = duskEnv("MOCK_BASE_LABEL") ?? "base";
  const quoteLabel = duskEnv("MOCK_QUOTE_LABEL") ?? "quote";
  const faucetId = faucetProgramId();
  const faucetAccount = await provider.connection.getAccountInfo(faucetId, "confirmed");
  if (!faucetAccount?.executable) {
    throw new Error(
      `Dusk faucet ${faucetId.toBase58()} is not deployed. Run yarn v2:deploy-faucet-devnet first.`
    );
  }
  const faucetAuthority = deriveFaucetAuthorityAddress(faucetId);

  const baseMint = await createMintIfMissing({
    connection: provider.connection,
    payer,
    label: baseLabel,
    decimals,
    mintAuthority: faucetAuthority,
    tokenProgram,
  });
  const quoteMint = await createMintIfMissing({
    connection: provider.connection,
    payer,
    label: quoteLabel,
    decimals,
    mintAuthority: faucetAuthority,
    tokenProgram,
  });

  state.faucet = {
    programId: faucetId.toBase58(),
    mintAuthority: faucetAuthority.toBase58(),
  };
  state.mockMints[baseLabel] = baseMint;
  state.mockMints[quoteLabel] = quoteMint;
  writeState(state);

  console.log("Dusk mock mints ready");
  console.log(`State: ${duskEnv("DEVNET_STATE") ?? "default"}`);
  console.log(`${baseLabel}: ${baseMint.mint}`);
  console.log(`${quoteLabel}: ${quoteMint.mint}`);
  console.log(`Token program: ${tokenProgram.toBase58()}`);
  console.log(`Faucet: ${faucetId.toBase58()}`);
  console.log(`Mint authority: ${faucetAuthority.toBase58()}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
