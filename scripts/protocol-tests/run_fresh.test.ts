import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { afterEach, describe, it } from "mocha";

import {
  processGroupIsAlive,
  stopProcessGroup,
} from "./run_fresh.js";

const processGroups = new Set<number>();

afterEach(() => {
  for (const processGroupId of processGroups) {
    if (!processGroupIsAlive(processGroupId)) continue;
    try {
      process.kill(-processGroupId, "SIGKILL");
    } catch {
      // The regression cleanup is best effort after an assertion failure.
    }
  }
  processGroups.clear();
});

describe("fresh protocol runner process-group cleanup", function () {
  this.timeout(10_000);

  it("kills a stubborn descendant after its detached leader exits", async () => {
    const descendantSource = [
      "process.on('SIGTERM', () => {});",
      "process.send?.('ready');",
      "setInterval(() => {}, 1000);",
    ].join("");
    const leaderSource = [
      "const { spawn } = require('node:child_process');",
      `const child = spawn(process.execPath, ['-e', ${JSON.stringify(descendantSource)}], `,
      "{ detached: false, stdio: ['ignore', 'ignore', 'ignore', 'ipc'] });",
      "child.once('message', () => {",
      "  child.disconnect();",
      "  process.stdout.write(String(child.pid) + '\\n', () => process.exit(0));",
      "});",
    ].join("");
    const leader = spawn(process.execPath, ["-e", leaderSource], {
      detached: true,
      stdio: ["ignore", "pipe", "ignore"],
    });
    assert.ok(leader.pid);
    processGroups.add(leader.pid);
    const exitPromise = once(leader, "exit");
    const [chunk] = await once(leader.stdout!, "data");
    const descendantPid = Number(Buffer.from(chunk as Buffer).toString("utf8").trim());
    assert.ok(Number.isSafeInteger(descendantPid) && descendantPid > 0);
    await exitPromise;

    assert.equal(leader.exitCode, 0);
    assert.equal(processGroupIsAlive(leader.pid), true);
    await stopProcessGroup({ pid: leader.pid }, 1_000);
    assert.equal(processGroupIsAlive(leader.pid), false);
    processGroups.delete(leader.pid);
  });
});
