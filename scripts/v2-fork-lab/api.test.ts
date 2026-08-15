import assert from "node:assert/strict";
import http from "node:http";
import { afterEach, describe, it } from "mocha";

import {
  createForkApiServer,
  forkApiConfigFromEnv,
  type ForkApiDependencies,
  type ForkApiRuntime,
} from "./api.js";

interface Fixture {
  runtime: ForkApiRuntime;
  port: number;
}

const fixtures: Fixture[] = [];

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
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

async function fixture(
  env: NodeJS.ProcessEnv,
  dependencies: ForkApiDependencies,
): Promise<Fixture> {
  const runtime = createForkApiServer(forkApiConfigFromEnv(env), dependencies);
  const port = await listen(runtime.server);
  const value = { runtime, port };
  fixtures.push(value);
  return value;
}

async function incompleteRequest(port: number): Promise<{
  status: number;
  payload: Record<string, unknown>;
}> {
  return new Promise((resolvePromise, reject) => {
    const request = http.request({
      host: "127.0.0.1",
      port,
      method: "POST",
      path: "/incomplete",
      headers: {
        "content-length": "2",
        "content-type": "application/json",
      },
    });
    request.once("error", reject);
    request.once("response", (response) => {
      const chunks: Buffer[] = [];
      response.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
      response.once("end", () => {
        request.destroy();
        resolvePromise({
          status: response.statusCode ?? 0,
          payload: JSON.parse(Buffer.concat(chunks).toString("utf8")) as Record<string, unknown>,
        });
      });
    });
    request.write("{");
  });
}

async function chunkedRequest(
  port: number,
  path: string,
  chunks: string[],
): Promise<{ status: number; payload: Record<string, unknown> }> {
  return new Promise((resolvePromise, reject) => {
    const request = http.request({
      host: "127.0.0.1",
      port,
      method: "POST",
      path,
      headers: { "content-type": "application/json" },
    });
    request.once("error", reject);
    request.once("response", (response) => {
      const responseChunks: Buffer[] = [];
      response.on("data", (chunk) => responseChunks.push(Buffer.from(chunk)));
      response.once("end", () => {
        resolvePromise({
          status: response.statusCode ?? 0,
          payload: JSON.parse(Buffer.concat(responseChunks).toString("utf8")) as Record<string, unknown>,
        });
      });
    });
    for (const chunk of chunks) request.write(chunk);
    request.end();
  });
}

afterEach(async () => {
  while (fixtures.length > 0) {
    await fixtures.pop()?.runtime.close();
  }
});

describe("public Dusk fork API HTTP limits", function () {
  this.timeout(5_000);

  it("rejects oversized bodies before parsing or routing", async () => {
    let routed = false;
    const value = await fixture(
      { FORK_API_MAX_BODY_BYTES: "32" },
      {
        requireForkAdminAuthorization() {},
        async route() {
          routed = true;
          return { success: true };
        },
      },
    );
    const response = await chunkedRequest(
      value.port,
      "/oversized",
      ["x".repeat(16), "x".repeat(17)],
    );
    assert.equal(response.status, 413);
    assert.equal(response.payload.code, "fork_api_body_too_large");
    assert.equal(routed, false);
  });

  it("rejects malformed JSON without crashing the API process", async () => {
    let routed = false;
    const value = await fixture(
      {},
      {
        requireForkAdminAuthorization() {},
        async route() {
          routed = true;
          return { success: true };
        },
      },
    );
    const response = await chunkedRequest(value.port, "/malformed", ["{"]);
    assert.equal(response.status, 400);
    assert.match(String(response.payload.error), /JSON/);
    assert.equal(routed, false);

    const healthy = await fetch(`http://127.0.0.1:${value.port}/healthy`);
    assert.equal(healthy.status, 200);
  });

  it("returns 408 when a client stalls before completing its body", async () => {
    let routed = false;
    const value = await fixture(
      { FORK_API_BODY_TIMEOUT_MS: "30" },
      {
        requireForkAdminAuthorization() {},
        async route() {
          routed = true;
          return { success: true };
        },
      },
    );
    const response = await incompleteRequest(value.port);
    assert.equal(response.status, 408);
    assert.equal(response.payload.code, "fork_api_body_timeout");
    assert.equal(routed, false);
  });

  it("times out a stalled route and holds its in-flight slot until it settles", async () => {
    let releaseRoute: (() => void) | undefined;
    const stalledRoute = new Promise<void>((resolvePromise) => {
      releaseRoute = resolvePromise;
    });
    const dependencies: ForkApiDependencies = {
      requireForkAdminAuthorization() {},
      async route(req) {
        if (req.url === "/stalled") await stalledRoute;
        return { success: true };
      },
    };
    const value = await fixture(
      {
        FORK_API_MAX_IN_FLIGHT_REQUESTS: "1",
        FORK_API_ROUTE_TIMEOUT_MS: "30",
      },
      dependencies,
    );

    const timedOut = await fetch(`http://127.0.0.1:${value.port}/stalled`, {
      method: "POST",
      body: "{}",
    });
    const timedOutPayload = await timedOut.json() as {
      code: string;
      uncertainOutcome: boolean;
    };
    assert.equal(timedOut.status, 504);
    assert.equal(timedOutPayload.code, "fork_api_route_timeout");
    assert.equal(timedOutPayload.uncertainOutcome, true);

    const saturated = await fetch(`http://127.0.0.1:${value.port}/healthy`);
    assert.equal(saturated.status, 503);
    assert.equal(
      (await saturated.json() as { code: string }).code,
      "fork_api_request_limit",
    );

    releaseRoute?.();
    await delay(10);
    const recovered = await fetch(`http://127.0.0.1:${value.port}/healthy`);
    assert.equal(recovered.status, 200);
  });

  it("rejects unsafe limit configuration", () => {
    const defaults = forkApiConfigFromEnv({});
    assert.equal(defaults.maxBodyBytes, 1_048_576);
    assert.equal(defaults.maxInFlightRequests, 32);
    assert.equal(defaults.bodyTimeoutMs, 15_000);
    assert.equal(defaults.routeTimeoutMs, 120_000);
    assert.throws(
      () => forkApiConfigFromEnv({ FORK_API_MAX_IN_FLIGHT_REQUESTS: "0" }),
      /positive safe integer/,
    );
    assert.throws(
      () => forkApiConfigFromEnv({ FORK_API_ROUTE_TIMEOUT_MS: "600001" }),
      /no greater than 600000/,
    );
  });

  it("requires an explicit browser-reachable RPC URL only in hosted mode", () => {
    const local = forkApiConfigFromEnv({
      SURFPOOL_RPC_URL: "http://127.0.0.1:8899",
    });
    assert.equal(local.publicRpcUrl, local.surfpoolRpcUrl);

    assert.throws(
      () => forkApiConfigFromEnv({
        DUSK_REQUIRE_PUBLIC_RPC_URL: "true",
        SURFPOOL_RPC_URL: "http://v2-surfpool-rpc.railway.internal:8899",
        PUBLIC_SURFPOOL_RPC_URL: "  ",
      }),
      /requires an explicit PUBLIC_SURFPOOL_RPC_URL or SURFPOOL_RPC_PROXY_URL/,
    );

    const hosted = forkApiConfigFromEnv({
      DUSK_REQUIRE_PUBLIC_RPC_URL: "true",
      SURFPOOL_RPC_URL: "http://v2-surfpool-rpc.railway.internal:8899",
      SURFPOOL_RPC_PROXY_URL: "https://fork-rpc.example.test",
    });
    assert.equal(hosted.publicRpcUrl, "https://fork-rpc.example.test");

    assert.throws(
      () => forkApiConfigFromEnv({
        DUSK_REQUIRE_PUBLIC_RPC_URL: "true",
        PUBLIC_SURFPOOL_RPC_URL: "ws://fork-rpc.example.test",
      }),
      /must use http: or https:/,
    );
  });

  it("requires a nonblank admin secret only for hosted API images", () => {
    assert.doesNotThrow(() => forkApiConfigFromEnv({}));
    assert.throws(
      () => forkApiConfigFromEnv({ FORK_REQUIRE_ADMIN_TOKEN: "true" }),
      /requires a nonblank FORK_ADMIN_TOKEN/,
    );
    assert.throws(
      () => forkApiConfigFromEnv({
        FORK_REQUIRE_ADMIN_TOKEN: "true",
        FORK_ADMIN_TOKEN: "   ",
      }),
      /requires a nonblank FORK_ADMIN_TOKEN/,
    );
    assert.doesNotThrow(() => forkApiConfigFromEnv({
      FORK_REQUIRE_ADMIN_TOKEN: "true",
      FORK_ADMIN_TOKEN: "hosted-secret",
    }));
  });
});
