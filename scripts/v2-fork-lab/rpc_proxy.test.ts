import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { afterEach, describe, it } from "mocha";
import WebSocket, { WebSocketServer, type RawData } from "ws";

import {
  createRpcProxyServer,
  rpcProxyConfigFromEnv,
  type RpcProxyRuntime,
} from "./rpc_proxy.js";

interface Fixture {
  upstreamHttp: http.Server;
  upstreamWs: WebSocketServer;
  proxy: RpcProxyRuntime;
  proxyPort: number;
  upstreamMethods: string[];
  upstreamWebSocketConnections: number;
}

const fixtures: Fixture[] = [];

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

async function waitUntil(predicate: () => boolean): Promise<void> {
  const deadline = Date.now() + 1_000;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error("Timed out waiting for test state");
    await delay(5);
  }
}

function listen(server: http.Server): Promise<number> {
  return new Promise((resolvePromise, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error("Expected a TCP listener"));
        return;
      }
      resolvePromise(address.port);
    });
  });
}

function text(data: RawData): string {
  if (Array.isArray(data)) return Buffer.concat(data).toString("utf8");
  if (data instanceof ArrayBuffer) return Buffer.from(data).toString("utf8");
  return data.toString("utf8");
}

function nextMessage(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolvePromise, reject) => {
    socket.once("error", reject);
    socket.once("message", (data) => {
      socket.off("error", reject);
      resolvePromise(JSON.parse(text(data)) as Record<string, unknown>);
    });
  });
}

function nextClose(socket: WebSocket): Promise<number> {
  return new Promise((resolvePromise) => {
    socket.once("close", (code) => resolvePromise(code));
  });
}

function openWebSocket(url: string, headers?: Record<string, string>): Promise<WebSocket> {
  return new Promise((resolvePromise, reject) => {
    const socket = new WebSocket(url, { headers });
    socket.once("open", () => resolvePromise(socket));
    socket.once("error", reject);
  });
}

async function fixture(
  envOverrides: NodeJS.ProcessEnv = {},
  behavior: { failHealthRpc?: boolean } = {},
): Promise<Fixture> {
  const upstreamMethods: string[] = [];
  const upstreamHttp = http.createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(Buffer.from(chunk));
    const payload = JSON.parse(Buffer.concat(chunks).toString("utf8")) as {
      id: unknown;
      method: string;
    };
    upstreamMethods.push(`http:${payload.method}`);
    if (payload.method === "getHealth") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(behavior.failHealthRpc
        ? {
            jsonrpc: "2.0",
            id: payload.id,
            error: { code: -32000, message: "unhealthy" },
          }
        : {
            jsonrpc: "2.0",
            id: payload.id,
            result: "ok",
          }));
      return;
    }
    if (payload.method === "slowRpc") await delay(150);
    if (payload.method === "largeResponse") {
      const body = JSON.stringify({
        jsonrpc: "2.0",
        id: payload.id,
        result: "x".repeat(512),
      });
      res.writeHead(200, {
        "content-type": "application/json",
        "transfer-encoding": "chunked",
      });
      res.write(body.slice(0, 32));
      res.write(body.slice(32, 96));
      res.end(body.slice(96));
      return;
    }
    if (payload.method === "slowReaderResponse") {
      const body = JSON.stringify({
        jsonrpc: "2.0",
        id: payload.id,
        result: "x".repeat(2_000_000),
      });
      res.writeHead(200, {
        "content-length": Buffer.byteLength(body),
        "content-type": "application/json",
      });
      res.end(body);
      return;
    }
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({
      jsonrpc: "2.0",
      id: payload.id,
      result: true,
    }));
  });
  const upstreamWs = new WebSocketServer({ server: upstreamHttp });
  let upstreamWebSocketConnections = 0;
  upstreamWs.on("connection", (socket) => {
    upstreamWebSocketConnections += 1;
    socket.on("message", (data) => {
      const payload = JSON.parse(text(data)) as
        | { id: unknown; method: string }
        | Array<{ id: unknown; method: string }>;
      const requests = Array.isArray(payload) ? payload : [payload];
      const responses = requests.map((request) => {
        upstreamMethods.push(`ws:${request.method}`);
        if (request.method === "signatureSubscribe") {
          setTimeout(() => {
            if (socket.readyState !== WebSocket.OPEN) return;
            socket.send(JSON.stringify({
              jsonrpc: "2.0",
              method: "signatureNotification",
              params: {
                subscription: 41,
                result: { context: { slot: 123 }, value: { err: null } },
              },
            }));
          }, 10);
          return { jsonrpc: "2.0", id: request.id, result: 41 };
        }
        return { jsonrpc: "2.0", id: request.id, result: true };
      });
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify(Array.isArray(payload) ? responses : responses[0]));
      }
    });
  });
  const upstreamPort = await listen(upstreamHttp);

  const proxy = createRpcProxyServer(rpcProxyConfigFromEnv({
    FORK_ADMIN_TOKEN: "http-admin-only",
    FORK_RPC_PROXY_BLOCKED_METHODS: "dangerous_method",
    SURFPOOL_RPC_URL: `http://127.0.0.1:${upstreamPort}`,
    SURFPOOL_WS_URL: `ws://127.0.0.1:${upstreamPort}`,
    ...envOverrides,
  }));
  const proxyPort = await listen(proxy.server);
  const value = {
    upstreamHttp,
    upstreamWs,
    proxy,
    proxyPort,
    upstreamMethods,
    get upstreamWebSocketConnections() {
      return upstreamWebSocketConnections;
    },
  };
  fixtures.push(value);
  return value;
}

afterEach(async () => {
  while (fixtures.length > 0) {
    const value = fixtures.pop();
    if (!value) continue;
    await value.proxy.close();
    for (const socket of value.upstreamWs.clients) socket.terminate();
    await new Promise<void>((resolvePromise) => value.upstreamWs.close(() => resolvePromise()));
    if (value.upstreamHttp.listening) {
      await new Promise<void>((resolvePromise, reject) => {
        value.upstreamHttp.close((error) => error ? reject(error) : resolvePromise());
      });
    }
  }
});

describe("public Surfpool RPC WebSocket proxy", function () {
  this.timeout(10_000);

  it("forwards signature subscriptions and rejects every blocked WS method", async () => {
    const value = await fixture();
    const socket = await openWebSocket(
      `ws://127.0.0.1:${value.proxyPort}`,
      { "x-fork-admin-token": "http-admin-only" },
    );

    const subscribed = nextMessage(socket);
    socket.send(JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "signatureSubscribe",
      params: ["test-signature", { commitment: "confirmed" }],
    }));
    assert.deepEqual(await subscribed, { jsonrpc: "2.0", id: 1, result: 41 });
    assert.deepEqual(await nextMessage(socket), {
      jsonrpc: "2.0",
      method: "signatureNotification",
      params: {
        subscription: 41,
        result: { context: { slot: 123 }, value: { err: null } },
      },
    });

    const surfnetBlocked = nextMessage(socket);
    socket.send('{"jsonrpc":"2.0","id":2,"method":"surf', { fin: false });
    socket.send('net_resetNetwork","params":[]}', { fin: true });
    assert.equal(
      ((await surfnetBlocked).error as { code: number }).code,
      -32099,
    );

    const airdropBlocked = nextMessage(socket);
    socket.send(JSON.stringify({
      jsonrpc: "2.0",
      id: 3,
      method: "requestAirdrop",
      params: ["wallet", 1_000_000_000],
    }));
    assert.equal(
      ((await airdropBlocked).error as { code: number }).code,
      -32099,
    );

    const malformed = nextMessage(socket);
    socket.send("{");
    assert.equal(((await malformed).error as { code: number }).code, -32700);

    const configuredBlocked = nextMessage(socket);
    socket.send(JSON.stringify([
      { jsonrpc: "2.0", id: 4, method: "getSlot", params: [] },
      { jsonrpc: "2.0", id: 5, method: "dangerous_method", params: [] },
    ]));
    assert.equal(
      ((await configuredBlocked).error as { code: number }).code,
      -32099,
    );
    assert.deepEqual(value.upstreamMethods, ["ws:signatureSubscribe"]);
    socket.close();
  });

  it("keeps HTTP admin forwarding while filtering public HTTP and closes with upstream", async () => {
    const value = await fixture();
    const blocked = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 4, method: "surfnet_resetNetwork" }),
    });
    assert.equal(blocked.status, 403);

    const airdrop = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 5,
        method: "requestAirdrop",
        params: ["wallet", 1_000_000_000],
      }),
    });
    assert.equal(airdrop.status, 403);

    const admin = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-fork-admin-token": "http-admin-only",
      },
      body: JSON.stringify({ jsonrpc: "2.0", id: 6, method: "surfnet_resetNetwork" }),
    });
    assert.equal(admin.status, 200);

    const ambiguousAdmin = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: {
        authorization: "Bearer http-admin-only",
        "content-type": "application/json",
        "x-fork-admin-token": "http-admin-only",
      },
      body: JSON.stringify({ jsonrpc: "2.0", id: 7, method: "surfnet_resetNetwork" }),
    });
    assert.equal(ambiguousAdmin.status, 403);

    const socket = await openWebSocket(`ws://127.0.0.1:${value.proxyPort}`);
    const forwarded = nextMessage(socket);
    socket.send(JSON.stringify({ jsonrpc: "2.0", id: 8, method: "getSlot" }));
    assert.deepEqual(await forwarded, { jsonrpc: "2.0", id: 8, result: true });
    const closed = nextClose(socket);
    for (const upstream of value.upstreamWs.clients) upstream.close(1012, "restart");
    assert.equal(await closed, 1012);
    assert.deepEqual(value.upstreamMethods, [
      "http:surfnet_resetNetwork",
      "ws:getSlot",
    ]);
  });

  it("caps JSON-RPC batch cardinality on HTTP and WebSocket messages", async () => {
    const value = await fixture({ FORK_RPC_PROXY_MAX_BATCH_ITEMS: "2" });
    const oversizedBatch = [
      { jsonrpc: "2.0", id: 1, method: "getSlot", params: [] },
      { jsonrpc: "2.0", id: 2, method: "getBlockHeight", params: [] },
      { jsonrpc: "2.0", id: 3, method: "getGenesisHash", params: [] },
    ];

    const httpResponse = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-fork-admin-token": "http-admin-only",
      },
      body: JSON.stringify(oversizedBatch),
    });
    assert.equal(httpResponse.status, 400);
    assert.equal(
      ((await httpResponse.json() as { error: { code: number } }).error.code),
      -32094,
    );

    const socket = await openWebSocket(`ws://127.0.0.1:${value.proxyPort}`);
    const rejected = nextMessage(socket);
    socket.send(JSON.stringify(oversizedBatch));
    assert.equal(((await rejected).error as { code: number }).code, -32094);
    assert.equal(socket.readyState, WebSocket.OPEN);

    const forwarded = nextMessage(socket);
    socket.send(JSON.stringify({ jsonrpc: "2.0", id: 4, method: "getSlot" }));
    assert.deepEqual(await forwarded, { jsonrpc: "2.0", id: 4, result: true });
    assert.deepEqual(value.upstreamMethods, ["ws:getSlot"]);
    socket.close();
  });

  it("counts WS batch items against the per-client budget without charging notifications", async () => {
    const value = await fixture({
      FORK_RPC_PROXY_MAX_BATCH_ITEMS: "10",
      FORK_RPC_PROXY_WS_CLIENT_MAX_OPERATIONS_PER_WINDOW: "2",
      FORK_RPC_PROXY_WS_GLOBAL_MAX_OPERATIONS_PER_WINDOW: "20",
      FORK_RPC_PROXY_WS_OPERATION_WINDOW_MS: "5000",
    });
    const socket = await openWebSocket(`ws://127.0.0.1:${value.proxyPort}`);

    const subscribed = nextMessage(socket);
    socket.send(JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "signatureSubscribe",
      params: ["test-signature"],
    }));
    assert.deepEqual(await subscribed, { jsonrpc: "2.0", id: 1, result: 41 });
    assert.equal((await nextMessage(socket)).method, "signatureNotification");

    const closed = nextClose(socket);
    socket.send(JSON.stringify([
      { jsonrpc: "2.0", id: 2, method: "getSlot" },
      { jsonrpc: "2.0", id: 3, method: "getBlockHeight" },
    ]));
    assert.equal(await closed, 1008);
    assert.deepEqual(value.upstreamMethods, ["ws:signatureSubscribe"]);
  });

  it("enforces the shared WS operation budget by closing only the excess client", async () => {
    const value = await fixture({
      FORK_RPC_PROXY_MAX_BATCH_ITEMS: "10",
      FORK_RPC_PROXY_WS_CLIENT_MAX_OPERATIONS_PER_WINDOW: "10",
      FORK_RPC_PROXY_WS_GLOBAL_MAX_OPERATIONS_PER_WINDOW: "3",
      FORK_RPC_PROXY_WS_OPERATION_WINDOW_MS: "5000",
    });
    const first = await openWebSocket(`ws://127.0.0.1:${value.proxyPort}`);
    const second = await openWebSocket(`ws://127.0.0.1:${value.proxyPort}`);

    first.send(JSON.stringify([
      { jsonrpc: "2.0", id: 1, method: "getSlot" },
      { jsonrpc: "2.0", id: 2, method: "getBlockHeight" },
    ]));
    await waitUntil(() => value.upstreamMethods.length === 2);

    const secondResponse = nextMessage(second);
    second.send(JSON.stringify({ jsonrpc: "2.0", id: 3, method: "getSlot" }));
    assert.deepEqual(await secondResponse, { jsonrpc: "2.0", id: 3, result: true });

    const firstClosed = nextClose(first);
    first.send(JSON.stringify({ jsonrpc: "2.0", id: 4, method: "getBalance" }));
    assert.equal(await firstClosed, 1008);
    assert.equal(second.readyState, WebSocket.OPEN);
    assert.deepEqual(value.upstreamMethods, [
      "ws:getSlot",
      "ws:getBlockHeight",
      "ws:getSlot",
    ]);
    second.close();
  });

  it("preserves abnormal upstream closure so web3 reconnects", async () => {
    const value = await fixture();
    const socket = await openWebSocket(`ws://127.0.0.1:${value.proxyPort}`);
    const forwarded = nextMessage(socket);
    socket.send(JSON.stringify({ jsonrpc: "2.0", id: 8, method: "getSlot" }));
    assert.deepEqual(await forwarded, { jsonrpc: "2.0", id: 8, result: true });

    const closed = nextClose(socket);
    for (const upstream of value.upstreamWs.clients) upstream.terminate();
    assert.equal(await closed, 1006);
  });

  it("reports healthy only after probing both private RPC transports", async () => {
    const value = await fixture({
      FORK_RPC_PROXY_HEALTH_CACHE_MS: "5000",
      FORK_RPC_PROXY_HEALTH_PROBE_TIMEOUT_MS: "100",
    });
    const response = await fetch(`http://127.0.0.1:${value.proxyPort}/health`);
    assert.equal(response.status, 200);
    const payload = await response.json() as {
      ok: boolean;
      rpc: boolean;
      websocket: boolean;
      checkedAt: string;
    };
    assert.equal(payload.ok, true);
    assert.equal(payload.rpc, true);
    assert.equal(payload.websocket, true);
    assert.equal(Number.isNaN(Date.parse(payload.checkedAt)), false);
    assert.equal("target" in payload, false);
    assert.equal("websocketTarget" in payload, false);
    assert.equal(
      (await fetch(`http://127.0.0.1:${value.proxyPort}/health`)).status,
      200,
    );
    assert.deepEqual(value.upstreamMethods, ["http:getHealth"]);
    assert.equal(value.upstreamWebSocketConnections, 1);
  });

  it("returns 503 when either private RPC transport is unhealthy", async () => {
    const rpcFailure = await fixture(
      {
        FORK_RPC_PROXY_HEALTH_CACHE_MS: "1",
        FORK_RPC_PROXY_HEALTH_PROBE_TIMEOUT_MS: "100",
      },
      { failHealthRpc: true },
    );
    const rpcResponse = await fetch(
      `http://127.0.0.1:${rpcFailure.proxyPort}/health`,
    );
    assert.equal(rpcResponse.status, 503);
    assert.equal(
      (await rpcResponse.json() as { rpc: boolean; websocket: boolean }).rpc,
      false,
    );

    const websocketFailure = await fixture({
      FORK_RPC_PROXY_HEALTH_CACHE_MS: "1",
      FORK_RPC_PROXY_HEALTH_PROBE_TIMEOUT_MS: "30",
    });
    for (const socket of websocketFailure.upstreamWs.clients) socket.terminate();
    await new Promise<void>((resolvePromise) =>
      websocketFailure.upstreamWs.close(() => resolvePromise()),
    );
    const websocketResponse = await fetch(
      `http://127.0.0.1:${websocketFailure.proxyPort}/health`,
    );
    const websocketPayload = await websocketResponse.json() as {
      rpc: boolean;
      websocket: boolean;
    };
    assert.equal(websocketResponse.status, 503);
    assert.equal(websocketPayload.rpc, true);
    assert.equal(websocketPayload.websocket, false);
  });

  it("rejects excess public clients before opening private sockets", async () => {
    const value = await fixture({ FORK_RPC_PROXY_WS_MAX_CLIENTS: "1" });
    const first = await openWebSocket(`ws://127.0.0.1:${value.proxyPort}`);
    await assert.rejects(
      openWebSocket(`ws://127.0.0.1:${value.proxyPort}`),
      /503/,
    );

    const forwarded = nextMessage(first);
    first.send(JSON.stringify({ jsonrpc: "2.0", id: 9, method: "getSlot" }));
    assert.deepEqual(await forwarded, { jsonrpc: "2.0", id: 9, result: true });
    assert.equal(value.upstreamWs.clients.size, 1);
    first.close();
  });

  it("rejects oversized HTTP bodies before buffering or forwarding", async () => {
    const value = await fixture({ FORK_RPC_PROXY_HTTP_MAX_BODY_BYTES: "32" });
    const response = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "x".repeat(33),
    });
    assert.equal(response.status, 413);
    assert.deepEqual(value.upstreamMethods, []);
  });

  it("caps private HTTP response bytes before forwarding", async () => {
    const value = await fixture({ FORK_RPC_PROXY_HTTP_MAX_RESPONSE_BYTES: "64" });
    const response = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 10, method: "largeResponse" }),
    });
    assert.equal(response.status, 502);
    assert.equal(
      ((await response.json() as { error: { code: number } }).error.code),
      -32096,
    );
    assert.deepEqual(value.upstreamMethods, ["http:largeResponse"]);
  });

  it("bounds aggregate HTTP response memory with an in-flight request cap", async () => {
    const value = await fixture({
      FORK_RPC_PROXY_HTTP_MAX_IN_FLIGHT_REQUESTS: "1",
    });
    const first = fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 11, method: "slowRpc" }),
    });
    await waitUntil(() => value.upstreamMethods.includes("http:slowRpc"));

    const rejected = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 12, method: "getSlot" }),
    });
    assert.equal(rejected.status, 503);
    assert.equal((await first).status, 200);
    assert.deepEqual(value.upstreamMethods, ["http:slowRpc"]);
  });

  it("retains the HTTP memory slot until a slow downstream response closes", async () => {
    const value = await fixture({
      FORK_RPC_PROXY_HTTP_MAX_IN_FLIGHT_REQUESTS: "1",
      FORK_RPC_PROXY_HTTP_MAX_RESPONSE_BYTES: "3000000",
    });
    const body = JSON.stringify({
      jsonrpc: "2.0",
      id: 13,
      method: "slowReaderResponse",
    });
    const slowClient = net.createConnection({
      host: "127.0.0.1",
      port: value.proxyPort,
    });
    await new Promise<void>((resolvePromise, reject) => {
      slowClient.once("connect", resolvePromise);
      slowClient.once("error", reject);
    });
    // Never attach a data listener: the socket remains paused and cannot
    // drain the multi-megabyte proxy response into user space.
    slowClient.write(
      `POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: ${Buffer.byteLength(body)}\r\nConnection: keep-alive\r\n\r\n${body}`,
    );
    await waitUntil(() =>
      value.upstreamMethods.includes("http:slowReaderResponse")
    );
    await delay(25);

    const rejected = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 14, method: "getSlot" }),
    });
    assert.equal(rejected.status, 503);
    assert.deepEqual(value.upstreamMethods, ["http:slowReaderResponse"]);

    slowClient.destroy();
    await delay(25);
    const accepted = await fetch(`http://127.0.0.1:${value.proxyPort}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 15, method: "getSlot" }),
    });
    assert.equal(accepted.status, 200);
  });

  it("sets conservative receiver limits and rejects unsafe overrides", () => {
    const config = rpcProxyConfigFromEnv({});
    assert.equal(config.maxBatchItems, 100);
    assert.equal(config.wsMaxFragments, 128);
    assert.equal(config.wsMaxBufferedChunks, 256);
    assert.equal(config.wsMaxBufferedBytes, 2_097_152);
    assert.equal(config.wsMaxClients, 64);
    assert.equal(config.wsClientMaxOperationsPerWindow, 256);
    assert.equal(config.wsGlobalMaxOperationsPerWindow, 4_096);
    assert.equal(config.wsOperationWindowMs, 1_000);
    assert.equal(config.httpMaxBodyBytes, 1_048_576);
    assert.equal(config.httpMaxResponseBytes, 4_194_304);
    assert.equal(config.httpMaxInFlightRequests, 32);
    assert.equal(config.healthProbeTimeoutMs, 5_000);
    assert.equal(config.healthCacheMs, 5_000);
    assert.throws(
      () => rpcProxyConfigFromEnv({ FORK_RPC_PROXY_WS_MAX_FRAGMENTS: "1025" }),
      /no greater than 1024/,
    );
    assert.throws(
      () => rpcProxyConfigFromEnv({ FORK_RPC_PROXY_WS_MAX_CLIENTS: "0" }),
      /positive safe integer/,
    );
    assert.throws(
      () => rpcProxyConfigFromEnv({ FORK_RPC_PROXY_MAX_BATCH_ITEMS: "1001" }),
      /no greater than 1000/,
    );
    assert.throws(
      () => rpcProxyConfigFromEnv({ FORK_RPC_PROXY_WS_OPERATION_WINDOW_MS: "60001" }),
      /no greater than 60000/,
    );
  });

  it("requires a nonblank admin secret only for hosted proxy images", () => {
    assert.doesNotThrow(() => rpcProxyConfigFromEnv({}));
    assert.throws(
      () => rpcProxyConfigFromEnv({ FORK_REQUIRE_ADMIN_TOKEN: "true" }),
      /requires a nonblank FORK_ADMIN_TOKEN/,
    );
    assert.throws(
      () => rpcProxyConfigFromEnv({
        FORK_REQUIRE_ADMIN_TOKEN: "true",
        FORK_ADMIN_TOKEN: "\t",
      }),
      /requires a nonblank FORK_ADMIN_TOKEN/,
    );
    assert.equal(
      rpcProxyConfigFromEnv({
        FORK_REQUIRE_ADMIN_TOKEN: "true",
        FORK_ADMIN_TOKEN: "hosted-secret",
      }).adminToken,
      "hosted-secret",
    );
  });
});
