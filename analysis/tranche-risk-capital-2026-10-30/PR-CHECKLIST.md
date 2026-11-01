# Tranche implementation PR and cleanup

User instruction, 31 October 2026: commit the research and documentation with the
implementation PR, then remove generated builds, caches and temporary files.

## Include in the PR

- Native implementation, tests, regenerated interfaces and SDK changes.
- This research directory: source, assumptions, reference models, source citations,
  verification, numerical results and the final review document. Keep older
  experiments clearly identified as historical; do not relabel their results as
  validation of the new defaults.
- Referenced Omnipair brand artwork/tokens/font needed to reproduce the visuals.
- The V1 controller inspection, reproduction, current parameter defaults and
  governance update requirements.

Scope staging to these files and the native implementation. Do not add unrelated
dirty work from this shared checkout. Follow the repository branch-name policy
and run every required CI command before any commit, including a documentation
commit. The implementation includes this research; the final verification manifest records
the completed checks. The task attachment identifies the implementation PR.

## Cleanup after validation and commit

First confirm the intended source, results and figures are committed and the PR
contains them. Then remove task-owned generated Rust targets, Python bytecode,
plotting caches, temporary reproduction executables and disposable render files.
Record the selected paths and bytes recovered in a cleanup receipt.

Preserve source, published review artifacts, raw research evidence, lockfiles,
wallet/program keypairs, SDK interface sources and active worktrees. Do not remove
another task's build directory or a compiler's active output. Inspect generated
target directories for identity files before deleting them; a broad target wipe
must not remove program keypairs. Preserve any needed ignored files before archiving
a worktree. Cleanup follows successful validation so it does not force an avoidable
rebuild during the PR checks.
