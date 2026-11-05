import { rmSync } from "node:fs";

// TypeScript does not remove outputs for deleted source modules. Always build
// the package from a clean generated directory so retired APIs cannot ship.
rmSync(new URL("../dist/", import.meta.url), { recursive: true, force: true });
