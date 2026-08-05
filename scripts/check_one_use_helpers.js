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

const instructionTestPlacementErrors = [];
for (const [file, originalSource] of originalSources) {
  const instructionSource =
    file.startsWith("programs/dusk/src/instructions/") ||
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
    const includeDirectory = file.startsWith("programs/dusk/src/instructions/")
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

for (const error of instructionTestPlacementErrors) console.error(error);

for (const candidate of actionable) {
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
} else if (instructionTestPlacementErrors.length > 0 || actionable.length > 0) {
  console.error(
    `Code-shape check failed: ${instructionTestPlacementErrors.length} misplaced instruction test module(s); ` +
      `${actionable.length} private/internal production function(s) have at most one call.`
  );
  process.exit(1);
} else {
  console.log(`One-use helper check passed (${declarations.length} internal production functions scanned).`);
}
