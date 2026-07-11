import { addressString, type AddressLike } from "./address.js";

export const DEFAULT_INDEXER_BASE_URL = "https://api.indexer.omnipair.fi/api/v1";

export type FetchLike = typeof fetch;
export type ActivityCategory = "swaps" | "liquidity" | "lending";
export type ActivitySort = "recent" | "oldest";
export type HistoryRange = "1H" | "2H" | "4H" | "12H" | "24H" | "7D" | "30D";
export type PortfolioRange = "7D" | "30D" | "90D" | "ALL";
export type Visibility = "visible" | "all";

export interface ApiResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
  message?: string;
}

export interface RequestOptions {
  params?: Record<string, string | number | boolean | undefined | null>;
  signal?: AbortSignal;
  headers?: HeadersInit;
}

export interface PaginationOptions {
  limit?: number;
  offset?: number;
}

export interface PoolListOptions extends PaginationOptions {
  token0?: AddressLike;
  token1?: AddressLike;
  sortBy?: string;
  sortOrder?: "asc" | "desc" | string;
}

export interface WindowHoursOptions {
  windowHours?: number;
}

export interface CandleOptions {
  resolution: number;
  from: number;
  to: number;
}

export interface ActivityOptions extends PaginationOptions {
  categories?: ActivityCategory[];
  poolAddress?: AddressLike;
  sort?: ActivitySort;
}

export interface PositionOptions extends PaginationOptions {
  userAddress?: AddressLike;
  poolAddress?: AddressLike;
  type?: "liquidity" | "lending" | "borrow" | "long" | "short" | "all" | string;
  status?: "open" | "closed" | "all" | string;
}

export class DuskIndexerError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly body: unknown
  ) {
    super(message);
    this.name = "DuskIndexerError";
  }
}

export class DuskIndexerClient {
  readonly baseUrl: string;
  private readonly fetchImpl: FetchLike;

  constructor(options: { baseUrl?: string; fetch?: FetchLike } = {}) {
    this.baseUrl = normalizeBaseUrl(options.baseUrl ?? DEFAULT_INDEXER_BASE_URL);
    this.fetchImpl = options.fetch ?? globalThis.fetch;
    if (!this.fetchImpl) {
      throw new Error("Dusk indexer client requires a fetch implementation.");
    }
  }

  async request<T = unknown>(path: string, options: RequestOptions = {}): Promise<T> {
    const response = await this.fetchImpl(this.url(path, options.params), {
      headers: options.headers,
      signal: options.signal,
    });
    const body = await readResponseBody(response);

    if (!response.ok) {
      throw new DuskIndexerError(
        `Dusk indexer request failed with HTTP ${response.status}`,
        response.status,
        body
      );
    }

    return body as T;
  }

  url(path: string, params?: RequestOptions["params"]): string {
    const url = new URL(path.replace(/^\/+/, ""), `${this.baseUrl}/`);
    for (const [key, value] of Object.entries(params ?? {})) {
      if (value !== undefined && value !== null) {
        url.searchParams.set(key, String(value));
      }
    }
    return url.toString();
  }

  stats<T = unknown>(options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/stats", options);
  }

  volumeChart<T = unknown>(timeframe?: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/stats/volume-chart", withParams(options, { timeframe }));
  }

  feesChart<T = unknown>(timeframe?: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/stats/fees-chart", withParams(options, { timeframe }));
  }

  interestChart<T = unknown>(timeframe?: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/stats/interest-chart", withParams(options, { timeframe }));
  }

  swapCountChart<T = unknown>(timeframe?: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/stats/swap-count-chart", withParams(options, { timeframe }));
  }

  pools<T = unknown>(params: PoolListOptions = {}, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/pools", withParams(options, serializeParams(params)));
  }

  pool<T = unknown>(poolAddress: AddressLike, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request(`/pools/${addressString(poolAddress)}`, options);
  }

  poolTvl<T = unknown>(options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/pools/tvl", options);
  }

  poolValueBaselines<T = unknown>(
    params: { range?: HistoryRange; visibility?: Visibility } = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request("/pools/value-baselines", withParams(options, params));
  }

  pairedTokens<T = unknown>(
    tokenAddress: AddressLike,
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(`/pools/paired-tokens/${addressString(tokenAddress)}`, options);
  }

  poolStats<T = unknown>(
    poolAddress: AddressLike,
    params: WindowHoursOptions = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(`/pools/${addressString(poolAddress)}/stats`, withParams(options, params));
  }

  poolRateHistory<T = unknown>(
    poolAddress: AddressLike,
    params: { range?: HistoryRange } = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/pools/${addressString(poolAddress)}/rate-history`,
      withParams(options, params)
    );
  }

  poolVolume<T = unknown>(
    poolAddress: AddressLike,
    params: WindowHoursOptions = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(`/pools/${addressString(poolAddress)}/volume`, withParams(options, params));
  }

  poolFees<T = unknown>(
    poolAddress: AddressLike,
    params: WindowHoursOptions = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(`/pools/${addressString(poolAddress)}/fees`, withParams(options, params));
  }

  poolPriceChart<T = unknown>(
    poolAddress: AddressLike,
    params: WindowHoursOptions = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/pools/${addressString(poolAddress)}/price-chart`,
      withParams(options, params)
    );
  }

  poolCandles<T = unknown>(
    poolAddress: AddressLike,
    params: CandleOptions,
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(`/pools/${addressString(poolAddress)}/candles`, withParams(options, params));
  }

  poolSwaps<T = unknown>(
    poolAddress: AddressLike,
    params: PaginationOptions = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(`/pools/${addressString(poolAddress)}/swaps`, withParams(options, params));
  }

  poolActivity<T = unknown>(
    poolAddress: AddressLike,
    params: Omit<ActivityOptions, "poolAddress"> = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/pools/${addressString(poolAddress)}/activity`,
      withParams(options, serializeActivityParams(params))
    );
  }

  poolLiquidityEvents<T = unknown>(
    poolAddress: AddressLike,
    params: { userAddress: AddressLike } & PaginationOptions,
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/pools/${addressString(poolAddress)}/liquidity-events`,
      withParams(options, serializeParams(params))
    );
  }

  userSwaps<T = unknown>(
    userAddress: AddressLike,
    params: PaginationOptions & { poolAddress?: AddressLike } = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(`/users/${addressString(userAddress)}/swaps`, withParams(options, serializeParams(params)));
  }

  userLiquidityEvents<T = unknown>(
    userAddress: AddressLike,
    params: { poolAddress: AddressLike } & PaginationOptions,
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/users/${addressString(userAddress)}/liquidity-events`,
      withParams(options, serializeParams(params))
    );
  }

  userLendingEvents<T = unknown>(
    userAddress: AddressLike,
    params: PaginationOptions & { poolAddress?: AddressLike } = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/users/${addressString(userAddress)}/lending-events`,
      withParams(options, serializeParams(params))
    );
  }

  userActivity<T = unknown>(
    userAddress: AddressLike,
    params: ActivityOptions = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/users/${addressString(userAddress)}/activity`,
      withParams(options, serializeActivityParams(params))
    );
  }

  userPortfolioSnapshots<T = unknown>(
    userAddress: AddressLike,
    range: PortfolioRange,
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/users/${addressString(userAddress)}/portfolio-snapshots`,
      withParams(options, { range })
    );
  }

  userLpEarnings<T = unknown>(
    userAddress: AddressLike,
    params: { range?: PortfolioRange; poolAddress?: AddressLike } = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/users/${addressString(userAddress)}/lp-earnings`,
      withParams(options, serializeParams(params))
    );
  }

  userPositions<T = unknown>(
    userAddress: AddressLike,
    params: Omit<PositionOptions, "userAddress"> = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request(
      `/users/${addressString(userAddress)}/positions`,
      withParams(options, serializeParams(params))
    );
  }

  positions<T = unknown>(
    params: PositionOptions = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request("/positions", withParams(options, serializeParams(params)));
  }

  liquidityPositions<T = unknown>(
    params: PositionOptions = {},
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request("/positions/liquidity", withParams(options, serializeParams(params)));
  }

  position<T = unknown>(positionId: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request(`/positions/${positionId}`, options);
  }

  coingeckoTickers<T = unknown>(options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/coingecko/tickers", options);
  }

  geckoLatestBlock<T = unknown>(options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/gecko/latest-block", options);
  }

  geckoAsset<T = unknown>(id: AddressLike, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/gecko/asset", withParams(options, { id: addressString(id) }));
  }

  geckoPair<T = unknown>(id: AddressLike, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/gecko/pair", withParams(options, { id: addressString(id) }));
  }

  geckoEvents<T = unknown>(
    params: { fromBlock: number; toBlock: number },
    options?: RequestOptions
  ): Promise<ApiResponse<T>> {
    return this.request("/gecko/events", withParams(options, params));
  }

  cmcFactory<T = unknown>(options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/cmc/factory", options);
  }

  cmcSummary<T = unknown>(options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/cmc/summary", options);
  }

  cmcAssets<T = unknown>(options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/cmc/assets", options);
  }

  cmcTicker<T = unknown>(options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request("/cmc/ticker", options);
  }

  cmcTrades<T = unknown>(marketPair: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request(`/cmc/trades/${encodeURIComponent(marketPair)}`, options);
  }
}

function normalizeBaseUrl(baseUrl: string): string {
  return baseUrl.replace(/\/+$/, "");
}

function withParams(options: RequestOptions | undefined, params?: object): RequestOptions {
  return {
    ...options,
    params: {
      ...options?.params,
      ...serializeParams(params ?? {}),
    },
  };
}

function serializeParams(params: object): RequestOptions["params"] {
  return Object.fromEntries(
    Object.entries(params as Record<string, unknown>).map(([key, value]) => [key, serializeParam(value)])
  );
}

function serializeActivityParams(params: ActivityOptions): RequestOptions["params"] {
  return {
    ...serializeParams(params),
    categories: params.categories?.join(","),
  };
}

async function readResponseBody(response: Response): Promise<unknown> {
  const contentType = response.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) {
    return response.json();
  }
  return response.text();
}

function serializeParam(value: unknown): string | number | boolean | undefined | null {
  if (value === undefined || value === null || typeof value !== "object") {
    return value as string | number | boolean | undefined | null;
  }
  if (Array.isArray(value)) {
    return value.join(",");
  }
  try {
    return addressString(value as AddressLike);
  } catch {
    return String(value);
  }
}
