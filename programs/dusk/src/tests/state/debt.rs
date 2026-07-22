use super::*;

    #[test]
    fn add_margin_principal_accumulates_per_side() {
        let mut debt = Debt::default();
        debt.add_margin_principal(MarketAsset::Base, 1_000).unwrap();
        debt.add_margin_principal(MarketAsset::Base, 500).unwrap();
        debt.add_margin_principal(MarketAsset::Quote, 200).unwrap();
        assert_eq!(debt.fixed_base_principal, 1_500);
        assert_eq!(debt.fixed_quote_principal, 200);
    }

    #[test]
    fn realize_margin_repay_is_all_principal_without_interest() {
        let mut debt = Debt {
            fixed_base_shares: 1_000,
            base_borrow_index_nad: NAD as u128,
            fixed_base_principal: 1_000,
            ..Debt::default()
        };
        let interest = debt.realize_margin_repay(MarketAsset::Base, 400).unwrap();
        assert_eq!(interest, 0);
        assert_eq!(debt.fixed_base_principal, 600);
    }

    #[test]
    fn realize_margin_repay_splits_accrued_interest() {
        // Index 1.1: 1_000 of principal now owes 1_100 of debt.
        let mut debt = Debt {
            fixed_base_shares: 1_000,
            base_borrow_index_nad: (NAD as u128) * 11 / 10,
            fixed_base_principal: 1_000,
            ..Debt::default()
        };
        // Repay 550 of 1_100: 500 principal + 50 interest.
        let interest = debt.realize_margin_repay(MarketAsset::Base, 550).unwrap();
        assert_eq!(interest, 50);
        assert_eq!(debt.fixed_base_principal, 500);
    }

    #[test]
    fn realize_margin_repay_full_clears_principal_and_returns_all_interest() {
        let mut debt = Debt {
            fixed_quote_shares: 1_000,
            quote_borrow_index_nad: (NAD as u128) * 11 / 10,
            fixed_quote_principal: 1_000,
            ..Debt::default()
        };
        let interest = debt
            .realize_margin_repay(MarketAsset::Quote, 1_100)
            .unwrap();
        assert_eq!(interest, 100);
        assert_eq!(debt.fixed_quote_principal, 0);
    }

    #[test]
    fn liquidation_writeoff_reduces_principal_without_realizing_interest_as_cash() {
        let mut debt = Debt {
            fixed_base_shares: 1_000,
            base_borrow_index_nad: (NAD as u128) * 11 / 10,
            fixed_base_principal: 1_000,
            ..Debt::default()
        };

        let interest = debt
            .realize_margin_liquidation(MarketAsset::Base, 550, 1_100)
            .unwrap();

        assert_eq!(interest, 50);
        assert_eq!(debt.fixed_base_principal, 0);
    }

    #[test]
    fn isolated_debt_uses_separate_shares_and_principal() {
        let mut debt = Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            ..Debt::default()
        };

        let shares = debt.add_isolated_debt(MarketAsset::Base, 1_000).unwrap();

        assert_eq!(shares, 1_000);
        assert_eq!(debt.isolated_base_shares, 1_000);
        assert_eq!(debt.isolated_base_principal, 1_000);
        assert_eq!(debt.fixed_base_shares, 0);
        assert_eq!(debt.isolated_debt(MarketAsset::Base).unwrap(), 1_000);
    }

    #[test]
    fn isolated_repay_splits_interest_without_touching_margin_principal() {
        let mut debt = Debt {
            base_borrow_index_nad: (NAD as u128) * 11 / 10,
            isolated_base_shares: 1_000,
            isolated_base_principal: 1_000,
            fixed_base_principal: 777,
            ..Debt::default()
        };
        let mut position_shares = 1_000;
        let mut position_principal = 1_000;

        let clearance = debt
            .clear_isolated_debt(
                MarketAsset::Base,
                &mut position_shares,
                &mut position_principal,
                550,
            )
            .unwrap();

        assert_eq!(clearance.principal_paid, 500);
        assert_eq!(clearance.interest_paid, 50);
        assert_eq!(debt.isolated_base_principal, 500);
        assert_eq!(position_principal, 500);
        assert_eq!(debt.fixed_base_principal, 777);
    }

    #[test]
    fn isolated_repay_reports_actual_debt_reduced_after_rounded_share_burn() {
        let mut debt = Debt {
            base_borrow_index_nad: (NAD as u128) * 3 / 2,
            isolated_base_shares: 100,
            isolated_base_principal: 100,
            ..Debt::default()
        };
        let mut position_shares = 100;
        let mut position_principal = 100;

        let clearance = debt
            .clear_isolated_debt(
                MarketAsset::Base,
                &mut position_shares,
                &mut position_principal,
                2,
            )
            .unwrap();

        assert_eq!(clearance.shares_burned, 2);
        assert_eq!(clearance.debt_reduced, 3);
        assert_eq!(clearance.aggregate_debt_reduced, 3);
        assert_eq!(clearance.remaining_debt, 147);
        assert_eq!(clearance.principal_paid, 1);
        assert_eq!(clearance.interest_paid, 1);
        assert_eq!(clearance.live_debit_for_cash_repay().unwrap(), 2);
        assert_eq!(position_shares, 98);
        assert_eq!(position_principal, 98);
        assert_eq!(debt.isolated_base_shares, 98);
        assert_eq!(debt.isolated_base_principal, 98);
    }

    #[test]
    fn isolated_repay_uses_aggregate_debt_delta_across_positions() {
        let mut debt = Debt {
            base_borrow_index_nad: (NAD as u128) * 3 / 2,
            isolated_base_shares: 2,
            isolated_base_principal: 2,
            ..Debt::default()
        };
        let mut position_shares = 1;
        let mut position_principal = 1;

        let clearance = debt
            .clear_isolated_debt(
                MarketAsset::Base,
                &mut position_shares,
                &mut position_principal,
                1,
            )
            .unwrap();

        assert_eq!(clearance.debt_reduced, 1);
        assert_eq!(clearance.aggregate_debt_reduced, 2);
        assert_eq!(clearance.live_debit_for_cash_repay().unwrap(), 1);
        assert_eq!(debt.isolated_base_shares, 1);
    }

    #[test]
    fn isolated_writeoff_reports_aggregate_debt_delta_across_positions() {
        let mut debt = Debt {
            base_borrow_index_nad: (NAD as u128) * 3 / 2,
            isolated_base_shares: 2,
            isolated_base_principal: 2,
            ..Debt::default()
        };
        let mut position_shares = 1;
        let mut position_principal = 1;

        let writeoff = debt
            .writeoff_isolated_position(
                MarketAsset::Base,
                &mut position_shares,
                &mut position_principal,
            )
            .unwrap();

        assert_eq!(writeoff.debt_written_off, 1);
        assert_eq!(writeoff.aggregate_debt_written_off, 2);
        assert_eq!(debt.isolated_base_shares, 1);
    }
