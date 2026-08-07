# Dusk Rust Style

Dusk follows the readable instruction style established in Omnipair V1 while
keeping Dusk's stricter safety, testing, and code-shape rules.

## Architecture

Dusk uses three direct layers:

1. instruction adapters own Anchor accounts, validation, token CPIs, custody
   reconciliation, and events;
2. account methods and the small `market/` domain modules own checked state
   mutation and return receipts only when another phase needs the result;
3. pure math owns stateless fixed-point algorithms and quotes.

`state/` is account-shaped: every production state file defines exactly one
`#[account]` type. Serialized child values stay beside their owning account;
they do not become files or directories of their own. This is the Omnipair V1
rule: a state file represents an on-chain account, not an architectural noun.

Use an explicit state machine only for state that persists across transactions,
such as proposal, auction, or ramp lifecycles. A swap, borrow, repayment, or LP
change is an atomic domain operation, not a separate state-machine framework.
The `market/` directory exists only for the few large behaviors shared by
multiple instructions: AMM, liquidity, lending, and leverage.

## Instruction files

Use this order:

1. external imports, then one separated `crate` import group;
2. instruction argument types;
3. the Anchor `Accounts` type;
4. the `Accounts` implementation: validation, shared update hook, then handler;
5. the external test-module include, when present.

Keep program entrypoints thin. Validation and business logic belong on the
instruction's `Accounts` type or on the relevant state type.

An instruction handler should read from top to bottom as one narrative:

```text
bind accounts and arguments
derive direction and local values
calculate and validate
execute token CPIs
reconcile state and custody
emit final events
```

Alias or destructure `ctx.accounts` when it makes that narrative easier to
follow. Preserve explicit `Context` lifetimes, nested borrow scopes, boxing,
and raw-account handling when they exist for SBF stack, compute, or
Token-2022 correctness.

## Spacing and comments

- `rustfmt` is authoritative; Dusk uses a 120-column limit.
- Separate meaningful account groups and handler phases with one blank line.
- Use short `//` comments to name a business phase or explain a formula,
  rounding direction, required ordering, or safety invariant.
- Use `///` for public behavior and required `CHECK:` safety explanations.
- Do not narrate obvious syntax or preserve obsolete implementation history.
- Keep unit suffixes explicit: `_bps`, `_nad`, `_q64`, `_slots`, `_shares`,
  `_amount_in`, and `_amount_out`.

## Dusk-specific guardrails

- Never reorder instruction accounts or program entrypoints for style alone;
  their order is part of the IDL and client surface.
- Do not move runtime validation into Anchor constraints without a separate
  compute, error-semantics, and stack analysis.
- Do not change event mechanisms, CPI order, state-mutation order, math, or
  rounding in a style-only change.
- Instruction test bodies live under `src/tests`; production files contain
  only the `#[cfg(test)]` include bridge.
- Every private, non-recursive production helper needs at least two genuine
  production call sites. Inline one-use helpers.
- Prefer the least code that states the business operation directly. Do not
  extract wrappers, forwarding methods, builders, traits, or helper functions
  merely for symmetry, naming, testing, or visual tidiness. Extract code only
  when reuse, recursion, borrow/stack constraints, or a genuinely independent
  algorithm requires a boundary.
- Every `state/*.rs` file except `mod.rs` owns exactly one `#[account]`; do not
  create nested production modules under `state/`.
- Do not add a generic `transitions` module or `Transition` trait. Place checked
  mutations with the domain that owns their invariants.
- Keep complex money-moving entrypoints separate. Group only short related
  administrative entrypoints whose account and mutation stories remain clear.

## Required checks

```bash
cargo fmt --all -- --check
yarn check:hygiene
yarn check:code-shape
yarn check:clippy
cargo test -p dusk --lib
```
