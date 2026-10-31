# Third-party notices

The `add_transfer_hook_accounts` function in `programs/dusk/src/token.rs` adapts account resolution and privilege de-escalation from the Solana Program Library packages `spl-transfer-hook-interface` 0.9.0 and `spl-tlv-account-resolution` 0.9.0. These packages are licensed under the Apache License, Version 2.0; see [the license text](licenses/Apache-2.0.txt).

Sources: `spl-transfer-hook-interface/src/onchain.rs` (`add_extra_accounts_for_execute_cpi`) and `spl-tlv-account-resolution/src/state.rs` (`add_to_cpi_instruction` and privilege de-escalation), as distributed in the versions pinned by Cargo.lock. This adaptation retains a reusable account-data borrow list and reserves capacity once to avoid repeated allocations with Solana’s bump allocator. The corresponding differential test compares account order, privileges, dynamic seeds and errors with the upstream implementation.

This notice does not replace the repository’s Business Source License. The adapted helper remains subject to its Apache-2.0 terms.
