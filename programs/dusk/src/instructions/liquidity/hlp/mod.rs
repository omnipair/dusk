mod deposit_single_sided;
mod withdraw_single_sided;

use anchor_lang::prelude::*;

use crate::{
    constants::{FUTARCHY_AUTHORITY_SEED_PREFIX, MARKET_V2_SEED_PREFIX},
    errors::ErrorCode,
    state::{FutarchyAuthority, HlpYieldEligibility, Market, MarketAsset, MarketSide, ProtocolAuctionSplit},
};

pub use deposit_single_sided::*;
pub use withdraw_single_sided::*;

/// Validate the two program authorities outside Anchor's generated account
/// parser. Constructing Market-derived seed arrays while the large Market
/// deserialization frame is live can exceed SBF's 4 KiB stack limit.
pub(crate) fn validate_hlp_authority_pdas(
    market: &Market,
    market_key: Pubkey,
    futarchy_authority: &FutarchyAuthority,
    futarchy_authority_key: Pubkey,
) -> Result<()> {
    let market_bump = [market.bump];
    let expected_market = Pubkey::create_program_address(
        &[
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
            &market_bump,
        ],
        &crate::ID,
    )
    .map_err(|_| error!(ErrorCode::InvalidMarket))?;
    require_keys_eq!(market_key, expected_market, ErrorCode::InvalidMarket);

    let futarchy_bump = [futarchy_authority.bump];
    let expected_futarchy =
        Pubkey::create_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX, &futarchy_bump], &crate::ID)
            .map_err(|_| error!(ErrorCode::InvalidFutarchyAuthority))?;
    require_keys_eq!(
        futarchy_authority_key,
        expected_futarchy,
        ErrorCode::InvalidFutarchyAuthority
    );
    Ok(())
}

/// Reconcile a partial direct Token-2022 hLP burn before the next hLP entry or
/// exit. Historical nested yLP growth is published against the old stored
/// denominator first; only future growth and principal pricing use the smaller
/// live supply. A complete direct burn is intentionally fail-closed because no
/// holder remains who can authorize the vault's normal final exit.
pub(crate) fn reconcile_live_hlp_supply(
    market: &mut Market,
    target_asset: MarketAsset,
    live_mint_supply: u64,
) -> Result<()> {
    market.checkpoint_hlp_yield_from_ylp(target_asset)?;
    let stored_supply = match target_asset {
        MarketAsset::Base => &mut market.base_hlp_vault.hlp_supply,
        MarketAsset::Quote => &mut market.quote_hlp_vault.hlp_supply,
    };
    require_gte!(*stored_supply, live_mint_supply, ErrorCode::InvalidHlpMintSupply);
    if *stored_supply > 0 {
        require!(live_mint_supply > 0, ErrorCode::InvalidHlpMintSupply);
    }
    if live_mint_supply < *stored_supply {
        *stored_supply = live_mint_supply;
    }
    Ok(())
}

pub(crate) fn record_hlp_interest_credit(
    borrowed_side: &mut MarketSide,
    actual_interest_credit: u64,
    manager_fee_bps: u16,
    protocol_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
    eligible_ylp_supply: u64,
) -> Result<()> {
    borrowed_side.record_interest_credit_with_supply(
        actual_interest_credit,
        manager_fee_bps,
        protocol_fee_bps,
        protocol_auction_split,
        0,
        eligible_ylp_supply,
    )?;
    Ok(())
}

/// Publish one inline hLP interest-vault credit against ownership captured
/// before predictive or post-trade rebalancing. Both hLP vaults own ordinary
/// yLP and therefore both must receive their historical share of either
/// asset's interest stream.
pub(crate) fn record_inline_hlp_interest_credit(
    market: &mut Market,
    borrowed_asset: MarketAsset,
    actual_interest_credit: u64,
    manager_fee_bps: u16,
    protocol_fee_bps: u16,
    protocol_auction_split: ProtocolAuctionSplit,
    eligibility: HlpYieldEligibility,
) -> Result<()> {
    record_hlp_interest_credit(
        market.side_mut(borrowed_asset),
        actual_interest_credit,
        manager_fee_bps,
        protocol_fee_bps,
        protocol_auction_split,
        eligibility.ylp_supply,
    )?;
    market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, eligibility.base_hlp_ylp_shares)?;
    market.checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, eligibility.quote_hlp_ylp_shares)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{constants::YIELD_GROWTH_SCALE_Q64, state::accrue_fee_liability_with_remainder};
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
        let eligible_ylp_supply = borrowed_side.shares.ylp_supply;

        record_hlp_interest_credit(
            &mut borrowed_side,
            actual_interest_credit,
            0,
            0,
            ProtocolAuctionSplit::default(),
            eligible_ylp_supply,
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

    #[test]
    fn inline_interest_uses_operation_start_ownership_for_spot_and_leverage() {
        let mut market = Market::default();
        market.base_side.shares.ylp_supply = 100;
        market.quote_side.shares.ylp_supply = 100;
        market.base_hlp_vault.ylp_shares = 50;
        market.base_hlp_vault.hlp_supply = 50;
        market.quote_hlp_vault.ylp_shares = 25;
        market.quote_hlp_vault.hlp_supply = 25;
        let eligibility = HlpYieldEligibility {
            ylp_supply: 100,
            base_hlp_ylp_shares: 50,
            quote_hlp_ylp_shares: 25,
        };

        // Model a same-operation burn from base-hLP and mint into quote-hLP.
        // Neither mutation may alter who earned the already-accrued interest.
        market.base_side.shares.ylp_supply = 75;
        market.quote_side.shares.ylp_supply = 75;
        market.base_hlp_vault.ylp_shares = 0;
        market.quote_hlp_vault.ylp_shares = 50;
        record_inline_hlp_interest_credit(
            &mut market,
            MarketAsset::Quote,
            10,
            0,
            0,
            ProtocolAuctionSplit::default(),
            eligibility,
        )
        .unwrap();

        assert_eq!(market.quote_side.fees.interest_liability, 10);
        assert_eq!(market.quote_side.fees.unallocated_interest_liability, 0);
        let global_growth = market.quote_side.fees.interest_growth_index_q64;
        let (base_hlp_whole, base_hlp_remainder) =
            accrue_fee_liability_with_remainder(50, global_growth, 0, 0).unwrap();
        assert_eq!(base_hlp_whole, 4);
        assert_eq!(market.base_hlp_vault.quote_interest_remainder_q64, base_hlp_remainder);
        assert_eq!(
            market.base_hlp_vault.quote_interest_growth_index_q64,
            crate::state::distribute_growth_q64(base_hlp_whole, 50, 0).unwrap().0
        );
        // Quote-hLP receives only its 25 start shares, not its 50 post-mint
        // shares: two whole atoms plus one-half atom at the yLP layer.
        let (quote_hlp_whole, quote_hlp_remainder) =
            accrue_fee_liability_with_remainder(25, global_growth, 0, 0).unwrap();
        assert_eq!(quote_hlp_whole, 2);
        assert_eq!(market.quote_hlp_vault.quote_interest_remainder_q64, quote_hlp_remainder);
        let (ordinary_amount, ordinary_remainder) =
            accrue_fee_liability_with_remainder(25, market.quote_side.fees.interest_growth_index_q64, 0, 0).unwrap();
        assert_eq!(ordinary_amount, 2);
        assert_eq!(ordinary_remainder, quote_hlp_remainder);
        market.quote_side.fees.assert_backed().unwrap();
    }

    #[test]
    fn partial_direct_burn_checkpoints_old_supply_before_reconciling() {
        let mut market = Market::default();
        market.base_side.shares.ylp_supply = 10;
        market.quote_side.shares.ylp_supply = 10;
        market.base_side.fees.unallocated_swap_fee_liability = 10;
        market.base_hlp_vault.ylp_shares = 10;
        market.base_hlp_vault.hlp_supply = 10;

        reconcile_live_hlp_supply(&mut market, MarketAsset::Base, 5).unwrap();

        assert_eq!(market.base_hlp_vault.hlp_supply, 5);
        assert_eq!(
            market.base_hlp_vault.base_swap_fee_growth_index_q64,
            YIELD_GROWTH_SCALE_Q64
        );
        assert_eq!(market.base_hlp_vault.unallocated_base_swap_fee_amount, 0);
    }

    #[test]
    fn direct_burn_reconciliation_rejects_external_minting_and_all_burn_zombie() {
        let mut market = Market::default();
        market.quote_hlp_vault.hlp_supply = 1;

        for invalid_live_supply in [0, 2] {
            let error = reconcile_live_hlp_supply(&mut market, MarketAsset::Quote, invalid_live_supply).unwrap_err();
            match error {
                anchor_lang::error::Error::AnchorError(error) => {
                    assert_eq!(error.error_name, "InvalidHlpMintSupply")
                }
                other => panic!("unexpected error: {other:?}"),
            }
            assert_eq!(market.quote_hlp_vault.hlp_supply, 1);
        }
    }
}
