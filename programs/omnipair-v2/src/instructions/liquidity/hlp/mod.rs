mod deposit_single_sided;
mod withdraw_single_sided;

use anchor_lang::prelude::*;

use crate::state::{YieldAccount, YieldTokenKind};

pub use deposit_single_sided::*;
pub use withdraw_single_sided::*;

fn initialize_or_validate_hlp_yield_account(
    yield_account: &mut Account<YieldAccount>,
    owner: Pubkey,
    market: Pubkey,
    asset_mint: Pubkey,
    bump: u8,
) -> Result<()> {
    if yield_account.owner == Pubkey::default() {
        yield_account.initialize(owner, market, asset_mint, YieldTokenKind::Hlp, owner, bump);
    }
    yield_account.assert_account(owner, market, asset_mint, YieldTokenKind::Hlp)
}
