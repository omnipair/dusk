import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export function prepareBuild(root) {
  // Anchor 0.31.1 Config::discover runs keys_sync when both target/deploy
  // and [programs.localnet] are absent. A clean devnet-only checkout must
  // create this directory before invoking any Anchor command, or its source
  // declarations and devnet addresses are replaced with generated identities.
  for (const directory of ["deploy", "idl", "types"]) {
    mkdirSync(resolve(root, "target", directory), { recursive: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  prepareBuild(resolve(fileURLToPath(new URL("..", import.meta.url))));
}
