import { fork } from "node:child_process";
import { once } from "node:events";
import http from "node:http";
import { connect, createServer as createTcpServer, } from "node:net";
import { fileURLToPath, pathToFileURL } from "node:url";
import WebSocket from "ws";
const DEFAULT_CHILD_SHUTDOWN_GRACE_MS = 2_000;
function saturatedIncrement(value) {
    return Math.min(Number.MAX_SAFE_INTEGER, value + 1);
}
function targetAddress(targetUrl) {
    const target = new URL(targetUrl);
    const defaults = {
        "http:": 80,
        "https:": 443,
        "ws:": 80,
        "wss:": 443,
    };
    const fallback = defaults[target.protocol];
    if (!fallback)
        throw new Error(`Unsupported TCP proxy target: ${target.protocol}`);
    return { host: target.hostname, port: Number(target.port || fallback) };
}
function isLocalTargetHost(hostname, listenHost) {
    const normalized = hostname.replace(/^\[|\]$/g, "").toLowerCase();
    const normalizedListen = listenHost.replace(/^\[|\]$/g, "").toLowerCase();
    const localHosts = new Set(["localhost", "127.0.0.1", "::1", "0.0.0.0", "::"]);
    return normalized === normalizedListen ||
        (localHosts.has(normalized) && localHosts.has(normalizedListen));
}
function rejectRelaySelfLoop(targetUrl, listenHost, listenPort, label) {
    const target = new URL(targetUrl);
    const address = targetAddress(targetUrl);
    if (address.port === listenPort && isLocalTargetHost(target.hostname, listenHost)) {
        throw new Error(`${label} target must not point back to its stable relay listener`);
    }
}
async function probeRawRpcHealth(rpcUrl, timeoutMs) {
    const response = await fetch(rpcUrl, {
        method: "POST",
        headers: {
            "content-type": "application/json",
        },
        body: JSON.stringify({
            jsonrpc: "2.0",
            id: "dusk-relay-target-health",
            method: "getHealth",
        }),
        signal: AbortSignal.timeout(timeoutMs),
    });
    if (!response.ok) {
        await response.body?.cancel();
        throw new Error("raw RPC target health probe returned a non-success status");
    }
    const contentLength = Number(response.headers.get("content-length") ?? "0");
    if (Number.isFinite(contentLength) && contentLength > 4_096) {
        await response.body?.cancel();
        throw new Error("raw RPC target health probe response is too large");
    }
    const body = await response.text();
    if (Buffer.byteLength(body) > 4_096) {
        throw new Error("raw RPC target health probe response is too large");
    }
    let payload;
    try {
        payload = JSON.parse(body);
    }
    catch {
        throw new Error("raw RPC target health probe returned invalid JSON");
    }
    if (!payload ||
        typeof payload !== "object" ||
        payload.result !== "ok" ||
        "error" in payload) {
        throw new Error("raw RPC target health probe returned an unhealthy result");
    }
}
async function probeRawWebSocket(wsUrl, timeoutMs) {
    await new Promise((resolvePromise, rejectPromise) => {
        let settled = false;
        let socket;
        const finish = (error) => {
            if (settled)
                return;
            settled = true;
            clearTimeout(timer);
            if (socket) {
                socket.removeAllListeners();
                socket.on("error", () => undefined);
                if (socket.readyState === WebSocket.OPEN)
                    socket.close();
                else if (socket.readyState !== WebSocket.CLOSED)
                    socket.terminate();
            }
            if (error)
                rejectPromise(error);
            else
                resolvePromise();
        };
        const timer = setTimeout(() => finish(new Error("raw WebSocket target health probe timed out")), timeoutMs);
        timer.unref();
        try {
            socket = new WebSocket(wsUrl, {
                followRedirects: false,
                handshakeTimeout: timeoutMs,
                perMessageDeflate: false,
            });
        }
        catch (error) {
            finish(error instanceof Error ? error : new Error(String(error)));
            return;
        }
        socket.once("open", () => finish());
        socket.once("error", (error) => finish(error));
        socket.once("unexpected-response", (_request, response) => {
            finish(new Error(`raw WebSocket target rejected upgrade with HTTP ${response.statusCode}`));
        });
        socket.once("close", () => {
            if (!settled)
                finish(new Error("raw WebSocket target closed before upgrade"));
        });
    });
}
/** Probes both dynamic Surfpool transports concurrently and within one bound. */
export async function probeRawSurfpoolTargets(targets, timeoutMs) {
    await Promise.all([
        probeRawRpcHealth(targets.rpcUrl, timeoutMs),
        probeRawWebSocket(targets.wsUrl, timeoutMs),
    ]);
}
function listen(server, host, port) {
    return new Promise((resolvePromise, rejectPromise) => {
        const onError = (error) => {
            server.off("listening", onListening);
            rejectPromise(error);
        };
        const onListening = () => {
            server.off("error", onError);
            const address = server.address();
            if (!address || typeof address === "string") {
                rejectPromise(new Error("TCP proxy did not expose an IP socket"));
                return;
            }
            resolvePromise(address.port);
        };
        server.once("error", onError);
        server.once("listening", onListening);
        server.listen({ host, port });
    });
}
/**
 * A byte-for-byte relay for Surfpool's loopback RPC servers.
 *
 * Listener failures are fatal. A reset/refusal on one upstream connection is
 * connection-local: it closes that pair and increments metrics, allowing a
 * subsequent API or wallet request to reconnect normally.
 */
export async function createTcpProxy(params) {
    const target = targetAddress(params.targetUrl);
    const connectTimeoutMs = params.connectTimeoutMs ?? 5_000;
    const maxConnections = params.maxConnections ?? 256;
    if (!Number.isSafeInteger(connectTimeoutMs) || connectTimeoutMs < 1) {
        throw new Error("TCP proxy connect timeout must be a positive safe integer");
    }
    if (!Number.isSafeInteger(maxConnections) || maxConnections < 1) {
        throw new Error("TCP proxy max connections must be a positive safe integer");
    }
    const sockets = new Set();
    let acceptedConnections = 0;
    let activeConnections = 0;
    let downstreamErrors = 0;
    let upstreamErrors = 0;
    let rejectFailure = () => undefined;
    const failure = new Promise((_resolve, reject) => {
        rejectFailure = reject;
    });
    void failure.catch(() => undefined);
    let failed = false;
    let closing;
    const reportListenerFailure = (error) => {
        if (closing || failed)
            return;
        failed = true;
        rejectFailure(new Error(`${params.label} stable TCP proxy listener failed: ${error.message}`));
    };
    const server = createTcpServer((downstream) => {
        acceptedConnections = saturatedIncrement(acceptedConnections);
        activeConnections = saturatedIncrement(activeConnections);
        let downstreamClosed = false;
        const upstream = connect(target);
        const connectTimer = setTimeout(() => {
            upstream.destroy(new Error(`${params.label} upstream connect timed out after ${connectTimeoutMs}ms`));
        }, connectTimeoutMs);
        connectTimer.unref();
        sockets.add(downstream);
        sockets.add(upstream);
        downstream.setNoDelay(true);
        downstream.setKeepAlive(true);
        upstream.setNoDelay(true);
        upstream.setKeepAlive(true);
        upstream.once("connect", () => clearTimeout(connectTimer));
        downstream.on("error", () => {
            downstreamErrors = saturatedIncrement(downstreamErrors);
            upstream.destroy();
        });
        upstream.on("error", () => {
            upstreamErrors = saturatedIncrement(upstreamErrors);
            downstream.destroy();
        });
        downstream.on("close", () => {
            sockets.delete(downstream);
            if (!downstreamClosed) {
                downstreamClosed = true;
                activeConnections = Math.max(0, activeConnections - 1);
            }
        });
        upstream.on("close", () => {
            clearTimeout(connectTimer);
            sockets.delete(upstream);
        });
        downstream.pipe(upstream).pipe(downstream);
    });
    server.maxConnections = maxConnections;
    const snapshot = () => ({
        acceptedConnections,
        activeConnections,
        downstreamErrors,
        upstreamErrors,
    });
    const close = () => {
        if (closing)
            return closing;
        closing = new Promise((resolvePromise) => {
            for (const socket of sockets)
                socket.destroy();
            if (!server.listening) {
                resolvePromise();
                return;
            }
            server.close(() => resolvePromise());
        });
        return closing;
    };
    try {
        const port = await listen(server, params.listenHost, params.listenPort);
        server.on("error", reportListenerFailure);
        server.on("close", () => {
            if (!closing) {
                reportListenerFailure(new Error("listener closed unexpectedly"));
            }
        });
        return {
            host: params.listenHost,
            port,
            targetUrl: params.targetUrl,
            failure,
            snapshot,
            close,
        };
    }
    catch (error) {
        await close();
        throw error;
    }
}
function localProxyUrl(protocol, host, port) {
    const localHost = host === "0.0.0.0" ? "127.0.0.1" : host === "::" ? "[::1]" : host;
    return `${protocol}://${localHost}:${port}`;
}
function sendHealthJson(response, statusCode, payload, headOnly) {
    const body = Buffer.from(JSON.stringify(payload));
    response.writeHead(statusCode, {
        "cache-control": "no-store",
        connection: "close",
        "content-length": String(body.byteLength),
        "content-type": "application/json; charset=utf-8",
        "x-content-type-options": "nosniff",
    });
    response.end(headOnly ? undefined : body);
}
async function listenHttp(server, host, port) {
    return new Promise((resolvePromise, reject) => {
        const onError = (error) => {
            server.off("listening", onListening);
            reject(error);
        };
        const onListening = () => {
            server.off("error", onError);
            const address = server.address();
            if (!address || typeof address === "string") {
                reject(new Error("Relay health server did not expose an IP socket"));
                return;
            }
            resolvePromise(address.port);
        };
        server.once("error", onError);
        server.once("listening", onListening);
        server.listen({ host, port });
    });
}
function sendToParent(message) {
    if (!process.connected || !process.send)
        return;
    try {
        process.send(message);
    }
    catch {
        // The IPC disconnect handler owns shutdown when the controller is gone.
    }
}
async function runRelayWorker() {
    let config;
    let healthServer;
    let rpcProxy;
    let wsProxy;
    let status = "starting";
    let startedAt = new Date().toISOString();
    let readyAt = null;
    let programCount = 0;
    let lastHeartbeat;
    let targetProbeTimer;
    let targetProbeInFlight;
    let targetProbeAttempts = 0;
    let targetProbeSuccesses = 0;
    let targetProbeFailures = 0;
    let targetProbeConsecutiveFailures = 0;
    let targetProbeLastProbeAt = null;
    let targetProbeLastSuccessAt = null;
    let targetProbeLastFailureAt = null;
    let targetProbeHealthy = false;
    let closing;
    const targetProbeSnapshot = () => ({
        attempts: targetProbeAttempts,
        successes: targetProbeSuccesses,
        failures: targetProbeFailures,
        consecutiveFailures: targetProbeConsecutiveFailures,
        inFlight: Boolean(targetProbeInFlight),
        lastProbeAt: targetProbeLastProbeAt,
        lastSuccessAt: targetProbeLastSuccessAt,
        lastFailureAt: targetProbeLastFailureAt,
    });
    const close = (exitCode) => {
        if (closing)
            return closing;
        status = status === "failed" ? "failed" : "stopping";
        closing = (async () => {
            if (targetProbeTimer)
                clearTimeout(targetProbeTimer);
            targetProbeTimer = undefined;
            await Promise.allSettled([targetProbeInFlight]);
            await Promise.allSettled([rpcProxy?.close(), wsProxy?.close()]);
            if (healthServer?.listening) {
                await new Promise((resolvePromise) => {
                    healthServer?.close(() => resolvePromise());
                    healthServer?.closeAllConnections();
                });
            }
            process.exitCode = exitCode;
        })();
        return closing;
    };
    const fatal = (cause) => {
        const error = cause instanceof Error ? cause : new Error(String(cause));
        if (status === "failed" || status === "stopping")
            return;
        status = "failed";
        sendToParent({ type: "fatal", error: error.message });
        const exitDelay = config
            ? Math.min(config.healthRequestTimeoutMs, 250)
            : 0;
        setTimeout(() => {
            void close(1).finally(() => process.exit(1));
        }, exitDelay).unref();
    };
    const scheduleTargetProbe = (targets, delayMs) => {
        if (!config || closing || status === "failed" || status === "stopping")
            return;
        if (targetProbeTimer)
            clearTimeout(targetProbeTimer);
        targetProbeTimer = setTimeout(() => {
            targetProbeTimer = undefined;
            if (targetProbeInFlight || !config)
                return;
            targetProbeAttempts = saturatedIncrement(targetProbeAttempts);
            targetProbeLastProbeAt = new Date().toISOString();
            const probe = probeRawSurfpoolTargets(targets, config.targetProbeTimeoutMs).then(() => {
                targetProbeSuccesses = saturatedIncrement(targetProbeSuccesses);
                targetProbeConsecutiveFailures = 0;
                targetProbeHealthy = true;
                targetProbeLastSuccessAt = new Date().toISOString();
            }, (error) => {
                targetProbeFailures = saturatedIncrement(targetProbeFailures);
                targetProbeConsecutiveFailures = saturatedIncrement(targetProbeConsecutiveFailures);
                targetProbeLastFailureAt = new Date().toISOString();
                if (config &&
                    targetProbeConsecutiveFailures >= config.targetProbeFailureThreshold) {
                    targetProbeHealthy = false;
                    fatal(new Error(`raw Surfpool target failed ${targetProbeConsecutiveFailures} consecutive bounded probes`, { cause: error }));
                }
            }).finally(() => {
                targetProbeInFlight = undefined;
                if (config && !closing && status !== "failed" && status !== "stopping") {
                    scheduleTargetProbe(targets, config.targetProbeIntervalMs);
                }
            });
            targetProbeInFlight = probe;
            void probe.catch(fatal);
        }, delayMs);
        targetProbeTimer.unref();
    };
    process.on("message", (raw) => {
        void (async () => {
            if (!raw || typeof raw !== "object" || !("type" in raw))
                return;
            if (raw.type === "start") {
                if (config)
                    throw new Error("Surfpool relay worker was started twice");
                config = raw.config;
                startedAt = new Date().toISOString();
                healthServer = http.createServer((request, response) => {
                    const headOnly = request.method === "HEAD";
                    let pathname;
                    try {
                        pathname = request.url
                            ? new URL(request.url, "http://relay-health.invalid").pathname
                            : "/";
                    }
                    catch {
                        sendHealthJson(response, 400, { ok: false, status: "bad_request" }, headOnly);
                        return;
                    }
                    if (pathname !== "/health") {
                        sendHealthJson(response, 404, { ok: false, status: "not_found" }, headOnly);
                        return;
                    }
                    if (request.method !== "GET" && request.method !== "HEAD") {
                        response.setHeader("allow", "GET, HEAD");
                        sendHealthJson(response, 405, { ok: false, status: "method_not_allowed" }, headOnly);
                        return;
                    }
                    const heartbeatAgeMs = lastHeartbeat
                        ? Math.max(0, Date.now() - lastHeartbeat.atMs)
                        : null;
                    const heartbeatFresh = heartbeatAgeMs !== null &&
                        heartbeatAgeMs <= raw.config.heartbeatStaleMs;
                    const targetsActive = Boolean(rpcProxy && wsProxy);
                    const ok = status === "ready" &&
                        targetsActive &&
                        heartbeatFresh &&
                        targetProbeHealthy;
                    sendHealthJson(response, ok ? 200 : 503, {
                        ok,
                        status,
                        startedAt,
                        readyAt,
                        programCount,
                        targetsActive,
                        heartbeatFresh,
                        heartbeatAgeMs,
                        targetProbeHealthy,
                        targetProbe: targetProbeSnapshot(),
                        eventDrain: lastHeartbeat ?? null,
                        rpc: rpcProxy?.snapshot() ?? null,
                        websocket: wsProxy?.snapshot() ?? null,
                    }, headOnly);
                });
                healthServer.requestTimeout = raw.config.healthRequestTimeoutMs;
                healthServer.headersTimeout = raw.config.healthRequestTimeoutMs;
                healthServer.keepAliveTimeout = 1_000;
                healthServer.maxRequestsPerSocket = 1;
                healthServer.maxConnections = raw.config.healthMaxConnections;
                healthServer.on("clientError", (_error, socket) => socket.destroy());
                const healthPort = await listenHttp(healthServer, raw.config.healthHost, raw.config.healthPort);
                healthServer.on("error", fatal);
                sendToParent({ type: "started", healthPort });
                return;
            }
            if (!config)
                throw new Error("Surfpool relay worker is not started");
            if (raw.type === "activate") {
                if (rpcProxy || wsProxy)
                    throw new Error("Surfpool relay targets were activated twice");
                rejectRelaySelfLoop(raw.targets.rpcUrl, config.listenHost, config.rpcPort, "Surfpool RPC");
                rejectRelaySelfLoop(raw.targets.wsUrl, config.listenHost, config.wsPort, "Surfpool WebSocket");
                let rpc;
                let websocket;
                try {
                    rpc = await createTcpProxy({
                        label: "Surfpool RPC",
                        listenHost: config.listenHost,
                        listenPort: config.rpcPort,
                        targetUrl: raw.targets.rpcUrl,
                        connectTimeoutMs: config.upstreamConnectTimeoutMs,
                        maxConnections: config.maxConnections,
                    });
                    websocket = await createTcpProxy({
                        label: "Surfpool WebSocket",
                        listenHost: config.listenHost,
                        listenPort: config.wsPort,
                        targetUrl: raw.targets.wsUrl,
                        connectTimeoutMs: config.upstreamConnectTimeoutMs,
                        maxConnections: config.maxConnections,
                    });
                }
                catch (error) {
                    // Activation is transactional: never leave one stable listener alive
                    // after its peer failed to bind.
                    await Promise.allSettled([rpc?.close(), websocket?.close()]);
                    throw error;
                }
                rpcProxy = rpc;
                wsProxy = websocket;
                void Promise.race([rpc.failure, websocket.failure]).catch(fatal);
                scheduleTargetProbe(raw.targets, 0);
                sendToParent({
                    type: "activated",
                    rpcPort: rpc.port,
                    wsPort: websocket.port,
                });
                return;
            }
            if (raw.type === "heartbeat") {
                lastHeartbeat = raw.value;
                return;
            }
            if (raw.type === "ready") {
                if (!rpcProxy || !wsProxy)
                    throw new Error("Relay cannot become ready before activation");
                if (!Number.isSafeInteger(raw.programCount) || raw.programCount < 1) {
                    throw new Error("Relay readiness program count must be positive");
                }
                programCount = raw.programCount;
                readyAt = new Date().toISOString();
                status = "ready";
                return;
            }
            if (raw.type === "failed") {
                status = "failed";
                return;
            }
            if (raw.type === "stopping") {
                if (status !== "failed")
                    status = "stopping";
                return;
            }
            if (raw.type === "shutdown") {
                await close(0);
                process.exit(0);
            }
        })().catch(fatal);
    });
    process.once("disconnect", () => {
        void close(0).finally(() => process.exit(0));
    });
    process.once("SIGINT", () => {
        void close(0).finally(() => process.exit(0));
    });
    process.once("SIGTERM", () => {
        void close(0).finally(() => process.exit(0));
    });
}
function sendChild(child, message) {
    if (!child.connected)
        return;
    try {
        child.send(message, (error) => {
            if (error && child.connected)
                child.kill("SIGTERM");
        });
    }
    catch {
        if (child.connected)
            child.kill("SIGTERM");
    }
}
function timeoutFailure(label, milliseconds) {
    return new Promise((_resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`${label} timed out after ${milliseconds}ms`)), milliseconds);
        timer.unref();
    });
}
function relayChildExited(child) {
    return child.exitCode !== null || child.signalCode !== null;
}
async function waitForRelayChildExit(child, timeoutMs) {
    if (relayChildExited(child))
        return true;
    let timer;
    try {
        return await Promise.race([
            once(child, "exit").then(() => true),
            new Promise((resolvePromise) => {
                timer = setTimeout(() => resolvePromise(false), timeoutMs);
                timer.unref();
            }),
        ]);
    }
    finally {
        if (timer)
            clearTimeout(timer);
    }
}
async function terminateRelayChild(child, shutdownGraceMs, requestGracefulShutdown) {
    if (relayChildExited(child))
        return;
    if (requestGracefulShutdown) {
        sendChild(child, { type: "shutdown" });
        if (await waitForRelayChildExit(child, shutdownGraceMs))
            return;
    }
    child.kill("SIGTERM");
    if (await waitForRelayChildExit(child, shutdownGraceMs))
        return;
    child.kill("SIGKILL");
    if (await waitForRelayChildExit(child, shutdownGraceMs))
        return;
    throw new Error(`Surfpool relay worker did not exit after SIGKILL within ${shutdownGraceMs}ms`);
}
export async function startSurfpoolRelayWorkerProcess(config, options = {}) {
    const modulePath = options.modulePath ?? fileURLToPath(import.meta.url);
    const startupTimeoutMs = options.startupTimeoutMs ?? 10_000;
    const shutdownGraceMs = options.shutdownGraceMs ?? DEFAULT_CHILD_SHUTDOWN_GRACE_MS;
    const child = fork(modulePath, [], {
        env: process.env,
        execArgv: process.execArgv,
        stdio: ["ignore", "inherit", "inherit", "ipc"],
    });
    options.onChildSpawn?.(child);
    let closing = false;
    let closePromise;
    let rejectFailure = () => undefined;
    const failure = new Promise((_resolve, reject) => {
        rejectFailure = reject;
    });
    void failure.catch(() => undefined);
    let resolveStarted = () => undefined;
    let rejectStarted = () => undefined;
    const started = new Promise((resolvePromise, reject) => {
        resolveStarted = resolvePromise;
        rejectStarted = reject;
    });
    let resolveActivated;
    let rejectActivated;
    child.on("message", (raw) => {
        if (!raw || typeof raw !== "object" || !("type" in raw))
            return;
        if (raw.type === "started") {
            resolveStarted(raw.healthPort);
            return;
        }
        if (raw.type === "activated") {
            resolveActivated?.({ rpcPort: raw.rpcPort, wsPort: raw.wsPort });
            resolveActivated = undefined;
            rejectActivated = undefined;
            return;
        }
        if (raw.type === "fatal") {
            const error = new Error(`Surfpool relay worker failed: ${raw.error}`);
            rejectStarted(error);
            rejectActivated?.(error);
            if (!closing)
                rejectFailure(error);
        }
    });
    child.once("error", (error) => {
        rejectStarted(error);
        rejectActivated?.(error);
        if (!closing)
            rejectFailure(error);
    });
    child.once("exit", (code, signal) => {
        const error = new Error(`Surfpool relay worker exited with ${signal ?? `code ${code ?? "unknown"}`}`);
        rejectStarted(error);
        rejectActivated?.(error);
        if (!closing)
            rejectFailure(error);
    });
    sendChild(child, { type: "start", config });
    let healthPort;
    try {
        healthPort = await Promise.race([
            started,
            timeoutFailure("Surfpool relay worker startup", startupTimeoutMs),
            failure,
        ]);
    }
    catch (error) {
        closing = true;
        try {
            await terminateRelayChild(child, shutdownGraceMs, true);
        }
        catch (shutdownError) {
            throw new AggregateError([error, shutdownError], "Surfpool relay worker startup failed and child termination also failed");
        }
        throw error;
    }
    const close = () => {
        if (closePromise)
            return closePromise;
        closing = true;
        closePromise = (async () => {
            await terminateRelayChild(child, shutdownGraceMs, true);
        })();
        return closePromise;
    };
    return {
        healthHost: config.healthHost,
        healthPort,
        failure,
        async activate(targets) {
            if (resolveActivated)
                throw new Error("Surfpool relay activation is already pending");
            const activated = new Promise((resolvePromise, reject) => {
                resolveActivated = resolvePromise;
                rejectActivated = reject;
            });
            sendChild(child, { type: "activate", targets });
            const ports = await Promise.race([
                activated,
                timeoutFailure("Surfpool relay activation", startupTimeoutMs),
                failure,
            ]);
            return {
                rpcUrl: localProxyUrl("http", config.listenHost, ports.rpcPort),
                wsUrl: localProxyUrl("ws", config.listenHost, ports.wsPort),
            };
        },
        heartbeat(value) {
            sendChild(child, { type: "heartbeat", value });
        },
        markReady(programCount) {
            sendChild(child, { type: "ready", programCount });
        },
        markFailed() {
            sendChild(child, { type: "failed" });
        },
        markStopping() {
            sendChild(child, { type: "stopping" });
        },
        close,
    };
}
if (process.argv[1] &&
    import.meta.url === pathToFileURL(process.argv[1]).href) {
    runRelayWorker().catch((error) => {
        console.error(error);
        process.exit(1);
    });
}
