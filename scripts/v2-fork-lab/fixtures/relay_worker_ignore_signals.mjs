// Test fixture: deliberately never acknowledges IPC and ignores graceful
// termination so the parent must complete its bounded SIGKILL ladder.
process.on("message", () => undefined);
process.on("SIGINT", () => undefined);
process.on("SIGTERM", () => undefined);
setInterval(() => undefined, 1_000);
