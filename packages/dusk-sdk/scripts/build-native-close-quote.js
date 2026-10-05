import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const sdk = fileURLToPath(new URL("..", import.meta.url));
const root = path.join(sdk, "../..");
execFileSync("cargo", ["build", "--locked", "--release", "--target", "wasm32-unknown-unknown", "-p", "dusk-native-close-quote"], {
  cwd: root,
  stdio: "inherit",
});
mkdirSync(path.join(sdk, "dist"), { recursive: true });
copyFileSync(
  path.join(root, "target/wasm32-unknown-unknown/release/dusk_native_close_quote.wasm"),
  path.join(sdk, "dist/native-close-quote.wasm")
);
