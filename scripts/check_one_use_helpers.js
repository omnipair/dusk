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
  if (
    /#\s*\[\s*cfg\s*\([^\]]*\bbenchmark\b[^\]]*\)\s*\]\s*(?:(?:pub(?:\([^)]*\))?|const|async|unsafe)\s+)*(?:fn|impl)\b/.test(
      source
    )
  ) {
    architecturePlacementErrors.push(
      `${file}: benchmark-only implementations belong under programs/dusk/benchmark/; keep only a conditional include bridge in src/`
    );
  }
  if (file.startsWith("programs/dusk/src/state/")) {
    for (const line of source.split("\n").map((value) => value.trim()).filter((value) => value.includes("crate::transitions::"))) {
      architecturePlacementErrors.push(
        `${file}: persistent-state dependency on transition-owned types is forbidden; move the type to state/`
      );
    }
  }
}

const testPlacementErrors = [];
for (const [file, originalSource] of originalSources) {
  const source = blankNonCode(originalSource);
  if (/#\s*\[\s*test\s*\]/.test(source) || /\bproptest\s*!/.test(source)) {
    testPlacementErrors.push(
      `${file}: test bodies belong under that program's src/tests directory`
    );
  }
  const cfgTest = /#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]/g;
  for (const match of originalSource.matchAll(cfgTest)) {
    let cursor = match.index + match[0].length;
    while (/\s/.test(originalSource[cursor] ?? "")) cursor += 1;
    const attributeStart = cursor;
    while (originalSource.startsWith("#[", cursor)) {
      const end = originalSource.indexOf("]", cursor + 2);
      if (end < 0) break;
      cursor = end + 1;
      while (/\s/.test(originalSource[cursor] ?? "")) cursor += 1;
    }
    const attributes = originalSource.slice(attributeStart, cursor);
    const tail = originalSource.slice(cursor);
    const directInclude = tail.match(/^include!\s*\(\s*"([^"]+)"\s*\)\s*;/);
    if (directInclude) {
      if (!/(?:^|\/)tests\/support\//.test(directInclude[1])) {
        testPlacementErrors.push(
          `${file}: direct #[cfg(test)] includes must come from src/tests/support`
        );
      }
      continue;
    }

    const module = tail.match(/^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*([;{])/);
    if (!module) {
      testPlacementErrors.push(
        `${file}: #[cfg(test)] may only introduce an external test module or support include`
      );
      continue;
    }
    if (module[2] === ";") {
      if (!/#\[path\s*=\s*"[^"]*tests\//.test(attributes)) {
        testPlacementErrors.push(
          `${file}: test-only modules must use #[path] into src/tests`
        );
      }
      continue;
    }

    const open = originalSource.indexOf("{", cursor);
    let close = open + 1;
    let depth = 1;
    while (open >= 0 && close < originalSource.length && depth > 0) {
      if (originalSource[close] === "{") depth += 1;
      else if (originalSource[close] === "}") depth -= 1;
      close += 1;
    }
    const moduleSource = open >= 0 ? originalSource.slice(open, close) : "";
    const bridgePattern = /^\{\s*include!\s*\(\s*"[^"]*tests\/[^"]+\.rs"\s*\)\s*;\s*\}$/;
    if (depth !== 0 || !bridgePattern.test(moduleSource)) {
      testPlacementErrors.push(
        `${file}: test module ${module[1]} must contain only one include from src/tests`
      );
    }
  }
}

const previewPlacementErrors = [];
for (const [file, originalSource] of originalSources) {
  const instructionRoot = [
    "programs/dusk/src/instructions/",
    "programs/leverage_delegate/src/instructions/",
  ].find((root) => file.startsWith(root));
  if (!instructionRoot || file === `${instructionRoot}preview.rs`) continue;
  const source = blankNonCode(originalSource);
  if (
    /\b(?:struct|enum|type)\s+Preview[A-Za-z0-9_]*\b/.test(source) ||
    /\bimpl(?:<[^>]*>)?\s+Preview[A-Za-z0-9_]*\b/.test(source) ||
    /\bfn\s+preview_[A-Za-z0-9_]*\b/.test(source)
  ) {
    previewPlacementErrors.push(
      `${file}: preview instruction types and helpers belong in ${instructionRoot}preview.rs`
    );
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
  declaration.globalCalls = [...callSources].reduce(
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
const internalDead = declarations.filter(({ crateCalls, recursive }) => crateCalls === 0 && !recursive);
// These helpers are intentional domain boundaries in the current predeployment
// architecture. Keep the exact names here so this check remains a regression
// gate: new one-use helpers and stale exceptions both fail. Placement errors
// are never baselined.
const acceptedOneUseHelpers = new Map([
  // Keep the shared index capacity search and exact fractional-growth kernel
  // separate from market writes. Boundary tests exercise index overflow,
  // exhausted LP units, fee conservation, repayment, and liquidation recovery.
  ["programs/dusk/src/transitions/lending/accrual.rs", new Set(["bounded_accrual_index", "unbounded_accrual_index"])],
  // Separate SBF frames keep the large swap/market scratch state out of the
  // caller's 4 KiB stack. These boundaries are exercised by native liquidation
  // and compound-fee conservation tests and the executable SBF paths.
  ["programs/dusk/src/transitions/amm/compounding.rs", new Set(["prepare_post_settlement_compounding"])],
  // All derived hedge fields must move with a fee-adjusted endpoint; keep the
  // refresh beside the endpoint materialization and signed rounding rules.
  // Explicit NAV-to-coupon conversion boundary, tested at rounding limits and
  // with negative funded but positive total hLP equity.
  ["programs/dusk/src/transitions/tranches/market_income.rs", new Set(["quote_hlp_capitalization_for_value"])],
  // Bounded fixed-point exp2/log2 kernels are kept separate so their precision,
  // iteration limits and overflow behavior can be reviewed independently of
  // the controller's economic branches. apply is the debt-index carry boundary.
  ["programs/dusk/src/math/borrow_controller.rs", new Set(["fractional_factor", "half_life_crossing_ms"])],
  ["programs/dusk/src/transitions/tranches/risk_rate_book.rs", new Set(["apply"])],
  // Pure event construction is tested without Anchor account fixtures; the
  // instruction helper emits the result through the shared self-CPI path.
  // New hLP income allocation is a pure transition boundary: economic tests
  // exercise conservation without custody CPIs; the instruction moves exactly
  // its returned debits and credits atomically.
  ["programs/dusk/src/transitions/liquidity/hlp/harvest.rs", new Set(["settle_new_hlp_public_interest"])],
  // Flash session boundaries are pure accounting kernels, separately exercised
  // without token CPIs; instructions bind them to custody and atomic settlement.
  ["programs/dusk/src/transitions/liquidity/hlp/flash.rs", new Set(["begin_hlp_flash_recovery", "settle_hlp_flash_recovery"])],
  ["programs/dusk/src/transitions/liquidity/hlp/flash_terminal.rs", new Set(["settle_terminal_hlp_flash"])],
  // The advance ledger's funding and one-shot payment consumption boundaries
  // own the frozen fee and transient-credit invariants. Keep these independently
  // testable from reserve mutation and Token-2022 CPIs.
  ["programs/dusk/src/transitions/interest_advance.rs", new Set(["take_payment"])],
  // Emergency inventory preparation must remain separate from executable
  // swaps: a virtual pricing endpoint must never become an hLP/reserve plan.
  ["programs/dusk/src/transitions/amm/liquidation.rs", new Set(["prepare_ema_liquidation"])],
  // Keep the pure, clock-bound hLP admission and fee-boundary calculations
  // independently testable from Anchor account deserialization and sysvars.
  ["programs/dusk/src/instructions/preview.rs", new Set(["preview_hlp_funding_limit", "gross_funding_limit"])],
  [
    "programs/dusk/src/transitions/amm/curve.rs",
    // Oracle geometry construction has independent price/depth inputs, unlike
    // live-reserve reconstruction, and is exercised across concentration tiers.
    new Set(["concentrated_total_liquidity_root", "prepare_concentrated_cache_and_geometry_at_depth",
      // One constructor and one hot-loop lookup share exact boundary dust;
      // inlining them would mix geometry construction with the SBF proof frame.
      "prepare_quote_path", "amount_out",
      // A nested branch's wide quadratic needs its own SBF frame: inlined into
      // the reconstruction loop it overflows the 4 KiB frame by 320 bytes.
      // The confirmed root estimate and the exact Newton square root behind it
      // are independent algorithms: the estimate may decline at any step with
      // `None`, and the fallback alone owns the closed form's error codes.
      // Built by value in the reconstruction's frame, the solver overflows it.
      "nested_total_liquidity_root", "confirmed_quadratic_root", "sqrt_floor_u512", "build_in"]),
  ],
  [
    "programs/dusk/src/transitions/amm/fees.rs",
    new Set(["gross_path_divergence_fee_raw"]),
  ],
  [
    "programs/dusk/src/transitions/amm/mod.rs",
    new Set([
      // Complete swaps now have one caller for these AMM-owned ledger/observation kernels.
      // These freeze a complete book/curve quote and isolate temporary geometry
      // so neither routine's caller exceeds the SBF stack-frame limit.
      // The rare distress retry stays in its own SBF frame; placing the
      // returned integrated quote and error on the swap frame overflows it.
      "prepare_current_hlp_quote_with_distress_freeze",
      "prepare_book_curve_mark",
      "credit_protected_recenter_reserve",
      "finalize_amm_trade_after_inventory_checkpoint",
      "advance_one_concentrated_controller_target",
      "apply_concentrated_curve_parameter_update",
      // Oracle/tranche integration exceeds the SBF 4 KiB frame when the
      // ordinary-only and hLP accounting branches share one function body.
      "integrated_curve_state_without_hlp",
      "integrated_hlp_curve_state_with_mark_nad",
      "output_denominated_dynamic_fee",
      "split_compounded_swap_fee",
      "commit_concentrated_recenter",
    ]),
  ],
  [
    "programs/dusk/src/transitions/market_config.rs",
    new Set([
      "effective_market_cap_fee_bps_at",
      "assert_liquidity_seeding_available_with_futarchy",
      // The runtime handler and native replay must enforce the same first-LP
      // signer policy; keeping the guard here prevents the two paths drifting.
      "require_initial_liquidity_authority",
    ]),
  ],
  [
    "programs/dusk/src/transitions/lending/preview.rs",
    new Set([
      "lending_side_preview",
      "borrow_capacity_quote_with_market_fees",
      "position_debt_side_quote",
      "position_capacity_quote_with_market_fees",
    ]),
  ],
  // Repayment is a complete accounting transition shared by the instruction,
  // native replay, and test support. Its curve, debt, reserve, risk, referral,
  // and health updates stay atomic and independently reviewable.
  [
    "programs/dusk/src/transitions/lending/mod.rs",
    new Set([
      "repay_with_finalization_and_fee",
      // The runtime reference-price path and analysis liquidation path share
      // one pessimistic curve reconstruction and its conservative rounding.
      "pessimistic_concentrated_curve",
    ]),
  ],
  [
    "programs/dusk/src/transitions/leverage.rs",
    new Set([
      // The shared AMM transition is the sole caller of the debt and quote kernels.
      // Keep leverage debt settlement and loss application owned by this
      // domain instead of embedding their accounting in the AMM swap module.
      "apply_leverage_lifecycle_transition",
      "apply_leverage_socialized_loss",
      "validate_leverage_swap_quote",
      "require_leverage_entry_limit",
      "commit_leverage_lifecycle_state",
      "derive_leverage_lifecycle_plan_from_state",
      "apply_leverage_lifecycle_plan",
      "post_swap_closeout_quote_with_quote",
      // Repayment now has one handler after margin withdrawal was removed.
      // Keep its hLP/risk checkpoint boundary independently parity-tested.
      "prepare_leverage_margin_operation",
    ]),
  ],
  [
    "programs/dusk/src/transitions/liquidity/hlp/engine.rs",
    new Set([
      "apply_concentrated_hlp_recovery",
      "target_asset",
      "prepare_terminal_hlp_waterfall",
      // Quote finalization must derive its point from the private, atom-rounded
      // ownership/debt plan rather than duplicate that accounting in the AMM.
      "debt_deltas",
      "hlp_valuation_from_values",
      "signed_value_difference",
    ]),
  ],
  [
    "programs/dusk/src/transitions/liquidity/hlp/integrated.rs",
    new Set(["refresh_hlp_endpoint",
      "reconstruct_hlp_ownership",
      "apply_hlp_recovery_bonus",
      "quote_integrated_exact_in",
    ]),
  ],
  ["programs/dusk/src/transitions/liquidity/hlp/recovery.rs", new Set(["quote_hlp_recovery"])],
  [
    "programs/dusk/src/transitions/revenue.rs",
    new Set(["quote_protocol_auction_settlement", "credit_protocol_interest_revenue"]),
  ],
  ["programs/dusk/src/math/risk.rs", new Set(["ema_u128_including_zero"])],
  [
    "programs/leverage_delegate/src/instructions/entry/common.rs",
    new Set(["verify_opened_position"]),
  ],
  [
    "programs/leverage_delegate/src/instructions/hlp/common.rs",
    new Set(["withdraw_hlp_order_position"]),
  ],
]);
// Dedicated CPI frames keep the protection executor within the SBF stack limit.
acceptedOneUseHelpers.set("programs/leverage_delegate/src/instructions/protection/payment.rs", new Set(["make_payment"]));
acceptedOneUseHelpers.set("programs/leverage_delegate/src/instructions/protection/redeem.rs", new Set(["redeem_lp"]));
// Alternate flash settlement has its own CPI frame; merging it into the
// external settle handler would overlap two large SBF settlement frames.
// The paired-AMM preview and settle paths live together so they share the same
// final-payout accounting boundary.
acceptedOneUseHelpers.set(
  "programs/dusk/src/instructions/flash_amm_settlement.rs",
  new Set(["quote_flash_amm_liquidation", "settle_with_amm"])
);
// Keep the hLP trigger CPI in a separate SBF frame while centralizing all
// preview ownership in the delegate preview module.
acceptedOneUseHelpers.set(
  "programs/leverage_delegate/src/instructions/preview.rs",
  new Set(["fetch_hlp_order_trigger_preview"])
);
// Keep the complete emergency market/position mutation directly testable
// without Anchor custody accounts, using the same transition as execution.
// Candidate construction is a separate SBF frame from the recovery proof.
acceptedOneUseHelpers.set("programs/dusk/src/transitions/amm_liquidation.rs", new Set(["apply_emergency_liquidation", "prepare_emergency_candidate"]));
// Market cloning and curve preparation must not share the interval proof's
// SBF frame. The combined version exceeded the 4 KiB stack limit.
acceptedOneUseHelpers.set("programs/dusk/src/transitions/liquidation_sizing.rs", new Set(["liquidation_scratch"]));
// Separate SBF frame for the all-smaller-share recovery certificate.
acceptedOneUseHelpers.set("programs/dusk/src/transitions/flash_liquidation.rs", new Set(["require_first_flash_recovery"]));
// A liquidation's read cache keeps its keys private: the EMA curve is rebuilt
// unless every input that builds it repeats, and the one market mutation
// between proof and quote states which reads it invalidates.
acceptedOneUseHelpers.get("programs/dusk/src/transitions/flash_liquidation.rs").add("ema_curve");
acceptedOneUseHelpers.get("programs/dusk/src/transitions/flash_liquidation.rs").add("market_changed");
// The retired-cohort decoder needs its own SBF frame. Inlining it into the
// generic transfer-hook account mutation exceeded the 4 KiB stack limit.
acceptedOneUseHelpers.set("programs/dusk/src/instructions/transfer_hook.rs", new Set(["retired_hlp_yield_contexts"]));
// Bounded invariant construction is separated from evaluation: shape bands,
// cached coordinates/primitives, elementary antiderivatives, and the Taylor
// remainder certificate each have an independent mathematical contract.
// Taylor arrays and the output seed also require separate SBF stack frames.
acceptedOneUseHelpers.set("programs/dusk/src/math/op_gamm.rs", new Set([
  "raw_coefficients", "build_bands", "coordinate_uncached", "price_bounds_nad", "native_geometry_at",
  "historical_mark_nad", "native_primitive_uncached", "analytic_residual",
  "cpmm_integral", "taylor_cell", "elasticity", "native_seed", "solve_output", "taylor_quotient_integral",
]));
// Keep the rare four-limb path out of inlined Q56 arithmetic (the combined
// SBF body exceeded LLVM branch range). Log range reduction owns its series
// enclosure and is tested separately from interval endpoint composition.
acceptedOneUseHelpers.set("programs/dusk/src/math/op_gamm/interval.rs", new Set(["unsigned_ratio_wide", "ln_point"]));
// Source equivalence defines replay-history continuity across governance.
// observation is the authority boundary for price-free repayment, also tested
// directly to ensure deserialization cannot restore execution authority.
acceptedOneUseHelpers.set("programs/dusk/src/oracle.rs", new Set(["read", "assert_not_before", "bind"]));
// The native geometry memo costs one heap block, so an instruction opts in
// where it re-reads the same few curves; emergency liquidation does.
acceptedOneUseHelpers.get("programs/dusk/src/oracle.rs").add("memoize_native_geometry");
// The quote finisher owns fees and final ownership certification in a separate
// SBF frame; merging it with the solver exceeded the 4 KiB stack contract.
acceptedOneUseHelpers.get("programs/dusk/src/transitions/amm/mod.rs").add("finish_concentrated_swap");
// A native valuation's 696-byte quote needs its own SBF frame. On the swap
// evaluation frame it overlapped the stack-argument slots of that frame's calls.
acceptedOneUseHelpers.get("programs/dusk/src/transitions/amm/mod.rs").add("native_valuation_amount_out_nad");
acceptedOneUseHelpers.set("programs/dusk/src/transitions/amm/oracle.rs", new Set(["quote_oracle_integrated"]));
// Authenticate the immutable oracle policy before the atomic operator update.
acceptedOneUseHelpers.get("programs/dusk/src/transitions/market_config.rs").add("execute_parameter_updates_authenticated");
// Historical marks measure volatility only. The repayment boundary explicitly
// separates price-free debt reduction from authenticated price-sensitive risk.
acceptedOneUseHelpers.set("programs/dusk/src/transitions/oracle.rs", new Set(["update_for_debt_reduction"]));
// These boundaries isolate independently tested configuration rollback, guarded
// ramp candidates and centering loss calculations from the callers' SBF frames.
acceptedOneUseHelpers.get("programs/dusk/src/transitions/market_config.rs").add("copy_fee_profile_fields");
acceptedOneUseHelpers.get("programs/dusk/src/transitions/market_config.rs").add("execute_parameter_updates_inner");
acceptedOneUseHelpers.set("programs/dusk/src/transitions/curve_ramp.rs",new Set(["advance_concentration_ramp"]));
const unexpectedActionable = actionable.filter(
  ({ file, name }) => !acceptedOneUseHelpers.get(file)?.has(name)
);
const acceptedAudit = actionable.filter(({ file, name }) => acceptedOneUseHelpers.get(file)?.has(name));
const actionableKeys = new Set(actionable.map(({ file, name }) => `${file}:${name}`));
const staleAccepted = [...acceptedOneUseHelpers].flatMap(([file, names]) =>
  [...names]
    .filter((name) => !actionableKeys.has(`${file}:${name}`))
    .map((name) => ({ file, name }))
);
const publicDead = allDeclarations.filter(
  ({ file, name, visibility, implContext, globalCalls, recursive }) =>
    visibility === "pub" &&
    implContext !== "trait-impl" &&
    implContext !== "trait-definition" &&
    globalCalls === 0 &&
    !recursive &&
    !file.endsWith("/src/lib.rs") &&
    file !== "programs/dusk/src/benchmark_api.rs" &&
    file !== "programs/dusk/src/instructions/accounts.rs" &&
    !["bounded_swap_probe", "bounded_invariant_probe"].includes(name)
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
for (const error of testPlacementErrors) console.error(error);
for (const error of previewPlacementErrors) console.error(error);
for (const { file, name } of staleAccepted) {
  console.error(`${file}: stale accepted one-use helper ${name}`);
}
for (const { file, line, name } of publicDead) {
  console.error(`${file}:${line}: public fn ${name} has no production caller`);
}
for (const { file, line, name } of internalDead) {
  console.error(`${file}:${line}: internal fn ${name} has no production caller`);
}

for (const candidate of process.argv.includes("--broad") ? actionable : unexpectedActionable) {
  console.log(
    `${candidate.file}:${candidate.line}: ${candidate.visibility} fn ${candidate.name} ` +
      `(file calls=${candidate.fileCalls}, crate calls=${candidate.crateCalls})`
  );
}

if (process.argv.includes("--accepted")) {
  for (const candidate of acceptedAudit) {
    const source = sources.get(candidate.file);
    const bodyLines = candidate.body === undefined
      ? 0
      : source.slice(candidate.body.open, candidate.body.close + 1).split("\n").length;
    const prefix = source.slice(Math.max(0, candidate.nameOffset - 240), candidate.nameOffset);
    const inlineNever = /#\s*\[\s*inline\s*\(\s*never\s*\)\s*\]/.test(prefix);
    console.log(
      `${candidate.file}:${candidate.line}: ${candidate.name} ` +
        `(body lines=${bodyLines}, inline(never)=${inlineNever}, crate calls=${candidate.crateCalls})`
    );
  }
}

for (const candidate of publicAudit) {
  console.log(
    `${candidate.file}:${candidate.line}: public fn ${candidate.name} ` +
      `(file calls=${candidate.fileCalls}, crate calls=${candidate.crateCalls})`
  );
}

if (process.argv.includes("--accepted")) {
  console.log(`Accepted one-use audit entries: ${acceptedAudit.length}.`);
} else if (process.argv.includes("--public")) {
  console.log(`Public low-use audit candidates: ${publicAudit.length}.`);
} else if (process.argv.includes("--broad")) {
  console.log(`Broad one-use audit candidates: ${actionable.length}; internal declarations scanned: ${declarations.length}.`);
} else if (
  architecturePlacementErrors.length > 0 ||
  testPlacementErrors.length > 0 ||
  previewPlacementErrors.length > 0 ||
  staleAccepted.length > 0 ||
  publicDead.length > 0 ||
  internalDead.length > 0 ||
  unexpectedActionable.length > 0
) {
  console.error(
    `Code-shape check failed: ${architecturePlacementErrors.length} architecture placement error(s); ` +
      `${testPlacementErrors.length} misplaced test module(s); ` +
      `${previewPlacementErrors.length} misplaced preview declaration(s); ` +
      `${staleAccepted.length} stale one-use exception(s); ` +
      `${publicDead.length} public function(s) without production callers; ` +
      `${internalDead.length} internal function(s) without production callers; ` +
      `${unexpectedActionable.length} unreviewed private/internal production function(s) have at most one call.`
  );
  process.exit(1);
} else {
  console.log(
    `One-use helper check passed (${declarations.length} internal production functions scanned; ` +
      `${actionable.length} reviewed domain helper(s)).`
  );
}
