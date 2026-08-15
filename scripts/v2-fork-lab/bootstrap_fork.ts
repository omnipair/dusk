import { bootstrapForkMarkets, shutdownForkRuntime } from "./api_core.js";

try {
  const markets = await bootstrapForkMarkets();
  console.log(
    `Surfpool bootstrap controller prepared ${markets.length} market(s): ${markets
      .map((market) => `${market.label}=${market.market}`)
      .join(", ")}`,
  );
} finally {
  shutdownForkRuntime();
}
