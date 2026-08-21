import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";

const PROGRAM_ROOTS = [
  "programs/dusk/src",
  "programs/leverage_delegate/src",
  "programs/faucet/src",
];

function rustFiles(directory) {
  const files = [];
  for (const entry of readdirSync(directory)) {
    const fullPath = path.join(directory, entry);
    if (statSync(fullPath).isDirectory()) {
      if (entry !== "tests" && entry !== "proptest-regressions") {
        files.push(...rustFiles(fullPath));
      }
    } else if (entry.endsWith(".rs")) {
      files.push(fullPath);
    }
  }
  return files;
}

function blankNonCode(source) {
  const output = [...source];
  let state = "code";
  let blockDepth = 0;
  let rawHashes = 0;

  for (let index = 0; index < source.length; index += 1) {
    const current = source[index];
    const next = source[index + 1];

    if (state === "line-comment") {
      if (current === "\n") state = "code";
      else output[index] = " ";
      continue;
    }
    if (state === "block-comment") {
      output[index] = current === "\n" ? "\n" : " ";
      if (current === "/" && next === "*") {
        output[index + 1] = " ";
        blockDepth += 1;
        index += 1;
      } else if (current === "*" && next === "/") {
        output[index + 1] = " ";
        blockDepth -= 1;
        index += 1;
        if (blockDepth === 0) state = "code";
      }
      continue;
    }
    if (state === "string" || state === "char") {
      output[index] = current === "\n" ? "\n" : " ";
      if (current === "\\") {
        if (index + 1 < source.length) output[index + 1] = " ";
        index += 1;
      } else if ((state === "string" && current === '"') || (state === "char" && current === "'")) {
        state = "code";
      }
      continue;
    }
    if (state === "raw-string") {
      output[index] = current === "\n" ? "\n" : " ";
      if (current === '"' && source.slice(index + 1, index + 1 + rawHashes) === "#".repeat(rawHashes)) {
        for (let offset = 1; offset <= rawHashes; offset += 1) output[index + offset] = " ";
        index += rawHashes;
        state = "code";
      }
      continue;
    }

    if (current === "/" && next === "/") {
      output[index] = output[index + 1] = " ";
      state = "line-comment";
      index += 1;
    } else if (current === "/" && next === "*") {
      output[index] = output[index + 1] = " ";
      state = "block-comment";
      blockDepth = 1;
      index += 1;
    } else if (current === '"') {
      output[index] = " ";
      state = "string";
    } else if (current === "r") {
      const rawMatch = source.slice(index).match(/^r(#+)?"/);
      if (rawMatch) {
        rawHashes = rawMatch[1]?.length ?? 0;
        for (let offset = 0; offset < rawMatch[0].length; offset += 1) output[index + offset] = " ";
        index += rawMatch[0].length - 1;
        state = "raw-string";
      }
    } else if (current === "'" && /['\\]/.test(source[index + 2] ?? "")) {
      output[index] = " ";
      state = "char";
    }
  }
  return output.join("");
}

function blankCfgTestFunctionsAndModules(source) {
  const output = [...source];
  const attribute = /#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]/g;
  for (const match of source.matchAll(attribute)) {
    let cursor = match.index + match[0].length;
    while (/\s/.test(source[cursor] ?? "")) cursor += 1;
    while (source.startsWith("#[", cursor)) {
      const end = source.indexOf("]", cursor + 2);
      if (end < 0) break;
      cursor = end + 1;
      while (/\s/.test(source[cursor] ?? "")) cursor += 1;
    }
    const header = source.slice(cursor, cursor + 160);
    if (!/^(?:(?:pub(?:\([^)]*\))?|const|async|unsafe)\s+)*(?:fn|mod|impl)\b/.test(header)) continue;

    const open = source.indexOf("{", cursor);
    const semicolon = source.indexOf(";", cursor);
    let end;
    if (semicolon >= 0 && (open < 0 || semicolon < open)) {
      end = semicolon + 1;
    } else if (open >= 0) {
      let depth = 1;
      end = open + 1;
      while (end < source.length && depth > 0) {
        if (source[end] === "{") depth += 1;
        else if (source[end] === "}") depth -= 1;
        end += 1;
      }
    } else {
      continue;
    }
    for (let index = match.index; index < end; index += 1) {
      output[index] = source[index] === "\n" ? "\n" : " ";
    }
  }
  return output.join("");
}

const files = PROGRAM_ROOTS.flatMap(rustFiles).sort();
const originalSources = new Map(
  files.map((file) => [file, readFileSync(file, "utf8")])
);
const sources = new Map(
  [...originalSources].map(([file, source]) => [
    file,
    blankCfgTestFunctionsAndModules(blankNonCode(source)),
  ])
);

const architecturePlacementErrors = [];
for (const [file, source] of originalSources) {
  if (!file.startsWith("programs/dusk/src/")) continue;
  if (file.startsWith("programs/dusk/src/state/")) {
    const relativeStatePath = file.slice("programs/dusk/src/state/".length);
    if (relativeStatePath.includes("/")) {
      architecturePlacementErrors.push(
        `${file}: state is account-shaped; production state files must live directly under state/`
      );
    } else if (relativeStatePath !== "mod.rs") {
      const accountCount = [...blankNonCode(source).matchAll(/^\s*#\[account(?:\([^\]]*\))?\]/gm)].length;
      if (accountCount !== 1) {
        architecturePlacementErrors.push(
          `${file}: expected exactly one #[account] declaration, found ${accountCount}`
        );
      }
    }
  }
  if (file.includes("/state/market/transitions/")) {
    architecturePlacementErrors.push(
      `${file}: checked mutations belong to their owning domain, not a generic transitions module`
    );
  }
  if (/\bstate::market::transitions\b/.test(source)) {
    architecturePlacementErrors.push(
      `${file}: import the owning domain facade instead of transition internals`
    );
  }
  if (/\btrait\s+Transition\b/.test(blankNonCode(source))) {
    architecturePlacementErrors.push(
      `${file}: persistent lifecycles may use explicit statuses, but atomic operations must not use a generic Transition trait`
    );
  }
}

const instructionTestPlacementErrors = [];
for (const [file, originalSource] of originalSources) {
  const instructionSource =
    file.startsWith("programs/dusk/src/instructions/") ||
    file.startsWith("programs/leverage_delegate/src/instructions/") ||
    file === "programs/leverage_delegate/src/lib.rs" ||
    file === "programs/faucet/src/lib.rs";
  if (!instructionSource) continue;
  const source = blankNonCode(originalSource);
  if (/#\s*\[\s*test\s*\]/.test(source) || /\bproptest\s*!/.test(source)) {
    instructionTestPlacementErrors.push(
      `${file}: test bodies belong under that program's src/tests directory`
    );
  }
  const cfgTest = /#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]/g;
  for (const match of source.matchAll(cfgTest)) {
    let cursor = match.index + match[0].length;
    while (/\s/.test(source[cursor] ?? "")) cursor += 1;
    if (!source.slice(cursor).startsWith("mod tests")) {
      instructionTestPlacementErrors.push(
        `${file}: #[cfg(test)] may only introduce an external test include bridge`
      );
      continue;
    }
    const open = source.indexOf("{", cursor);
    let close = open + 1;
    let depth = 1;
    while (open >= 0 && close < source.length && depth > 0) {
      if (source[close] === "{") depth += 1;
      else if (source[close] === "}") depth -= 1;
      close += 1;
    }
    const moduleSource = open >= 0 ? originalSource.slice(open, close) : "";
    const includeDirectory = file.includes("/src/instructions/")
      ? "tests/instructions/"
      : "tests/";
    const bridgePattern = new RegExp(
      `^\\{\\s*include!\\s*\\(\\s*"[^"]*${includeDirectory.replaceAll("/", "\\/")}[^"]+\\.rs"\\s*\\)\\s*;\\s*\\}$`
    );
    if (
      depth !== 0 ||
      !bridgePattern.test(moduleSource)
    ) {
      instructionTestPlacementErrors.push(
        `${file}: instruction test bridge must contain only one include from src/${includeDirectory}`
      );
    }
  }
}

function implContextAt(source, offset) {
  const blocks = [];
  let headerStart = 0;
  for (let index = 0; index < offset; index += 1) {
    if (source[index] === "{") {
      const header = source.slice(headerStart, index);
      const implMatch = [...header.matchAll(/\bimpl\b/g)].at(-1);
      if (implMatch) {
        const signature = header.slice(implMatch.index);
        const beforeWhere = signature.split(/\bwhere\b/, 1)[0];
        blocks.push(/\bfor\b/.test(beforeWhere) ? "trait-impl" : "impl");
      } else if (/\btrait\b/.test(header)) {
        blocks.push("trait-definition");
      } else {
        blocks.push("other");
      }
      headerStart = index + 1;
    } else if (source[index] === "}") {
      blocks.pop();
      headerStart = index + 1;
    } else if (source[index] === ";") {
      headerStart = index + 1;
    }
  }
  if (blocks.includes("trait-definition")) return "trait-definition";
  if (blocks.includes("trait-impl")) return "trait-impl";
  return blocks.includes("impl") ? "impl" : "free";
}

function bodyRange(source, declarationEnd) {
  const open = source.indexOf("{", declarationEnd);
  const semicolon = source.indexOf(";", declarationEnd);
  if (open < 0 || (semicolon >= 0 && semicolon < open)) return undefined;
  let close = open + 1;
  let depth = 1;
  while (close < source.length && depth > 0) {
    if (source[close] === "{") depth += 1;
    else if (source[close] === "}") depth -= 1;
    close += 1;
  }
  return depth === 0 ? { open, close } : undefined;
}

const declarations = [];
const allDeclarations = [];
const declarationPattern = /^([ \t]*)(pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe)\s+)*(?:extern\s+"[^"]+"\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\b/gm;

for (const [file, source] of sources) {
  for (const match of source.matchAll(declarationPattern)) {
    const visibility = (match[2] ?? "").trim();
    const line = source.slice(0, match.index).split("\n").length;
    const name = match[3];
    const declaration = {
      file,
      line,
      name,
      nameOffset: match.index + match[0].lastIndexOf(name),
      visibility: visibility || "private",
      crateRoot: PROGRAM_ROOTS.find((root) => file.startsWith(`${root}/`)),
      implContext: implContextAt(source, match.index),
      body: bodyRange(source, match.index + match[0].length),
    };
    allDeclarations.push(declaration);
    if (
      visibility !== "pub" &&
      declaration.implContext !== "trait-impl" &&
      declaration.implContext !== "trait-definition"
    ) {
      declarations.push(declaration);
    }
  }
}

const callSources = new Map([...sources].map(([file, source]) => [file, [...source]]));
for (const declaration of allDeclarations) {
  const source = callSources.get(declaration.file);
  for (let offset = 0; offset < declaration.name.length; offset += 1) {
    source[declaration.nameOffset + offset] = " ";
  }
}
for (const [file, source] of callSources) callSources.set(file, source.join(""));

function callPositions(source, name, implContext) {
  const callPattern = new RegExp(`\\b${name}\\s*(?:::<[^(){};]*>)?\\s*\\(`, "g");
  return [...source.matchAll(callPattern)]
    .filter((match) => {
      let previous = match.index - 1;
      while (previous >= 0 && /\s/.test(source[previous])) previous -= 1;
      const methodSyntax = source[previous] === ".";
      return implContext === "free" ? !methodSyntax : methodSyntax || source[previous] === ":";
    })
    .map((match) => match.index);
}

for (const declaration of allDeclarations) {
  const fileCallPositions = callPositions(
    callSources.get(declaration.file),
    declaration.name,
    declaration.implContext
  );
  declaration.fileCalls = fileCallPositions.length;
  declaration.crateCalls = [...callSources]
    .filter(([file]) => file.startsWith(`${declaration.crateRoot}/`))
    .reduce(
      (total, [, source]) =>
        total + callPositions(source, declaration.name, declaration.implContext).length,
      0
    );
  declaration.recursive =
    declaration.body !== undefined &&
    fileCallPositions.some(
      (position) =>
        position > declaration.body.open && position < declaration.body.close
    );
}

const maximumCalls = process.argv.includes("--broad") ? 2 : 1;
const candidates = declarations.filter(({ visibility, fileCalls, crateCalls }) =>
  visibility === "private" ? fileCalls <= maximumCalls : crateCalls <= maximumCalls
);

const actionable = candidates.filter(({ recursive }) => !recursive);
// These helpers are intentional domain boundaries in the current predeployment
// architecture. Keep the exact names here so this check remains a regression
// gate: new one-use helpers still fail, while removing an accepted helper never
// blocks a cleanup commit. Placement errors are never baselined.
const acceptedOneUseHelpers = new Map([
  ["programs/dusk/src/instructions/leverage/liquidate_leverage.rs", new Set(["finish_liquidation"])],
  [
    "programs/dusk/src/instructions/leverage/open_leverage.rs",
    new Set(["leverage_entry_price_nad", "require_leverage_entry_limit", "leverage_entry_limit_satisfied"]),
  ],
  [
    "programs/dusk/src/instructions/prepare_swap.rs",
    new Set(["finalize_explicit_state", "rebalance_executes_token_changes", "split_claimable_fee_credit"]),
  ],
  ["programs/dusk/src/instructions/spot/swap.rs", new Set(["require_critical_hlp_liquidation"])],
  [
    "programs/dusk/src/market/amm.rs",
    new Set([
      "checkpoint_amm_socialized_loss_raw",
      "advance_one_explicit_controller_target",
      "apply_explicit_curve_parameter_update",
      "unrealized_interest",
      "quote_integrated_explicit_exact_in_nad",
      "quote_explicit_integrated_with_fee",
      "output_denominated_dynamic_fee",
      "split_compounded_swap_fee",
      "is_explicit",
      "preliminary_swap_inputs_for_state",
    ]),
  ],
  ["programs/dusk/src/market/lending.rs", new Set(["global_side_health_with_virtual_reserves"])],
  [
    "programs/dusk/src/market/leverage.rs",
    new Set([
      "set_debt",
      "commit_leverage_lifecycle_state",
      "derive_leverage_lifecycle_plan_from_state",
      "apply_leverage_lifecycle_plan",
      "cap_tracking_unrealized_interest",
      "rebase_explicit_curve_after_terminal_hlp_loss",
      "apply_leverage_socialized_loss",
      "post_swap_closeout_quote_with_quote",
      "debit_leverage_cash",
      "add_isolated_borrow_debt",
    ]),
  ],
  [
    "programs/dusk/src/market/liquidity.rs",
    new Set([
      "floors",
      "prepare_explicit_hlp_transition",
      "apply_explicit_hlp_recovery",
      "insurance_request",
      "target_asset",
      "prepare_terminal_hlp_waterfall",
      "prepare_explicit_hlp_transition_at_current_state",
      "debt_deltas",
      "interest_cash_floors",
      "checkpoint_pre_solve_fee_eligibility",
      "combine_hlp_rebalance_receipts",
      "empty_hlp_rebalance_receipt",
      "hlp_valuation_from_values",
      "signed_value_difference",
      "checkpoint_hlp_yield_from_ylp_pair",
      "hlp_debt_amount",
      "ylp_live_underlying_amount_from_values",
      "current_hlp_inventory_values_pair_nad_with_prices",
      "hlp_frozen_interest_claim_delta_value_nad",
      "hlp_tracking_deltas_nad",
      "stamp_hlp_tracking_reference",
      "require_hlp_borrow_headroom",
      "curve_slot",
    ]),
  ],
  [
    "programs/dusk/src/math/dynamic_fee.rs",
    new Set([
      "minimum_executable_input",
      "fee_share_cap_to_marginal_rate_nad",
      "state_potential",
      "marginal_rate_nad",
      "gross_path_divergence_fee_raw",
    ]),
  ],
  [
    "programs/dusk/src/math/explicit_curve.rs",
    new Set([
      "parameters",
      "center_point",
      "center_point_with_geometry",
      "price_bounds_nad",
      "prepare_centered_explicit_geometry",
      "prepare_centered_explicit_cache",
      "explicit_total_liquidity_root",
      "sqrt_floor_u512",
      "spot_price_nad",
      "point_at_price_nad",
    ]),
  ],
  [
    "programs/dusk/src/math/hlp_integrated.rs",
    new Set([
      "from_total_reserves",
      "reconstruct_hlp_ownership",
      "apply_hlp_recovery_bonus",
      "apply_compounded_ylp_fee",
      "quote_integrated_exact_in",
      "quote_integrated_exact_out",
      "quote_integrated_exact_in_with_frozen_fee",
    ]),
  ],
  ["programs/dusk/src/math/hlp_recovery.rs", new Set(["quote_hlp_recovery"])],
  ["programs/dusk/src/math/risk.rs", new Set(["ema_u128_including_zero"])],
  [
    "programs/dusk/src/state/market.rs",
    new Set([
      "commit_explicit_recenter",
      "effective_market_cap_fee_bps_at",
      "draw_window",
      "require_initial_liquidity_authority",
      "assert_liquidity_seeding_available_with_futarchy",
      "isolated_clearance_for_max",
    ]),
  ],
  [
    "programs/leverage_delegate/src/instructions/entry/common.rs",
    new Set(["verify_opened_position", "escrow_margin_after_bounty"]),
  ],
  [
    "programs/leverage_delegate/src/instructions/hlp/common.rs",
    new Set(["withdraw_hlp_order_position", "validate_hlp_order_kind", "hlp_order_trigger_met"]),
  ],
]);
const unexpectedActionable = actionable.filter(
  ({ file, name }) => !acceptedOneUseHelpers.get(file)?.has(name)
);
const publicAudit = process.argv.includes("--public")
  ? allDeclarations.filter(
      ({ visibility, implContext, crateCalls, recursive }) =>
        visibility === "pub" &&
        implContext !== "trait-impl" &&
        implContext !== "trait-definition" &&
        crateCalls <= 1 &&
        !recursive
    )
  : [];

for (const error of architecturePlacementErrors) console.error(error);
for (const error of instructionTestPlacementErrors) console.error(error);

for (const candidate of process.argv.includes("--broad") ? actionable : unexpectedActionable) {
  console.log(
    `${candidate.file}:${candidate.line}: ${candidate.visibility} fn ${candidate.name} ` +
      `(file calls=${candidate.fileCalls}, crate calls=${candidate.crateCalls})`
  );
}

for (const candidate of publicAudit) {
  console.log(
    `${candidate.file}:${candidate.line}: public fn ${candidate.name} ` +
      `(file calls=${candidate.fileCalls}, crate calls=${candidate.crateCalls})`
  );
}

if (process.argv.includes("--public")) {
  console.log(`Public low-use audit candidates: ${publicAudit.length}.`);
} else if (process.argv.includes("--broad")) {
  console.log(`Broad one-use audit candidates: ${actionable.length}; internal declarations scanned: ${declarations.length}.`);
} else if (
  architecturePlacementErrors.length > 0 ||
  instructionTestPlacementErrors.length > 0 ||
  unexpectedActionable.length > 0
) {
  console.error(
    `Code-shape check failed: ${architecturePlacementErrors.length} architecture placement error(s); ` +
      `${instructionTestPlacementErrors.length} misplaced instruction test module(s); ` +
      `${unexpectedActionable.length} unreviewed private/internal production function(s) have at most one call.`
  );
  process.exit(1);
} else {
  console.log(
    `One-use helper check passed (${declarations.length} internal production functions scanned; ` +
      `${actionable.length} reviewed domain helper(s)).`
  );
}
