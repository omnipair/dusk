/**
 * Instruction inputs for the generic Dusk write path: compile-time types
 * derived per instruction from the `Dusk` IDL type, and the run-time check
 * that walks the IDL before Anchor encodes anything.
 *
 * Anchor's Borsh encoder skips a struct field whose value is `undefined` and
 * leaves zero bytes behind, so a misspelled or missing u64 used to reach the
 * program as 0: a mistyped slippage floor became no floor. An `undefined`
 * Option, string or Vec field breaks the layout instead: the encoder steps
 * back a byte rather than past the field, so the call fails with an unrelated
 * offset or span error, or sends truncated data the program rejects. Anchor
 * also needs BN for 64-bit and wider integers and crashed on bigint or number
 * amounts with `src.toArrayLike is not a function`. Every generic call now
 * goes through `normalizeInstructionArgs` and `normalizeInstructionAccounts`
 * first.
 */
import * as anchorNamespace from "@coral-xyz/anchor";
import type { BN } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";

import type { AddressLike } from "./address.js";
import type { Dusk } from "./types_v2.js";

// Same interop as parameters.ts: the ESM build exposes `BN` directly, Node's
// CommonJS interop only through `default`.
type AnchorBNConstructor = new (value: string) => BN;
const anchorExports = anchorNamespace as unknown as {
  BN?: AnchorBNConstructor;
  default?: { BN?: AnchorBNConstructor };
};
const AnchorBN = (anchorExports.BN ?? anchorExports.default?.BN) as AnchorBNConstructor;

// ---------------------------------------------------------------------------
// Compile-time types
// ---------------------------------------------------------------------------

/**
 * A 64-bit or wider integer field. Converted to BN before encoding; a number
 * must be a safe integer and a string must be a decimal integer.
 */
export type IntegerInput = BN | bigint | number | string;

/** A `bytes`, `Vec<u8>` or `[u8; N]` field. */
export type ByteInput = Uint8Array | readonly number[];

type DuskIdlInstruction = Dusk["instructions"][number];

/** Every instruction name the Dusk program declares, in camelCase. */
export type DuskInstructionName = DuskIdlInstruction["name"];

/** Instructions that take no arguments. */
export type DuskInstructionWithoutArgs = Extract<DuskIdlInstruction, { args: readonly [] }>["name"];

type DuskInstructionMap = { [I in DuskIdlInstruction as I["name"]]: I };

type DuskIdlTypeDef = NonNullable<Dusk["types"]>[number];

// Self-referential and keyed by name, so TypeScript decodes only the types a
// call touches. An eager decode of all IDL types would exceed the compiler's
// instantiation budget (see scripts/patch_anchor_idl_types.js).
type DuskDefinedInputs = {
  [D in DuskIdlTypeDef as D["name"]]: TypeDefInput<D["type"]>;
};

/** Every defined type name in the Dusk IDL, in camelCase. */
export type DuskDefinedTypeName = keyof DuskDefinedInputs;

/** The input accepted for a defined IDL type, such as `"swapArgs"`. */
export type DuskDefinedInput<Name extends DuskDefinedTypeName> = DuskDefinedInputs[Name];

type WideInteger = "u64" | "i64" | "u128" | "i128" | "u256" | "i256";
type NarrowNumber = "u8" | "i8" | "u16" | "i16" | "u32" | "i32" | "f32" | "f64";

type ValueInput<T> = T extends WideInteger
  ? IntegerInput
  : T extends NarrowNumber
    ? number
    : T extends "bool"
      ? boolean
      : T extends "string"
        ? string
        : T extends "pubkey"
          ? AddressLike
          : T extends "bytes"
            ? ByteInput
            : T extends { option: infer U }
              ? ValueInput<U> | null
              : T extends { coption: infer U }
                ? ValueInput<U> | null
                : T extends { vec: infer U }
                  ? SequenceInput<U>
                  : T extends { array: readonly [infer U, unknown] }
                    ? SequenceInput<U>
                    : T extends { defined: { name: infer Name extends DuskDefinedTypeName } }
                      ? DuskDefinedInputs[Name]
                      : never;

type SequenceInput<U> = readonly ValueInput<U>[] | (U extends "u8" ? Uint8Array : never);

// An Option may be left out (it encodes as None), and so may a zero-length
// array such as `reserved: [u8; 0]`, which encodes no bytes at all.
type IsOmittable<T> = T extends { option: unknown } | { coption: unknown } | { array: readonly [unknown, 0] }
  ? true
  : false;

type NamedField = { readonly name: string; readonly type: unknown };

// Flattened so editors and compiler errors show the fields, not the IDL.
type Flatten<T> = { [K in keyof T]: T[K] } & {};

type NamedFieldsInput<F extends readonly NamedField[]> = Flatten<
  {
    [K in F[number] as IsOmittable<K["type"]> extends true ? never : K["name"]]: ValueInput<K["type"]>;
  } & {
    [K in F[number] as IsOmittable<K["type"]> extends true ? K["name"] : never]?: ValueInput<K["type"]>;
  }
>;

// Anchor keys tuple fields by index; a tuple or an object keyed "0", "1", …
// encodes the same way.
type TupleFieldsInput<F extends readonly unknown[]> =
  | { readonly [K in keyof F]: ValueInput<F[K]> }
  | { [K in keyof F as K extends `${number}` ? K : never]: ValueInput<F[K]> };

type FieldsInput<F> = F extends readonly []
  ? Record<string, never>
  : F extends readonly NamedField[]
    ? NamedFieldsInput<F>
    : F extends readonly unknown[]
      ? TupleFieldsInput<F>
      : Record<string, never>;

type VariantInput<V> = V extends { fields: infer F } ? FieldsInput<F> : Record<string, never>;

type EnumInput<V extends readonly { readonly name: string }[]> = {
  [K in V[number] as K["name"]]: { [P in K["name"]]: VariantInput<K> } & {
    [P in Exclude<V[number]["name"], K["name"]>]?: undefined;
  };
}[V[number]["name"]];

type TypeDefInput<T> = T extends { kind: "struct"; fields: infer F }
  ? FieldsInput<F>
  : T extends { kind: "struct" }
    ? Record<string, never>
    : T extends { kind: "enum"; variants: infer V extends readonly { readonly name: string }[] }
      ? EnumInput<V>
      : T extends { kind: "type"; alias: infer A }
        ? ValueInput<A>
        : never;

type IsSequenceType<T> = T extends "bytes" | { vec: unknown } | { array: unknown } ? true : false;

type InstructionArgsInput<I> = I extends { args: readonly [] }
  ? undefined | readonly []
  : I extends { args: readonly [infer A extends NamedField] }
    ? IsSequenceType<A["type"]> extends true
      ? readonly [ValueInput<A["type"]>]
      : ValueInput<A["type"]> | readonly [ValueInput<A["type"]>]
    : I extends { args: infer As extends readonly NamedField[] }
      ? { readonly [K in keyof As]: As[K] extends NamedField ? ValueInput<As[K]["type"]> : never }
      : never;

/**
 * Arguments for one instruction, checked against the IDL. A one-argument
 * instruction takes the argument itself or a one-element tuple; an
 * instruction without arguments takes nothing.
 */
export type DuskInstructionArgs<N extends DuskInstructionName = DuskInstructionName> =
  N extends DuskInstructionName ? InstructionArgsInput<DuskInstructionMap[N]> : never;

type AccountItem = {
  readonly name: string;
  readonly optional?: boolean;
  readonly accounts?: readonly unknown[];
};

// Anchor fills these in itself: a fixed address, a PDA, a relation, a signer
// (from the provider) and the event-CPI pair this SDK adds to every call.
type IsResolvable<A> = A extends
  | { address: string }
  | { pda: unknown }
  | { relations: unknown }
  | { signer: true }
  | { name: "eventAuthority" | "program" }
  ? true
  : false;

type IsRequiredAccount<A> = A extends { accounts: infer Sub extends readonly AccountItem[] }
  ? true extends IsRequiredAccount<Sub[number]>
    ? true
    : false
  : IsResolvable<A> extends true
    ? false
    : true;

type AccountValueInput<A> = A extends { accounts: infer Sub extends readonly AccountItem[] }
  ? AccountsInput<Sub>
  : A extends { optional: true }
    ? AddressLike | null
    : AddressLike;

type AccountsInput<As extends readonly AccountItem[]> = Flatten<
  {
    [A in As[number] as IsRequiredAccount<A> extends true ? A["name"] : never]: AccountValueInput<A>;
  } & {
    [A in As[number] as IsRequiredAccount<A> extends true ? never : A["name"]]?: AccountValueInput<A>;
  }
>;

/**
 * Accounts for one instruction. An account Anchor cannot resolve on its own
 * (no fixed address, PDA, relation or signer default) is required; an
 * optional account takes `null` for "none". Nested account groups are nested
 * objects.
 */
export type DuskInstructionAccounts<N extends DuskInstructionName = DuskInstructionName> =
  N extends DuskInstructionName
    ? DuskInstructionMap[N] extends { accounts: infer As extends readonly AccountItem[] }
      ? AccountsInput<As>
      : never
    : never;

// ---------------------------------------------------------------------------
// Run-time check
// ---------------------------------------------------------------------------

type RuntimeIdlType =
  | string
  | { option: RuntimeIdlType }
  | { coption: RuntimeIdlType }
  | { vec: RuntimeIdlType }
  | { array: [RuntimeIdlType, unknown] }
  | { defined: { name: string; generics?: unknown[] } }
  | { generic: string };

type RuntimeIdlFields = { name: string; type: RuntimeIdlType }[] | RuntimeIdlType[];

interface RuntimeIdlTypeDef {
  name: string;
  generics?: unknown[];
  type:
    | { kind: "struct"; fields?: RuntimeIdlFields }
    | { kind: "enum"; variants: { name: string; fields?: RuntimeIdlFields }[] }
    | { kind: "type"; alias: RuntimeIdlType };
}

interface RuntimeIdlAccountItem {
  name: string;
  optional?: boolean;
  accounts?: RuntimeIdlAccountItem[];
}

interface RuntimeIdlInstruction {
  name: string;
  args: { name: string; type: RuntimeIdlType }[];
  accounts: RuntimeIdlAccountItem[];
}

/** The parts of an Anchor IDL the check reads. `Program#idl` satisfies it. */
export interface InstructionInputIdl {
  instructions: readonly unknown[];
  types?: readonly unknown[];
}

/** Thrown when instruction arguments or accounts do not match the IDL. */
export class DuskInstructionInputError extends TypeError {
  constructor(
    readonly instruction: string,
    /** Dotted path of the offending field or account; empty for the whole input. */
    readonly path: string,
    detail: string
  ) {
    super(`Dusk instruction "${instruction}": ${detail}`);
    this.name = "DuskInstructionInputError";
  }
}

interface IdlIndex {
  instructions: Map<string, RuntimeIdlInstruction>;
  types: Map<string, RuntimeIdlTypeDef>;
}

const indexes = new WeakMap<object, IdlIndex>();

function indexIdl(idl: InstructionInputIdl): IdlIndex {
  let index = indexes.get(idl);
  if (!index) {
    index = {
      instructions: new Map(
        (idl.instructions as RuntimeIdlInstruction[]).map((instruction) => [instruction.name, instruction])
      ),
      types: new Map(((idl.types ?? []) as RuntimeIdlTypeDef[]).map((type) => [type.name, type])),
    };
    indexes.set(idl, index);
  }
  return index;
}

function instructionDefinition(idl: InstructionInputIdl, name: string): RuntimeIdlInstruction {
  const instruction = indexIdl(idl).instructions.get(name);
  if (!instruction) throw new Error(`Unknown Dusk instruction: ${name}`);
  return instruction;
}

/**
 * Check `args` against the instruction's IDL arguments and return the
 * positional values Anchor encodes: 64-bit and wider integers as BN, narrower
 * integers as numbers, public keys as PublicKey, absent options as null.
 * Throws `DuskInstructionInputError` naming the instruction and the field.
 */
export function normalizeInstructionArgs(idl: InstructionInputIdl, name: string, args: unknown): unknown[] {
  const instruction = instructionDefinition(idl, name);
  const expected = instruction.args;
  const single = expected.length === 1 ? expected[0]! : undefined;
  const positional =
    args === undefined
      ? []
      : Array.isArray(args) && (single === undefined || isTupleWrapper(args, single.type))
        ? args
        : [args];
  if (positional.length !== expected.length) {
    const names = expected.map((arg) => arg.name).join(", ");
    throw new DuskInstructionInputError(
      name,
      "",
      expected.length === 0
        ? `expects no arguments, received ${positional.length}`
        : `expects ${expected.length} argument${expected.length === 1 ? "" : "s"} (${names}), received ${positional.length}`
    );
  }
  const walker = new ArgsWalker(name, indexIdl(idl).types);
  return expected.map((arg, index) =>
    // With one argument the caller passes its fields directly, so paths start
    // at those fields; a primitive argument is named by itself.
    walker.value(arg.type, positional[index], expected.length === 1 && isStructLike(arg.type, walker) ? "" : arg.name)
  );
}

/**
 * Check an account map against the instruction's IDL accounts and return it
 * with every address as a PublicKey. Throws on an account name the
 * instruction does not declare, which Anchor would otherwise ignore while it
 * resolved (or defaulted) the real account.
 */
export function normalizeInstructionAccounts(
  idl: InstructionInputIdl,
  name: string,
  accounts: unknown
): Record<string, unknown> {
  return accountGroup(name, instructionDefinition(idl, name).accounts, accounts, "");
}

function accountGroup(
  instruction: string,
  items: RuntimeIdlAccountItem[],
  value: unknown,
  path: string
): Record<string, unknown> {
  if (!isRecord(value)) {
    throw new DuskInstructionInputError(
      instruction,
      path,
      path ? `account group "${path}" must be an object of accounts, received ${describe(value)}` : `accounts must be an object, received ${describe(value)}`
    );
  }
  const byName = new Map(items.map((item) => [item.name, item]));
  const out: Record<string, unknown> = {};
  for (const [key, child] of Object.entries(value)) {
    const childPath = join(path, key);
    const item = byName.get(key);
    if (!item) {
      throw new DuskInstructionInputError(
        instruction,
        childPath,
        `unknown account "${childPath}"${suggest(key, [...byName.keys()])}`
      );
    }
    if (child === undefined) continue;
    if (item.accounts) {
      out[key] = accountGroup(instruction, item.accounts, child, childPath);
      continue;
    }
    // Anchor maps null on an optional account to "none" and ignores it on any
    // other account, so null passes through unchanged.
    out[key] = child === null ? null : accountAddress(instruction, childPath, child);
  }
  return out;
}

function accountAddress(instruction: string, path: string, value: unknown): PublicKey {
  const key = publicKeyOrUndefined(value);
  if (!key) {
    throw new DuskInstructionInputError(
      instruction,
      path,
      `account "${path}" must be a PublicKey or base58 address, received ${describe(value)}`
    );
  }
  return key;
}

const INTEGER_BOUNDS: Record<string, { min: bigint; max: bigint; signed: boolean; bits: number }> = {
  u8: { min: 0n, max: 2n ** 8n - 1n, signed: false, bits: 8 },
  i8: { min: -(2n ** 7n), max: 2n ** 7n - 1n, signed: true, bits: 8 },
  u16: { min: 0n, max: 2n ** 16n - 1n, signed: false, bits: 16 },
  i16: { min: -(2n ** 15n), max: 2n ** 15n - 1n, signed: true, bits: 16 },
  u32: { min: 0n, max: 2n ** 32n - 1n, signed: false, bits: 32 },
  i32: { min: -(2n ** 31n), max: 2n ** 31n - 1n, signed: true, bits: 32 },
  u64: { min: 0n, max: 2n ** 64n - 1n, signed: false, bits: 64 },
  i64: { min: -(2n ** 63n), max: 2n ** 63n - 1n, signed: true, bits: 64 },
  u128: { min: 0n, max: 2n ** 128n - 1n, signed: false, bits: 128 },
  i128: { min: -(2n ** 127n), max: 2n ** 127n - 1n, signed: true, bits: 128 },
  u256: { min: 0n, max: 2n ** 256n - 1n, signed: false, bits: 256 },
  i256: { min: -(2n ** 255n), max: 2n ** 255n - 1n, signed: true, bits: 256 },
};

class ArgsWalker {
  constructor(
    private readonly instruction: string,
    private readonly types: Map<string, RuntimeIdlTypeDef>
  ) {}

  typeDef(name: string): RuntimeIdlTypeDef {
    const definition = this.types.get(name);
    if (!definition) throw new Error(`Dusk IDL is missing type ${name}`);
    if (definition.generics?.length) throw new Error(`Generic IDL type ${name} is not supported`);
    return definition;
  }

  fail(path: string, detail: string): never {
    throw new DuskInstructionInputError(this.instruction, path, detail);
  }

  invalid(path: string, expectation: string, value: unknown): never {
    this.fail(path, `${label(path)} must be ${expectation}, received ${describe(value)}`);
  }

  value(type: RuntimeIdlType, value: unknown, path: string): unknown {
    if (typeof type === "string") return this.primitive(type, value, path);
    if ("option" in type) return value === undefined || value === null ? null : this.value(type.option, value, path);
    if ("coption" in type) return value === undefined || value === null ? null : this.value(type.coption, value, path);
    if ("vec" in type) return this.sequence(type.vec, value, path, undefined);
    if ("array" in type) {
      const [element, length] = type.array;
      if (typeof length !== "number") throw new Error(`Generic array length at ${path} is not supported`);
      return this.sequence(element, value, path, length);
    }
    if ("defined" in type) return this.defined(this.typeDef(type.defined.name), value, path);
    throw new Error(`Unsupported IDL type at ${path}: ${JSON.stringify(type)}`);
  }

  private primitive(type: string, value: unknown, path: string): unknown {
    const bounds = INTEGER_BOUNDS[type];
    if (bounds) {
      const integer = this.integer(bounds, value, path);
      return bounds.bits > 32 ? new AnchorBN(integer.toString()) : Number(integer);
    }
    switch (type) {
      case "bool":
        if (typeof value !== "boolean") this.invalid(path, "a boolean", value);
        return value;
      case "string":
        if (typeof value !== "string") this.invalid(path, "a string", value);
        return value;
      case "pubkey": {
        const key = publicKeyOrUndefined(value);
        if (!key) this.invalid(path, "a PublicKey or base58 address", value);
        return key;
      }
      case "bytes":
        return Buffer.from(this.bytes(value, path, undefined));
      case "f32":
      case "f64":
        if (typeof value !== "number" || !Number.isFinite(value)) this.invalid(path, "a finite number", value);
        return value;
      default:
        throw new Error(`Unsupported IDL type at ${path}: ${type}`);
    }
  }

  private integer(bounds: (typeof INTEGER_BOUNDS)[string], value: unknown, path: string): bigint {
    const kind = `${bounds.signed ? "a signed" : "an unsigned"} ${bounds.bits}-bit integer`;
    const accepted = bounds.bits > 32 ? "BN, bigint, safe-integer number or decimal string" : "safe-integer number, bigint or decimal string";
    let integer: bigint;
    if (typeof value === "bigint") {
      integer = value;
    } else if (typeof value === "number") {
      if (!Number.isSafeInteger(value)) this.invalid(path, `${kind} (${accepted})`, value);
      integer = BigInt(value);
    } else if (typeof value === "string") {
      if (!/^-?\d+$/.test(value)) this.invalid(path, `${kind} (${accepted})`, value);
      integer = BigInt(value);
    } else if (isBNLike(value)) {
      integer = BigInt(value.toString(10));
    } else {
      this.invalid(path, `${kind} (${accepted})`, value);
    }
    if (integer < bounds.min || integer > bounds.max) {
      this.fail(path, `${label(path)} must be between ${bounds.min} and ${bounds.max}, received ${integer}`);
    }
    return integer;
  }

  private bytes(value: unknown, path: string, length: number | undefined): Uint8Array {
    let bytes: Uint8Array;
    if (value instanceof Uint8Array) {
      bytes = value;
    } else if (Array.isArray(value) && value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)) {
      bytes = Uint8Array.from(value);
    } else {
      this.invalid(path, length === undefined ? "bytes (Uint8Array or byte array)" : `${length} bytes (Uint8Array or byte array)`, value);
    }
    if (length !== undefined && bytes.length !== length) {
      this.fail(path, `${label(path)} must hold exactly ${length} bytes, received ${bytes.length}`);
    }
    return bytes;
  }

  private sequence(element: RuntimeIdlType, value: unknown, path: string, length: number | undefined): unknown[] {
    if (element === "u8" && value instanceof Uint8Array) {
      return [...this.bytes(value, path, length)];
    }
    if (!Array.isArray(value)) {
      this.invalid(path, length === undefined ? "an array" : `an array of ${length}`, value);
    }
    // A short fixed array would leave its tail zeroed and a long one would
    // spill into the next field, so the length is exact.
    if (length !== undefined && value.length !== length) {
      this.fail(path, `${label(path)} must hold exactly ${length} elements, received ${value.length}`);
    }
    return value.map((item, index) => this.value(element, item, `${path}[${index}]`));
  }

  private defined(definition: RuntimeIdlTypeDef, value: unknown, path: string): unknown {
    const type = definition.type;
    if (type.kind === "type") return this.value(type.alias, value, path);
    if (type.kind === "struct") return this.fields(type.fields, value, path, definition.name);
    if (!isRecord(value)) this.invalid(path, `one ${definition.name} variant, such as { ${type.variants[0]?.name}: {} }`, value);
    const keys = Object.keys(value);
    const names = type.variants.map((variant) => variant.name);
    if (keys.length !== 1) {
      this.fail(
        path,
        `${label(path)} must name exactly one ${definition.name} variant (${names.join(", ")}), received ${keys.length === 0 ? "none" : keys.join(", ")}`
      );
    }
    const key = keys[0]!;
    const variant = type.variants.find((candidate) => candidate.name === key);
    if (!variant) {
      const variantPath = join(path, key);
      this.fail(variantPath, `unknown ${definition.name} variant "${key}"${suggest(key, names)}`);
    }
    const variantValue = value[key];
    // Anchor encodes nothing for a unit variant; `{}` is the canonical value.
    if (!variant.fields?.length && (variantValue === undefined || variantValue === null)) return { [key]: {} };
    return { [key]: this.fields(variant.fields, variantValue, join(path, key), `${definition.name}.${key}`) };
  }

  private fields(fields: RuntimeIdlFields | undefined, value: unknown, path: string, typeName: string): unknown {
    const list = fields ?? [];
    if (list.length === 0) {
      if (!isRecord(value)) this.invalid(path, "an empty object {}", value);
      const [extra] = Object.keys(value);
      if (extra !== undefined) this.fail(join(path, extra), `unknown field "${join(path, extra)}"; ${typeName} has no fields`);
      return {};
    }
    const named = isNamedField(list[0]);
    if (!named) {
      const tuple = list as RuntimeIdlType[];
      const items = Array.isArray(value) ? value : isRecord(value) ? tupleItems(value, tuple.length, this, path) : undefined;
      if (!items) this.invalid(path, `a ${typeName} tuple of ${tuple.length}`, value);
      if (items.length !== tuple.length) {
        this.fail(path, `${label(path)} must hold exactly ${tuple.length} fields, received ${items.length}`);
      }
      return tuple.map((type, index) => this.value(type, items[index], join(path, String(index))));
    }
    if (!isRecord(value)) this.invalid(path, `a ${typeName} object`, value);
    const definitions = list as { name: string; type: RuntimeIdlType }[];
    const known = definitions.map((field) => field.name);
    for (const key of Object.keys(value)) {
      if (!known.includes(key)) {
        const fieldPath = join(path, key);
        this.fail(fieldPath, `unknown field "${fieldPath}"${suggest(key, known)}`);
      }
    }
    const out: Record<string, unknown> = {};
    for (const field of definitions) {
      const fieldPath = join(path, field.name);
      const fieldValue = value[field.name];
      if (fieldValue === undefined && isEmptyArray(field.type)) {
        out[field.name] = [];
        continue;
      }
      if (fieldValue === undefined && !isOptional(field.type)) {
        this.fail(fieldPath, `missing field "${fieldPath}"`);
      }
      out[field.name] = this.value(field.type, fieldValue, fieldPath);
    }
    return out;
  }
}

function tupleItems(value: Record<string, unknown>, length: number, walker: ArgsWalker, path: string): unknown[] {
  const items: unknown[] = [];
  for (const key of Object.keys(value)) {
    const index = Number(key);
    if (!Number.isInteger(index) || index < 0 || index >= length || String(index) !== key) {
      walker.fail(join(path, key), `unknown field "${join(path, key)}"; tuple fields are keyed 0–${length - 1}`);
    }
  }
  for (let index = 0; index < length; index += 1) {
    if (!(String(index) in value)) walker.fail(join(path, String(index)), `missing field "${join(path, String(index))}"`);
    items.push(value[String(index)]);
  }
  return items;
}

function isOptional(type: RuntimeIdlType): boolean {
  return typeof type === "object" && ("option" in type || "coption" in type);
}

function isEmptyArray(type: RuntimeIdlType): boolean {
  return typeof type === "object" && "array" in type && type.array[1] === 0;
}

function isNamedField(field: unknown): field is { name: string; type: RuntimeIdlType } {
  return typeof field === "object" && field !== null && "name" in field && "type" in field;
}

function isStructLike(type: RuntimeIdlType, walker: ArgsWalker): boolean {
  if (typeof type !== "object" || !("defined" in type)) return false;
  return walker.typeDef(type.defined.name).type.kind !== "type";
}

// A one-argument instruction also accepts `[value]`. When that argument is
// itself a sequence, an array is only a wrapper if it has exactly one element
// that is itself array-like; otherwise it is the value.
function isTupleWrapper(args: unknown[], type: RuntimeIdlType): boolean {
  const sequence = type === "bytes" || (typeof type === "object" && ("vec" in type || "array" in type));
  if (!sequence) return true;
  return args.length === 1 && (Array.isArray(args[0]) || args[0] instanceof Uint8Array);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    !(value instanceof Uint8Array) &&
    !(value instanceof PublicKey) &&
    !isBNLike(value)
  );
}

function isBNLike(value: unknown): value is BN {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { toArrayLike?: unknown }).toArrayLike === "function" &&
    Array.isArray((value as { words?: unknown }).words)
  );
}

function publicKeyOrUndefined(value: unknown): PublicKey | undefined {
  if (value instanceof PublicKey) return value;
  try {
    if (typeof value === "string") return new PublicKey(value);
    // A PublicKey from another copy of @solana/web3.js.
    if (typeof value === "object" && value !== null && typeof (value as { toBase58?: unknown }).toBase58 === "function") {
      return new PublicKey((value as { toBase58(): string }).toBase58());
    }
  } catch {
    return undefined;
  }
  return undefined;
}

function join(path: string, key: string): string {
  return path ? `${path}.${key}` : key;
}

function label(path: string): string {
  return path ? `"${path}"` : "the argument";
}

function describe(value: unknown): string {
  if (value === null) return "null";
  if (value === undefined) return "undefined";
  if (Array.isArray(value)) return `an array of ${value.length}`;
  if (value instanceof Uint8Array) return `${value.length} bytes`;
  if (value instanceof PublicKey) return `PublicKey ${value.toBase58()}`;
  if (isBNLike(value)) return `BN ${value.toString(10)}`;
  if (typeof value === "string") return `string ${JSON.stringify(value.length > 48 ? `${value.slice(0, 45)}...` : value)}`;
  if (typeof value === "bigint") return `bigint ${value}`;
  if (typeof value === "number") return `number ${value}`;
  if (typeof value === "object") return "an object";
  return typeof value;
}

function suggest(key: string, candidates: string[]): string {
  let best: string | undefined;
  let bestDistance = Infinity;
  for (const candidate of candidates) {
    const distance = editDistance(key.toLowerCase(), candidate.toLowerCase());
    if (distance < bestDistance) {
      best = candidate;
      bestDistance = distance;
    }
  }
  return best !== undefined && bestDistance <= Math.max(2, Math.floor(key.length / 3)) ? `; did you mean "${best}"?` : "";
}

function editDistance(left: string, right: string): number {
  let previous = Array.from({ length: right.length + 1 }, (_, index) => index);
  for (let i = 1; i <= left.length; i += 1) {
    const current = [i];
    for (let j = 1; j <= right.length; j += 1) {
      current[j] = Math.min(
        previous[j]! + 1,
        current[j - 1]! + 1,
        previous[j - 1]! + (left[i - 1] === right[j - 1] ? 0 : 1)
      );
    }
    previous = current;
  }
  return previous[right.length]!;
}
