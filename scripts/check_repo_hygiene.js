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

const unreliableDuskEventPatterns = [
  {
    label: "plain Anchor event logging; use emit_cpi! so indexers can recover the event from inner instructions",
    pattern: /\bemit!\s*\(/,
  },
  {
    label: "raw Solana data logging; use a typed emit_cpi! event instead",
    pattern: /\bsol_log_data\b/,
  },
];

const retiredBorrowControllerPatterns = [
  /\brate_at_target_nad\b/,
  /\brateAtTargetNad\b/,
  /\bbase_rate_at_target_nad\b/,
  /\bquote_rate_at_target_nad\b/,
  /\bbaseRateAtTargetNad\b/,
  /\bquoteRateAtTargetNad\b/,
  /\bDEFAULT_IRM_TARGET_UTILIZATION_BPS\b/,
  /\bDEFAULT_IRM_CURVE_STEEPNESS_NAD\b/,
  /\bDEFAULT_IRM_ADJUSTMENT_SPEED_PER_YEAR\b/,
  /\bINTEREST_(?:MIN|MAX|INITIAL)_RATE_AT_TARGET_NAD\b/,
  /\butilization_error_nad\b/,
  /\binstantaneous_rate_apr_nad\b/,
  /\badapt_rate_at_target_nad\b/,
  /\baccrued_index_nad\b/,
];

function isCurrentBorrowControllerSurface(normalized) {
  return (
    normalized.startsWith("programs/dusk/src/") ||
    normalized.startsWith("packages/dusk-sdk/src/") ||
    normalized === "programs/dusk/README.md" ||
    normalized === "docs/BORROW_RATE_CONTROLLER.md"
  );
}

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

  if (normalized.startsWith("programs/dusk/src/") && normalized.endsWith(".rs")) {
    for (const { label, pattern } of unreliableDuskEventPatterns) {
      if (pattern.test(text)) {
        failures.push(`${file}: contains ${label}`);
        break;
      }
    }
  }

  if (isCurrentBorrowControllerSurface(normalized)) {
    for (const pattern of retiredBorrowControllerPatterns) {
      if (pattern.test(text)) {
        failures.push(`${file}: contains retired utilization-target controller identifier ${pattern}`);
        break;
      }
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
