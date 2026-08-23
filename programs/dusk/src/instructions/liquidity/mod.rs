mod harvest;
mod hlp;
mod initialize_lp_transfer_hook;
mod initialize_yield_accounts;
mod set_yield_recipient;
mod ylp;

pub use harvest::*;
pub use hlp::*;
pub use initialize_lp_transfer_hook::*;
pub use initialize_yield_accounts::*;
pub use set_yield_recipient::*;
pub use ylp::*;

use crate::state::{YieldAccount, YieldTokenKind};
use anchor_lang::prelude::*;

pub(super) fn initialize_or_validate_yield_account(
    yield_account: &mut YieldAccount,
    owner: Pubkey,
    market: Pubkey,
    lp_mint: Pubkey,
    asset_mint: Pubkey,
    token_kind: YieldTokenKind,
    bump: u8,
) -> Result<bool> {
    let initialized = yield_account.owner == Pubkey::default();
    if initialized {
        yield_account.initialize(owner, market, lp_mint, asset_mint, token_kind, owner, bump);
    }
    yield_account.assert_account(owner, market, lp_mint, asset_mint, token_kind)?;
    Ok(initialized)
}
