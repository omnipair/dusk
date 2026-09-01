import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const PLATFORM_TOOLS_VERSION = "v1.54";
const TARGET = "sbf-solana-solana";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const platformToolsRoot =
  process.env.DUSK_PLATFORM_TOOLS_DIR ??
  join(homedir(), ".cache", "solana", PLATFORM_TOOLS_VERSION, "platform-tools");
const llvmBin = join(platformToolsRoot, "llvm", "bin");
const clang = ["clang-20", "clang"]
  .map((name) => join(llvmBin, name))
  .find(existsSync);
const linker = join(llvmBin, "lld");

if (!clang || !existsSync(linker)) {
  throw new Error(
    `Solana platform-tools ${PLATFORM_TOOLS_VERSION} are missing. ` +
      `Run: cargo build-sbf --install-only --tools-version ${PLATFORM_TOOLS_VERSION}`,
  );
}

const source = join(repoRoot, "emergency-halt", "abort.s");
const linkerScript = join(repoRoot, "emergency-halt", "abort.ld");
const objectDir = join(repoRoot, "target", "emergency-halt");
const deployDir = join(repoRoot, "target", "deploy");
const object = join(objectDir, "dusk_emergency_halt.o");
const output = join(deployDir, "dusk_emergency_halt.so");

mkdirSync(objectDir, { recursive: true });
mkdirSync(deployDir, { recursive: true });

execFileSync(clang, ["-target", TARGET, "-c", source, "-o", object], {
  cwd: repoRoot,
  stdio: "inherit",
});
execFileSync(
  linker,
  ["-flavor", "gnu", "--shared", "-T", linkerScript, "-o", output, object],
  { cwd: repoRoot, stdio: "inherit" },
);

console.log(
  `Built ${relative(repoRoot, output)} (${statSync(output).size} bytes) with platform-tools ${PLATFORM_TOOLS_VERSION}.`,
);
