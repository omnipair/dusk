import { duskProgram, ensureFutarchyAuthority, payerFromProvider, providerFromEnv } from "./common.ts";
import idl from "../../target/idl/dusk.json" with { type: "json" };

// Runs after every devnet deploy. An in-place upgrade already has the
// futarchy authority; a deploy to a new program ID gets it created here.
async function main() {
  const provider = providerFromEnv();
  const program = duskProgram(idl, provider);
  const authority = await ensureFutarchyAuthority({
    program,
    payer: payerFromProvider(provider).publicKey,
  });
  console.log(`Futarchy authority ready; team treasury ${authority.recipients.teamTreasury.toBase58()}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
