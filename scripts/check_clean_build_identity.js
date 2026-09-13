import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { prepareBuild } from "./prepare_build.js";

const root = fileURLToPath(new URL("..", import.meta.url));
const scripts = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).scripts;
for (const command of ["build:litesvm", "v2:build-devnet", "v2:build-leverage-delegate-devnet", "v2:build-faucet-devnet"]) {
  assert.ok(scripts[command].startsWith("node scripts/prepare_build.js && "), `${command} must prepare the devnet workspace before Anchor`);
}
const workspace = mkdtempSync(join(tmpdir(), "dusk-build-identity-"));
const files = ["Anchor.toml", "Cargo.toml", ...["dusk", "leverage_delegate", "faucet"].flatMap(
  (program) => [`programs/${program}/Cargo.toml`, `programs/${program}/src/lib.rs`],
)];
try {
  for (const file of files) {
    mkdirSync(dirname(join(workspace, file)), { recursive: true });
    copyFileSync(join(root, file), join(workspace, file));
  }
  prepareBuild(workspace);
  // keys list exercises Config::discover just like build, without compiling
  // or deploying. Any generated test keypairs stay in this disposable copy.
  execFileSync("anchor", ["keys", "list"], { cwd: workspace, stdio: "pipe" });
  for (const file of files) {
    assert.equal(readFileSync(join(workspace, file), "utf8"), readFileSync(join(root, file), "utf8"),
      `Anchor rewrote ${file} in a clean devnet checkout`);
  }
  console.log("Clean devnet build setup preserves every program identity.");
} finally {
  rmSync(workspace, { recursive: true, force: true });
}
