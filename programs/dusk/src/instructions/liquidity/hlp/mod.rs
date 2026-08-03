mod crank_rebalance;
mod deposit_single_sided;
mod withdraw_single_sided;

use anchor_lang::prelude::*;

use crate::state::{MarketSide, ProtocolAuctionSplit, YieldAccount, YieldTokenKind};

pub use crank_rebalance::*;
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

pub(crate) fn record_hlp_interest_credit(
    borrowed_side: &mut MarketSide,
    actual_interest_credit: u64,
    manager_fee_bps: u16,
    protocol_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
) -> Result<()> {
    borrowed_side.record_interest_credit(
        actual_interest_credit,
        manager_fee_bps,
        protocol_fee_bps,
        protocol_auction_split,
        0,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spl_token_2022::extension::transfer_fee::TransferFee;

    #[test]
    fn token_2022_hlp_interest_books_only_actual_vault_credit() {
        let transfer_fee = TransferFee {
            epoch: 0_u64.into(),
            maximum_fee: u64::MAX.into(),
            transfer_fee_basis_points: 300_u16.into(),
        };
        let gross_interest_paid = 10_000;
        let actual_interest_credit = transfer_fee.calculate_post_fee_amount(gross_interest_paid).unwrap();
        let mut borrowed_side = MarketSide::default();

        record_hlp_interest_credit(
            &mut borrowed_side,
            actual_interest_credit,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

        assert!(actual_interest_credit < gross_interest_paid);
        assert_eq!(actual_interest_credit, 9_700);
        assert_eq!(borrowed_side.fees.interest_vault_balance, actual_interest_credit);
        assert_eq!(
            borrowed_side.fees.unallocated_interest_liability,
            actual_interest_credit
        );
        borrowed_side.fees.assert_backed().unwrap();
    }
}
