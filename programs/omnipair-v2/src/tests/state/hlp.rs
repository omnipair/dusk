use super::*;
    use crate::constants::NAD;

    #[test]
    fn hlp_vault_checkpoints_owned_ylp_revenue_into_hlp_indexes() {
        let mut vault = HlpVault {
            ylp_shares: 50,
            hlp_supply: 25,
            ..HlpVault::default()
        };
        let mut base_side = MarketSide::default();
        let quote_side = MarketSide::default();
        base_side.fees.swap_fee_growth_index_nad = 2 * NAD as u128;
        base_side.fees.interest_growth_index_nad = 3 * NAD as u128;

        vault
            .checkpoint_yield_from_ylp(&base_side, &quote_side)
            .unwrap();

        assert_eq!(vault.base_swap_fee_growth_index_nad, 4 * NAD as u128);
        assert_eq!(vault.base_interest_growth_index_nad, 6 * NAD as u128);
        assert_eq!(
            vault.base_swap_fee_checkpoint_nad,
            base_side.fees.swap_fee_growth_index_nad
        );
        assert_eq!(
            vault.base_interest_checkpoint_nad,
            base_side.fees.interest_growth_index_nad
        );
    }

    #[test]
    fn hlp_debt_clearance_tracks_realized_interest_separately() {
        let mut vault = HlpVault::default();
        vault.add_debt_shares(1_000).unwrap();
        vault.add_debt_principal(1_000).unwrap();

        let clearance = vault
            .clear_debt_repay(550, 500, (NAD as u128) * 11 / 10)
            .unwrap();

        assert_eq!(clearance.interest_paid, 50);
        assert_eq!(clearance.principal_paid, 500);
        assert_eq!(clearance.debt_reduced, 550);
        assert_eq!(vault.debt_principal, 500);
    }

    #[test]
    fn hlp_debt_clearance_reports_actual_debt_reduced_after_rounded_share_burn() {
        let mut vault = HlpVault::default();
        vault.add_debt_shares(100).unwrap();
        vault.add_debt_principal(100).unwrap();

        let clearance = vault
            .clear_debt_repay(2, 2, (NAD as u128) * 3 / 2)
            .unwrap();

        assert_eq!(clearance.shares_burned, 2);
        assert_eq!(clearance.debt_reduced, 3);
        assert_eq!(clearance.remaining_debt, 147);
        assert_eq!(clearance.principal_paid, 1);
        assert_eq!(clearance.interest_paid, 1);
        assert_eq!(vault.debt_shares, 98);
        assert_eq!(vault.debt_principal, 98);
    }
