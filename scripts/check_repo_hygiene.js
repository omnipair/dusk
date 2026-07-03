import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
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

function readTextFile(file) {
  return readFileSync(file, "utf8");
}

function requireText(file, text, label) {
  if (!readTextFile(file).includes(text)) {
    failures.push(`${file}: missing ${label}`);
  }
}

function requirePattern(file, pattern, label) {
  if (!pattern.test(readTextFile(file))) {
    failures.push(`${file}: missing ${label}`);
  }
}

function checkDuskReadinessGuard() {
  requireText(
    "README.md",
    "Experimental software.",
    "experimental-software disclaimer",
  );
  requireText(
    "README.md",
    "Do not deploy it to mainnet, integrate it in production, or use it with real funds",
    "mainnet/production/funds warning",
  );
  requireText(
    "README.md",
    "Dusk is not production-ready until the V2 release checklist and owner signoff",
    "production-readiness gate warning",
  );

  const signoffFile = "programs/omnipair-v2/SIGNOFF_CHECKLIST.md";
  const allowedStatuses = new Set(["Pending", "Approved", "Blocked", "N/A"]);
  const signoff = readTextFile(signoffFile);
  const rows = signoff
    .split("\n")
    .filter((line) => line.startsWith("| ") && !line.includes(" --- "));

  let signoffRows = 0;
  for (const row of rows) {
    const cells = row
      .split("|")
      .slice(1, -1)
      .map((cell) => cell.trim());
    if (cells[0] === "Area") continue;
    if (cells.length !== 4) {
      failures.push(`${signoffFile}: malformed signoff row: ${row}`);
      continue;
    }

    signoffRows += 1;
    const [area, owner, status, evidence] = cells;
    if (!allowedStatuses.has(status)) {
      failures.push(`${signoffFile}: ${area} has invalid status '${status}'`);
    }
    if (status === "Approved") {
      if (owner === "TBD" || owner.length === 0) {
        failures.push(`${signoffFile}: ${area} is Approved without a named owner`);
      }
      if (evidence.length === 0 || /\bTBD\b/i.test(evidence)) {
        failures.push(`${signoffFile}: ${area} is Approved without concrete evidence`);
      }
    }
    if (status === "Blocked" && !/\bblocked\b|\bblocker\b/i.test(evidence)) {
      failures.push(`${signoffFile}: ${area} is Blocked without blocker evidence`);
    }
  }

  if (signoffRows < 10) {
    failures.push(`${signoffFile}: expected owner signoff rows, found ${signoffRows}`);
  }

  requireText(
    "programs/omnipair-v2/RELEASE_CHECKLIST.md",
    "`SIGNOFF_CHECKLIST.md` has no `Pending` or unresolved `Blocked` rows.",
    "hard signoff completion gate",
  );
  requireText(
    ".github/workflows/release-build.yaml",
    'vars.DUSK_RELEASES_ENABLED',
    "release workflow DUSK_RELEASES_ENABLED guard",
  );
  requireText(
    ".github/workflows/release-build.yaml",
    'Set repository variable DUSK_RELEASES_ENABLED=true only after release checklist and owner signoff are complete.',
    "release workflow owner-signoff warning",
  );
  requirePattern(
    ".github/workflows/release-build.yaml",
    /if \[ "\$RELEASES_ENABLED" != "true" \]; then/,
    "release-disabled branch",
  );
  requireText(
    ".github/workflows/anchor-buffer.yaml",
    'vars.DUSK_MAINNET_BUFFER_DEPLOYS_ENABLED',
    "mainnet buffer DUSK_MAINNET_BUFFER_DEPLOYS_ENABLED guard",
  );
  requireText(
    ".github/workflows/anchor-buffer.yaml",
    "Mainnet buffer deployment must use source=release.",
    "mainnet buffer release-source guard",
  );
  requireText(
    ".github/workflows/anchor-buffer.yaml",
    "Mainnet buffer deployment must transfer buffer authority to Squads.",
    "mainnet buffer Squads-transfer guard",
  );
}

for (const file of trackedFiles) {
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

checkDuskReadinessGuard();

if (failures.length > 0) {
  console.error("Repository hygiene check failed:");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log(`Repository hygiene check passed (${trackedFiles.length} tracked files scanned).`);
