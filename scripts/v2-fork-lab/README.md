# Omnipair V2 (Dusk) Surfpool Mainnet-Fork Lab

This stack runs `dusk` against a private Surfpool mainnet fork and exposes a small fork API for the Dusk webapp. It is intentionally separate from the Helius-backed indexer path because private Surfpool transactions are not visible to Helius Atlas.

## Services

- `v2-surfpool-rpc`: private, long-running Surfpool SDK controller. It starts
  an embedded mainnet fork from the configured remote datasource, deploys Dusk
  and `leverage_delegate` at their exact pinned addresses, bootstraps both
  markets, and supervises a child process that relays the SDK's dynamic RPC/WS
  sockets onto stable ports and owns controller health independently of the
  native SDK event loop.
- `v2-surfpool-rpc-proxy`: public Solana HTTP and WebSocket RPC proxy for
  wallets. It forwards subscriptions and confirmations while blocking
  `requestAirdrop`, `surfnet_*`, and configured private methods.
  `FORK_ADMIN_TOKEN` can authorize blocked HTTP calls; privileged WebSocket
  calls are always rejected.
- `v2-fork-api`: public Dusk fork API. In mainnet-fixture mode it bootstraps
  fork-only META/USDC CPMM and concentrated markets by default, funds wallets
  through bounded Surfpool cheatcodes, serves webapp-compatible Dusk read
  endpoints, and builds unsigned browser transactions. Routes under
  `/api/v2/fork/admin/*`, every request for an API-signed bootstrap
  transaction, and create-market preparation (which creates the three LP mint
  accounts with the shared controller) fail closed and require
  `x-fork-admin-token`.

## Local Commands

```sh
npm run build:v2-fork-rpc-controller
npm run v2-fork:surfpool
npm run v2-fork:surfpool:cli # legacy CLI fallback
npm run v2-fork:rpc-proxy
npm run v2-fork:api
npm run test-surfpool-v2
npm run surfpool-v2-e2e
npm run test:v2-fork-pure
npm run test:v2-fork-controller
npm run test:v2-fork-api
npm run test:v2-fork-rpc-proxy
npm run surfpool-sdk-e2e
```

`surfpool-sdk-e2e` is the canonical test runner. It embeds the official
`@solana/surfpool` SDK, deploys both binaries directly at the pinned Dusk and
`leverage_delegate` addresses (so it does not require program keypair files),
then runs both market kinds. Set `FORK_SDK_REMOTE_RPC_URL` for the default
META/USDC mainnet fixture. An offline structural smoke can instead set
`FORK_MARKET_FIXTURE=mixed-decimals`; that path deploys the repository's
purpose-built Token Metadata fixture from
`target/deploy/token_metadata_fixture.so`.

## Core Env

```sh
SURFPOOL_RPC_URL=http://127.0.0.1:8899
SURFPOOL_WS_URL=ws://127.0.0.1:8900
PUBLIC_SURFPOOL_RPC_URL=http://127.0.0.1:8898
FORK_SDK_REMOTE_RPC_URL=<required-hosted-mainnet-rpc-url>
SURFPOOL_RPC_PORT=8899
SURFPOOL_WS_PORT=8900
PORT=<Railway-injected-controller-health-port>
SURFPOOL_HEALTH_HOST=0.0.0.0
SURFPOOL_HEALTH_REQUEST_TIMEOUT_MS=5000
SURFPOOL_HEALTH_MAX_CONNECTIONS=32
SURFPOOL_STARTUP_PROBE_TIMEOUT_MS=15000
SURFPOOL_RELAY_HEARTBEAT_INTERVAL_MS=1000
SURFPOOL_RELAY_HEARTBEAT_STALE_MS=10000
SURFPOOL_RELAY_CONNECT_TIMEOUT_MS=5000
SURFPOOL_RELAY_MAX_CONNECTIONS=256
SURFPOOL_RELAY_TARGET_PROBE_INTERVAL_MS=5000
SURFPOOL_RELAY_TARGET_PROBE_TIMEOUT_MS=2000
SURFPOOL_RELAY_TARGET_PROBE_FAILURE_THRESHOLD=3
SURFPOOL_STARTUP_SETTLEMENT_TIMEOUT_MS=30000
SURFPOOL_PAYER_FUNDING_LAMPORTS=100000000000
DUSK_PROGRAM_ID=358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv
DUSK_BASE_MINT=METAwkXcqyXKy1AtsSgJ8JiUHwGCafnZL38n3vYmeta
DUSK_QUOTE_MINT=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
FORK_ADMIN_TOKEN=<shared-secret>
FORK_RPC_PROXY_MAX_BATCH_ITEMS=100
FORK_RPC_PROXY_WS_MAX_CLIENTS=64
FORK_RPC_PROXY_WS_MAX_PAYLOAD_BYTES=1048576
FORK_RPC_PROXY_WS_MAX_BUFFERED_BYTES=2097152
FORK_RPC_PROXY_WS_MAX_FRAGMENTS=128
FORK_RPC_PROXY_WS_MAX_BUFFERED_CHUNKS=256
FORK_RPC_PROXY_WS_CLIENT_MAX_OPERATIONS_PER_WINDOW=256
FORK_RPC_PROXY_WS_GLOBAL_MAX_OPERATIONS_PER_WINDOW=4096
FORK_RPC_PROXY_WS_OPERATION_WINDOW_MS=1000
FORK_RPC_PROXY_HTTP_MAX_BODY_BYTES=1048576
FORK_RPC_PROXY_HTTP_MAX_RESPONSE_BYTES=4194304
FORK_RPC_PROXY_HTTP_MAX_IN_FLIGHT_REQUESTS=32
FORK_RPC_PROXY_HTTP_REQUEST_TIMEOUT_MS=15000
FORK_RPC_PROXY_HTTP_UPSTREAM_TIMEOUT_MS=15000
FORK_RPC_PROXY_HEALTH_PROBE_TIMEOUT_MS=5000
FORK_RPC_PROXY_HEALTH_CACHE_MS=5000
FORK_API_MAX_BODY_BYTES=1048576
FORK_API_MAX_IN_FLIGHT_REQUESTS=32
FORK_API_BODY_TIMEOUT_MS=15000
FORK_API_ROUTE_TIMEOUT_MS=120000
FORK_LAB_PAYER_KEYPAIR_JSON=<same-64-byte-secret-array-on-rpc-and-api>
FORK_BOOTSTRAP_MARKETS=both
DUSK_FORK_NAMESPACE=dusk-surfpool
DUSK_BUILD_REVISION=<immutable-git-sha>
```

### Required Railway service wiring

Deploy all three services in the same Railway project and environment. Railway
private DNS uses `<service-name>.railway.internal`; it does not infer the
listening port, so the private Surfpool port must remain explicit.

Create three Railway services from this repository and explicitly select each
service's config-as-code file in its Railway settings. There is no root
`railway.json` or `railway.toml`, so Railway will not discover these nested
files automatically:

| Service | Railway config file path |
| --- | --- |
| `v2-surfpool-rpc` | `/railway/v2-surfpool-rpc.json` |
| `v2-surfpool-rpc-proxy` | `/railway/v2-rpc-proxy.json` |
| `v2-fork-api` | `/railway/v2-fork-api.json` |

Verify the selected path before each deploy; using one service's config for
another can expose the raw fork or run the wrong health check.

| Service | Required variables and exposure |
| --- | --- |
| `v2-surfpool-rpc` | `FORK_SDK_REMOTE_RPC_URL=<secret-mainnet-rpc>`, `FORK_LAB_PAYER_KEYPAIR_JSON=<shared-signer>`, `SURFPOOL_HOST=::`, `SURFPOOL_HEALTH_HOST=::`, `SURFPOOL_RPC_PORT=8899`, `SURFPOOL_WS_PORT=8900`; Railway injects a distinct `PORT` for `/health`; keep both raw ports private and do not attach a public domain |
| `v2-surfpool-rpc-proxy` | `SURFPOOL_RPC_URL=http://${{v2-surfpool-rpc.RAILWAY_PRIVATE_DOMAIN}}:8899`, `SURFPOOL_WS_URL=ws://${{v2-surfpool-rpc.RAILWAY_PRIVATE_DOMAIN}}:8900`, `FORK_ADMIN_TOKEN=<shared-admin-secret>`; attach the browser-facing public domain only to this filtered RPC service |
| `v2-fork-api` | `SURFPOOL_RPC_URL=http://${{v2-surfpool-rpc.RAILWAY_PRIVATE_DOMAIN}}:8899`, `PUBLIC_SURFPOOL_RPC_URL=https://${{v2-surfpool-rpc-proxy.RAILWAY_PUBLIC_DOMAIN}}` (or the equivalent `SURFPOOL_RPC_PROXY_URL` alias), plus the same signer/admin secrets and immutable `DUSK_BUILD_REVISION`; attach the public API domain here. Railway starts this service with `DUSK_REQUIRE_PUBLIC_RPC_URL=true`, so a missing, blank, or non-HTTP(S) public URL fails at startup. |

If the actual Railway service names differ, update the reference-variable
prefixes to match. Never use a `127.0.0.1` default between Railway services:
each service runs in a separate container. `PUBLIC_SURFPOOL_RPC_URL` is
mandatory for the hosted API because browser transaction payloads cannot use a
private Railway address; startup verification must confirm returned
`data.rpcUrl` points to the filtered public proxy. Public health and config
payloads never include the private Surfpool hostname or port. Hosted API
readiness also checks that this public URL blocks the Surfnet cheatcode prefix,
answers a bounded local-only RPC health probe, and exposes the same unique
fork-generation marker as the private RPC. The immutable source-network
genesis is pinned to the exact 32-byte fork-generation marker. Ordinary
readiness and transaction-build requests reuse it without remote genesis
traffic; a marker change triggers one new genesis read and a second marker
observation rejects reset races. A typo, raw
Surfnet URL, or stale proxy therefore keeps the API unhealthy without turning
health polling into remote-datasource traffic.
Local runs do not set `DUSK_REQUIRE_PUBLIC_RPC_URL`, so they retain the
private/local RPC fallback when no separate browser endpoint is configured.

Railway private networking supports IPv6, and older project environments may
resolve private DNS only over IPv6. The RPC image therefore binds the stable
controller proxies to `SURFPOOL_HOST=::`; the local fresh-stack runner
overrides that with `127.0.0.1`. Both `SURFPOOL_RPC_URL` and
`SURFPOOL_WS_URL` on the public proxy must use Railway private DNS and their
explicit private ports. Never expose port 8900 directly.

An HTTPS proxy URL is sufficient for browser `@solana/web3.js`: it derives a
same-domain `wss://` URL with no extra port, and Railway sends that upgrade to
the proxy's public HTTP service. For local HTTP on explicit port 8898, web3.js
would conventionally derive port 8899. Local callers that use subscriptions or
`confirmTransaction` must instead pass the filtered endpoint explicitly:

```ts
new Connection("http://127.0.0.1:8898", {
  commitment: "confirmed",
  wsEndpoint: "ws://127.0.0.1:8898",
});
```

The Dusk webapp exposes that override as
`NEXT_PUBLIC_DUSK_WS_URL=ws://127.0.0.1:8898`. In hosted environments set it
to `wss://<the same v2-surfpool-rpc-proxy public domain>`; the hosted webapp
requires and validates this explicit `ws://` or `wss://` endpoint.

`FORK_ADMIN_TOKEN` is required on the hosted fork API and RPC proxy. Both
hosted images set `FORK_REQUIRE_ADMIN_TOKEN=true`, so a missing or whitespace-
only secret prevents the service from starting; local runs retain explicit
opt-in behavior. Keep it in Railway service variables; never expose it to the
browser. Admin API requests
must send the exact value in `x-fork-admin-token`. A missing server variable
returns typed HTTP 503 (`fork_admin_auth_not_configured`), a missing request
header returns 401 (`fork_admin_auth_required`), and a wrong token returns 403
(`fork_admin_auth_forbidden`). The protocol-test harness forwards this header
from its own `FORK_ADMIN_TOKEN` environment variable for time-travel and
API-signed protocol-test scenarios.
The token does not authorize privileged WebSocket calls; all WS messages
matching `requestAirdrop`, `surfnet_*`, or `FORK_RPC_PROXY_BLOCKED_METHODS` are
rejected before reaching the private socket. Public HTTP `requestAirdrop` is
also blocked by default; browser funding goes through the bounded fork API.

Railway also requires one explicit Surfpool signer secret. Set the same
`FORK_LAB_PAYER_KEYPAIR_JSON` (or base64 equivalent) on the RPC and API
services. The SDK controller passes it directly to `Surfnet.startWithConfig` as
its deployment/bootstrap wallet, while the API loads it for admin-only signed
protocol-test transactions. The legacy CLI fallback materializes the same
secret as a keypair file. Hosted services fail readiness instead of generating
unrelated ephemeral signers. The RPC controller also records its signer public
key in a fork-owned marker; read-only API readiness verifies that marker, so two
different explicit secrets cannot silently start a broken stack.

`FORK_SDK_REMOTE_RPC_URL` is required by the hosted SDK controller; the
equivalent `SURFPOOL_DATASOURCE_RPC_URL` name is also accepted. It must be an
HTTP(S) Solana mainnet RPC capable of serving the META and USDC accounts needed
by the fork. Keep provider credentials in Railway service variables and never
publish this datasource URL to the browser. The SDK binds its own RPC and WS
servers to dynamic loopback ports. A supervised Node child process owns the
stable relays and `/health` listener, so native SDK work or a stalled
controller event loop cannot also stop accepting TCP connections. The
controller does not report ready until both programs and markets are prepared;
the child then exposes dual-stack raw TCP proxies on
`[::]:${SURFPOOL_RPC_PORT:-8899}` and
`[::]:${SURFPOOL_WS_PORT:-8900}`. This preserves stable service-to-service
addresses without relying on the CLI runbook's fixed-port deployment behavior.
The controller explicitly funds the shared payer before deployment and
bootstrap; the default is 100 SOL (the same budget used by the SDK E2E), and
`SURFPOOL_PAYER_FUNDING_LAMPORTS` accepts an exact positive safe-integer
lamport override.

The relay child immediately binds a bounded HTTP readiness listener to
Railway's injected `PORT` and serves `GET /health`. It returns 503 while the
fork is starting, after a fatal controller failure, during shutdown, or when
the controller heartbeat is older than `SURFPOOL_RELAY_HEARTBEAT_STALE_MS`.
It returns 200 only after both exact-address programs are deployed and authority-
aligned, both markets are bootstrapped, the stable RPC and WebSocket relays are
listening, both deployed binaries pass exact stable-RPC probes, and the stable
WebSocket handshake succeeds. `PORT` must differ from `SURFPOOL_RPC_PORT` and
`SURFPOOL_WS_PORT`; when `PORT` is absent locally, the health listener chooses
an ephemeral port. Requests and concurrent health connections are bounded by
`SURFPOOL_HEALTH_REQUEST_TIMEOUT_MS` and
`SURFPOOL_HEALTH_MAX_CONNECTIONS`. The final WebSocket startup probe is bounded
by `SURFPOOL_STARTUP_PROBE_TIMEOUT_MS`. The child also probes the dynamic raw
Surfpool RPC and WebSocket targets directly and concurrently: RPC uses the
local-only `getHealth` method and WebSocket uses a real upgrade. Health cannot
become ready before the first successful pair. An isolated failure is exposed
in degraded counters while traffic continues; reaching the bounded consecutive-
failure threshold changes health to 503 and is fatal. Configure this with
`SURFPOOL_RELAY_TARGET_PROBE_INTERVAL_MS` (default 5000, maximum 300000),
`SURFPOOL_RELAY_TARGET_PROBE_TIMEOUT_MS` (default 2000, maximum 120000), and
`SURFPOOL_RELAY_TARGET_PROBE_FAILURE_THRESHOLD` (default 3, maximum 20).
Dynamic targets pointing back to their own stable listener are rejected.
Health includes only bounded counters:
relay connection/error totals plus event-drain calls, event totals, failures,
the current 100–250 ms adaptive drain interval, and current/maximum tick lag.
Event payloads and upstream URLs are never retained or exposed.

Listener bind/error and child exit are fatal controller failures. A refused,
reset, or timed-out individual upstream connection closes only that socket pair
and increments its relay counter; later wallet/API connections can recover.
The parent sends a heartbeat every
`SURFPOOL_RELAY_HEARTBEAT_INTERVAL_MS`. Parent exit or IPC disconnect closes the
child; SIGINT/SIGTERM marks health stopping, closes relays, stops Surfnet, and
then exits. The container and Railway start Node directly as PID 1 so this
signal lifecycle is not hidden behind an npm wrapper.

Every asynchronous startup phase remains tracked after an abort/failure race,
including both parallel exact-program probes. The controller waits up to
`SURFPOOL_STARTUP_SETTLEMENT_TIMEOUT_MS` (default 30000, maximum 300000) for
all active phases before tearing down Surfnet. If native work remains hung, it
refuses unsafe in-process teardown and hard-exits the container after the
grace. Relay-child startup and shutdown separately use bounded graceful-exit,
SIGTERM, then SIGKILL escalation.

`railway/v2-surfpool-rpc.json` configures `/health` with a 600-second deployment
timeout so the native mainnet-fork bootstrap can finish without declaring an
unverified instance ready. Railway uses the injected `PORT` for this probe and
waits for HTTP 200 before activating the deployment. Railway healthchecks are
deployment readiness checks, not continuous post-deploy monitoring, so runtime
controller failure still exits the process and relies on the restart policy.

The controller's stable RPC and WS ports are raw Surfnet endpoints and expose
the full `surfnet_*` cheatcode surface. Keep `v2-surfpool-rpc` private on
Railway: do not attach a public domain or route public traffic directly to
either port. Only `v2-surfpool-rpc-proxy`, which filters unauthenticated
HTTP cheatcodes and all WebSocket cheatcodes, may be public. The relay accepts
browser WSS upgrades on the same public domain and forwards normal Solana
subscription messages to the private `SURFPOOL_WS_URL`. Payload, pending-queue,
backpressure, and upstream-handshake limits are bounded by the
`FORK_RPC_PROXY_WS_*` environment variables; conservative defaults are used
when they are unset. The global client cap is enforced before an upgrade can
open a private upstream socket, and both sides of the relay apply explicit
fragment and buffered-chunk limits. Every HTTP batch and individual WebSocket
message is limited to `FORK_RPC_PROXY_MAX_BATCH_ITEMS` entries. WebSocket text
messages also consume per-client and process-wide operation budgets; every
batch entry counts as one operation and even an empty or malformed message
costs one. The defaults allow 256 operations per client and 4,096 globally per
one-second window. A client that exceeds either budget is closed with policy
code 1008; upstream subscription notifications do not consume the budget.
HTTP JSON-RPC bodies are independently capped before parsing or forwarding;
private responses are read through a byte-limited stream before forwarding.
An in-flight HTTP cap makes the aggregate response-memory bound finite; each
slot is retained until the downstream response finishes or closes, and stalled
slow-reader sockets are terminated on the configured request timeout. Slow
public requests and stalled private RPC calls also have bounded timeouts.
Railway's proxy `/health` check performs a bounded local-only `getHealth` RPC
request and a real WebSocket upgrade against the two private Surfpool targets.
It returns 503 if either transport is unavailable. Successful and failed results are
deduplicated for `FORK_RPC_PROXY_HEALTH_CACHE_MS` (five seconds by default), so
public health polling cannot amplify into an unbounded number of private
connections; each probe is limited by
`FORK_RPC_PROXY_HEALTH_PROBE_TIMEOUT_MS`.
The public fork API independently caps request bodies before JSON parsing,
times out incomplete bodies, and returns typed 413 or 408 errors. Route work
has a separate deadline and returns typed HTTP 504; POST timeouts are marked
`uncertainOutcome` because the underlying Solana operation may have started.
Timed-out work retains its in-flight slot until it settles, so a stalled
Surfnet cannot create an unbounded queue of background mutations; saturation
returns typed HTTP 503.

`FORK_BOOTSTRAP_MARKETS` accepts `cpmm`, `concentrated`, or `both`. Mainnet
fixtures default to `both`; Token-2022-fee and mixed-decimal fixtures default to
`cpmm`. A concentrated market defaults to `200000000000` peak depth and
`100000000` fade scale. Override those with `DUSK_AMM_PEAK_DEPTH_NAD` and
`DUSK_AMM_FADE_SCALE_NAD`.

The two markets must have different parameter hashes. Deterministic hashes are
generated by default. To pin them, set both
`DUSK_FORK_PARAMS_HASH_CPMM` and
`DUSK_FORK_PARAMS_HASH_CONCENTRATED` to different 32-byte hex values. A shared
`DUSK_FORK_PARAMS_HASH` or `DUSK_MARKET_PARAMS_HASH` is accepted only when one
market is bootstrapped.

The fork API accepts `FORK_LAB_PAYER_KEYPAIR_JSON`, `FORK_LAB_PAYER_KEYPAIR_BASE64`, `FORK_LAB_PAYER_KEYPAIR`, or `ANCHOR_WALLET`. If none are set it creates a local `.v2-fork-lab/payer.json`.

## Deployment identity and reset safety

Every successful health, config, list, detail, and transaction-build response
includes a `deployment` object with schema `dusk-deployment.v1`. It carries the
Surfpool source network, genesis hash, fork generation, both program IDs,
canonical and raw IDL digests, confirmed source slot, observation time, build
revision, both deployed ProgramData binary digests, loader deployment slots, and
upgrade authorities. `deploymentIdentitySha256` is stable across API replicas
and RPC slot progress. Clients must validate this identity before and after
reads and immediately before signing a write.

`idlSha256` is the SHA-256 of recursively key-sorted JSON, not the bytes of one
formatted IDL file. This lets the source and packaged SDK IDLs prove semantic
identity while `idlRawSha256` still records the exact API artifact. The reviewed
Dusk canonical digest for this snapshot is
`fc4ece4350fd9cdb3564cc4a157c8f0f7eafccfe7fe1cb46b43e34e8ad13eed6`.

The RPC service stores a random 32-byte generation marker at a deterministic
Dusk PDA through the private Surfpool cheatcode before it reports ready. The
resulting `forkId` is stable across API restarts while that fork state survives
and changes when Surfpool state is reset. Railway runs the API with
`DUSK_REQUIRE_EXTERNAL_FORK_MARKER=true`, so multiple API replicas only read the
RPC-owned marker and cannot race to overwrite it. Local SDK tests may create a
missing marker for convenience. `DUSK_FORK_NAMESPACE` scopes the identifier; it
is not itself the fork ID. Set `DUSK_BUILD_REVISION` to the immutable deployed
Git SHA on Railway. A missing or non-executable Dusk or leverage-delegate
program fails readiness and all enveloped API responses closed. Program binary
hashes are read from the deployed upgradeable-loader ProgramData accounts, not
from files that may be absent or differ in the API image. Warm identity probes
read only the loader headers; full multi-megabyte ProgramData payloads are
re-hashed only when the fork ID, loader deployment slot, or upgrade authority
changes.

The RPC service is the single market-bootstrap controller. It initializes and
seeds the configured markets before API replicas are allowed to serve them. A
durable fork account records completed initial liquidity, so a cold API replica
or missing local `state.json` cannot deposit the seed twice. Railway API replicas
run with `DUSK_REQUIRE_PREBOOTSTRAPPED_MARKETS=true` and use a network-read-only
verification path: they never fund the payer, initialize accounts, or write
Surfpool markers, and fail closed if the RPC controller has not finished.
Persisted market records remain hints only: every
selected market is fetched, owner-decoded, and PDA-validated against the current
fork before use.

The API observes deployment identity before and after every request, keys its
market-bootstrap promise by the complete stable deployment fingerprint, and
rejects the response if immutable identity changes mid-request. A reset or
in-place program upgrade therefore cannot reuse a process-lifetime market/LP-mint
cache or combine old response data with a new fork envelope. Bootstrap work is
serialized across generations; after a mid-bootstrap reset, the old request
fails and the next generation repairs bootstrap before serving.
If a reset races an endpoint with side effects, or a multi-step wallet-funding
or time-travel operation fails after mutation starts, the API responds HTTP 409
with `uncertainOutcome: true`; callers must reconcile rather than blindly retry.

An in-place `surfnet_resetNetwork` also removes locally deployed programs, so it
is not a supported standalone Railway operation. Reset the fork by restarting
or redeploying `v2-surfpool-rpc`; that lifecycle redeploys both Dusk programs and
seeds a new marker before the API becomes ready.

Market state uses Anchor `fetchAndContext` at confirmed commitment. Its
`state.sourceSlot` is the market-account RPC context slot and `state.observedAt`
is that observation bank's block time or `null`. Preview-derived health is a
separate observation with `state.healthSourceSlot` and
`state.healthObservedAt`. `updatedAt` stays `null` unless an on-chain update slot
or indexer event can support it. Request time is never presented as chain update
time and the two observations are never relabeled as one atomic snapshot.

All `/api/v2/fork/admin/*` routes and all server-signed transaction requests
(`bootstrapSigned: true`, `/tx/bootstrap-rejection`, and `/tx/create-market`)
fail closed unless `FORK_ADMIN_TOKEN` is configured and the caller sends the
exact value in `x-fork-admin-token`. Create-market LP mint keypairs are derived
deterministically from the shared explicit controller signer and market label,
so retries and API replicas cannot select different mint identities. Missing
configuration returns 503, a missing header returns 401, and a wrong token
returns 403. Protocol-test clients forward this header automatically.

The public fork faucet accepts only on-curve wallet addresses. An existing
target must be a non-executable, zero-data System Program account; program,
PDA, mint, token, nonce, and other stateful accounts are rejected before any
fork mutation. Passing `sol: 0` leaves the wallet's lamports unchanged while
still allowing the requested test-token balances to be prepared. SOL values
must resolve to a nonnegative safe-integer number of lamports. Token funding is
a monotonic minimum-balance top-up on every fixture: requests at or below an
existing ATA balance are a no-op, so `0` can safely mean “do not top up.” Once an
airdrop RPC has been attempted, any submission or confirmation failure is
reported as an uncertain outcome; the API never follows it with a destructive
exact-account fallback.

For mainnet-fork fixtures, the faucet never exact-writes a user's associated
token account. It exact-funds only a deterministic controller-owned reservoir,
then sends the missing amount through the real SPL Token program. Token-2022
transfer fees are grossed up so the requested net minimum arrives. Concurrent
wallet activity or API replicas can therefore overfund or fail, but cannot
lower a user's token balance. Public funding accepts only the configured
fixture's mint and token-program pair (both configured META/USDC curves share
it); this prevents an arbitrary Token-2022 transfer hook from entering a
server-signed faucet transaction.

## API Endpoints

- `GET /health` (reports the filtered public RPC and deployment identity, never
  the private Surfpool hostname or port)
- `GET /api/v2/fork/config` (returns only the browser-reachable filtered RPC,
  never the private Surfpool hostname or port)
- `GET /api/v2/fork/yield-account?owner=:wallet&market=:marketAddress`
- `POST /api/v2/fork/fund-wallet`
- `POST /api/v2/fork/admin/time-travel` (admin token required)
- `POST /api/v2/fork/tx/add-liquidity`
- `POST /api/v2/fork/tx/swap`
- `POST /api/v2/fork/tx/deposit-collateral`
- `POST /api/v2/fork/tx/borrow`
- `POST /api/v2/fork/tx/repay`
- `POST /api/v2/fork/tx/deposit-single-sided`
- `POST /api/v2/fork/tx/withdraw-single-sided`
- `POST /api/v2/fork/tx/update-protocol-auction-config`
- `POST /api/v2/fork/tx/update-protocol-auction-recipients`
- `POST /api/v2/fork/tx/settle-protocol-auction`
- `GET /api/v2/markets`
- `GET /api/v2/markets/:marketAddress`
- `GET /api/v2/markets/:marketAddress/swaps`
- `GET /api/v2/users/:wallet/positions`
- `GET /api/v2/users/:wallet/activity`

Normal transaction endpoints return an unsigned base64 legacy transaction in
`data.transaction`. The browser wallet signs and submits it to `data.rpcUrl`,
which should be the public RPC proxy. The API-signed bootstrap/protocol-test
mode is admin-only and must never be exposed through browser configuration.

Market creation requires explicit `baseMint` and `quoteMint` fields. The API
preserves that protocol order and rejects ambiguous `mintA`/`mintB` input rather
than silently byte-sorting the pair.

By default, `GET /api/v2/fork/config` keeps the primary CPMM market in the
legacy top-level fields and adds a `markets` array containing both bootstrapped
market addresses, kinds, mints, parameter hashes, and seed status.
`GET /api/v2/markets` returns both markets. Pass the selected address as
`market` in transaction request bodies; omitting it intentionally targets the
primary market.

`GET /api/v2/users/:wallet/positions` and
`GET /api/v2/fork/yield-account` accept `?market=<address>`. The positions
endpoint probes all bootstrapped markets when `market` is omitted and a
`positionId` is provided; adding `market` restricts the lookup to that market.
Use `market` on the yield-account endpoint when reading the non-primary market.

Remote `test-surfpool-v2` runs add-liquidity and swap against every entry in
`config.markets`, with the legacy top-level `config.market` as a one-market
fallback.

Protocol-auction settlement requires `lane: "fee" | "buyback"` and an explicit
`source: "swap" | "interest"`. The API never defaults the source. Swap revenue
is sold from the matching reserve vault and interest revenue from the matching
interest vault, so the selected physical custody always matches the liability
that settlement debits.
