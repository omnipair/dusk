import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

const trackedFiles = execFileSync("git", ["ls-files", "-z"], { encoding: "utf8" })
  .split("\0")
  .filter(Boolean);

const privateArtifactNames = [
  /\.env(?:\..+)?$/,
  /(?:^|\/)[^/]*-keypair\.json$/,
  /(?:^|\/).*\.pem$/,
  /(?:^|\/).*\.key$/,
  /(?:^|\/)id_rsa(?:\..*)?$/,
  /(?:^|\/)id_ed25519(?:\..*)?$/,
];

const allowedTrackedPrivateArtifacts = new Set([".env.example"]);

const localPathPatterns = [
  {
    label: "macOS user-home absolute path",
    pattern: new RegExp("\\/" + "Users" + "\\/[^/\\s\"'`]+"),
  },
  {
    label: "Linux user-home absolute path",
    pattern: new RegExp("\\/" + "home" + "\\/[^/\\s\"'`]+"),
  },
  {
    label: "macOS screencapture temp path",
    pattern: new RegExp("\\/" + "var" + "\\/" + "folders" + "\\/"),
  },
  {
    label: "macOS screencapture temporary path",
    pattern: new RegExp("Temporary" + "Items"),
  },
  {
    label: "local workspace path",
    pattern: new RegExp("Desktop" + "\\/" + "Repos" + "\\/"),
  },
];

const secretLiteralPatterns = [
  /ghp_[A-Za-z0-9_]{20,}/,
  /github_pat_[A-Za-z0-9_]{20,}/,
  /sk_live_[A-Za-z0-9]{20,}/,
  /sk_test_[A-Za-z0-9]{20,}/,
  /xox[baprs]-[A-Za-z0-9-]{20,}/,
  /AIza[0-9A-Za-z_-]{20,}/,
  /Bearer\s+[A-Za-z0-9._~+/=-]{20,}/,
];

const failures = [];

for (const file of trackedFiles) {
  if (!existsSync(file)) continue;

  const normalized = file.split(path.sep).join("/");
  if (
    !allowedTrackedPrivateArtifacts.has(normalized) &&
    privateArtifactNames.some((pattern) => pattern.test(normalized))
  ) {
    failures.push(`${file}: tracked private-key/env artifact filename`);
    continue;
  }

  const buffer = readFileSync(file);
  if (buffer.includes(0)) continue;

  const text = buffer.toString("utf8");
  for (const { label, pattern } of localPathPatterns) {
    if (pattern.test(text)) {
      failures.push(`${file}: contains ${label}`);
      break;
    }
  }

  for (const pattern of secretLiteralPatterns) {
    if (pattern.test(text)) {
      failures.push(`${file}: contains high-confidence secret literal matching ${pattern}`);
      break;
    }
  }
}

if (failures.length > 0) {
  console.error("Repository hygiene check failed:");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log(`Repository hygiene check passed (${trackedFiles.length} tracked files scanned).`);
