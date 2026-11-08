/** Inspect all current markets; DUSK_APPLY=1 repairs mismatched LP metadata.
 * Uses the existing mints and the market operator wallet. Never creates pools. */
import { Metadata } from "@metaplex-foundation/mpl-token-metadata";
import { PublicKey, Transaction } from "@solana/web3.js";
import { DuskWrite, marketLpTokenNaming } from "../../packages/dusk-sdk/dist/index.js";
import { readTokenMetadata } from "../lp-metadata/logos.ts";
import { pinataConfigFromEnv } from "../lp-metadata/pinata.ts";
import { publishLpMetadata } from "../lp-metadata/publish.ts";
import { deriveTokenMetadataAddress, duskEnv, duskProgram, explorerTx, payerFromProvider, providerFromEnv } from "./common.ts";
import idl from "../../target/idl/dusk.json" with { type: "json" };

async function main() {
  const provider = providerFromEnv(), payer = payerFromProvider(provider);
  const program = duskProgram(idl, provider), write = new DuskWrite(program);
  const selected = duskEnv("MARKETS")?.split(",").map(value => value.trim());
  const markets = await program.account.market.all();
  if (selected?.some(key => !markets.some(row => row.publicKey.toBase58() === key))) {
    throw new Error("DUSK_MARKETS contains an address that is not a current Dusk market");
  }
  const plans = [];
  for (const { publicKey: market, account: state } of markets) {
    if (selected && !selected.includes(market.toBase58())) continue;
    const baseMint = state.baseSide.assetMint, quoteMint = state.quoteSide.assetMint;
    const [base, quote] = await Promise.all([
      readTokenMetadata(provider.connection, baseMint), readTokenMetadata(provider.connection, quoteMint),
    ]);
    if (!base.symbol || !quote.symbol) throw new Error(`Missing asset ticker for ${market.toBase58()}`);
    const naming = marketLpTokenNaming({ baseSymbol: base.symbol, quoteSymbol: quote.symbol });
    const mints = { srlp: state.srlpMint, baseHlp: state.baseSide.hlpMint, quoteHlp: state.quoteSide.hlpMint,
      ...(state.juniorMint.equals(PublicKey.default) ? {} : { junior: state.juniorMint }) };
    const changes = [];
    for (const kind of Object.keys(mints) as Array<keyof typeof mints>) {
      const existing = await Metadata.fromAccountAddress(provider.connection, deriveTokenMetadataAddress(mints[kind]));
      const clean = (value: string) => value.replace(/\0+$/g, "").trim();
      if (clean(existing.data.name) === naming[kind].name && clean(existing.data.symbol) === naming[kind].symbol) continue;
      if (!existing.isMutable || !existing.updateAuthority.equals(market)) throw new Error(`Cannot repair ${mints[kind]}`);
      changes.push({ kind, oldName: clean(existing.data.name), oldSymbol: clean(existing.data.symbol) });
    }
    plans.push({ market, state, baseMint, quoteMint, baseSymbol: base.symbol, quoteSymbol: quote.symbol, naming, mints, changes });
    console.log(JSON.stringify({ market: market.toBase58(), operator: state.operator.authority.toBase58(), changes: changes.map(change => ({ ...change, mint: mints[change.kind].toBase58(), name: naming[change.kind].name, symbol: naming[change.kind].symbol })) }));
  }
  if (duskEnv("APPLY") !== "1") { console.log("Inspection only; set DUSK_APPLY=1 to publish metadata and apply these corrections."); return; }
  // Check every required signature before any upload or transaction.
  for (const plan of plans.filter(plan => plan.changes.length)) {
    if (!plan.state.operator.authority.equals(payer.publicKey)) throw new Error(`Operator wallet required for ${plan.market.toBase58()}`);
  }
  const changed = plans.filter(plan => plan.changes.length);
  if (!changed.length) { console.log("All LP names and symbols already match their asset tickers."); return; }
  const pinata = pinataConfigFromEnv();
  for (const plan of changed) {
    const published = await publishLpMetadata({ connection: provider.connection, network: "devnet", ...plan, pinata });
    for (const { kind } of plan.changes) {
      const metadata = { name: plan.naming[kind].name, symbol: plan.naming[kind].symbol, uri: published[kind]!.uri };
      const lpMint = plan.mints[kind];
      const instruction = await write.updateLpMetadataInstruction({ market: plan.market, operator: payer.publicKey, lpMint, metadata });
      const metadataAddress = deriveTokenMetadataAddress(lpMint);
      if (instruction.keys.some(key => key.isWritable && !key.pubkey.equals(metadataAddress))) throw new Error("Unexpected writable account in metadata correction");
      const signature = await provider.sendAndConfirm(new Transaction().add(instruction), []);
      const actual = await Metadata.fromAccountAddress(provider.connection, metadataAddress);
      for (const field of ["name", "symbol", "uri"] as const) {
        if (actual.data[field].replace(/\0+$/g, "") !== metadata[field]) throw new Error(`Metadata read-back mismatch for ${lpMint}`);
      }
      if (!actual.updateAuthority.equals(plan.market)) throw new Error("Metadata authority changed");
      console.log(`${kind}: ${metadata.name} (${metadata.symbol}) — ${explorerTx(signature)}`);
    }
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
