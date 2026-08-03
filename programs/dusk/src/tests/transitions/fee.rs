use super::*;

    #[test]
    fn swap_protocol_fee_splits_between_auction_lanes_at_accrual() {
        let mut side = MarketSide::default();
        let receipt = side
            .record_swap_fee_credit(
            10_000,
            1_000,
            2_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 7_500,
                buyback_auction_bps: 2_500,
            },
        )
        .unwrap();

        assert_eq!(receipt.manager_swap_fee_liability, 1_000);
        assert_eq!(receipt.manager_interest_fee_liability, 0);
        assert_eq!(receipt.protocol_fee_liability, 1_500);
        assert_eq!(receipt.buyback_fee_liability, 500);
        assert_eq!(receipt.unallocated_swap_fee_liability, 7_000);
        assert_eq!(receipt.swap_fee_vault_balance, 10_000);
        side.fees.assert_backed().unwrap();
    }

    #[test]
    fn distributed_dynamic_surcharge_belongs_entirely_to_ylps() {
        let mut side = MarketSide::default();
        let receipt = side
            .record_claimable_swap_fees(
                10_000,
                3_000,
                1_000,
                2_000,
                ProtocolAuctionSplit {
                    fee_auction_bps: 7_500,
                    buyback_auction_bps: 2_500,
                },
                0,
            )
            .unwrap();

        assert_eq!(receipt.manager_swap_fee_liability, 1_000);
        assert_eq!(receipt.protocol_fee_liability, 1_500);
        assert_eq!(receipt.buyback_fee_liability, 500);
        assert_eq!(receipt.unallocated_swap_fee_liability, 10_000);
        assert_eq!(receipt.swap_fee_vault_balance, 13_000);
        side.fees.assert_backed().unwrap();
    }

    #[test]
    fn surcharge_only_credit_has_no_manager_or_protocol_leakage() {
        let mut side = MarketSide::default();
        let receipt = side
            .record_claimable_swap_fees(
                0,
                3_000,
                1_000,
                2_000,
                ProtocolAuctionSplit {
                    fee_auction_bps: 7_500,
                    buyback_auction_bps: 2_500,
                },
                0,
            )
            .unwrap();

        assert_eq!(receipt.manager_swap_fee_liability, 0);
        assert_eq!(receipt.protocol_fee_liability, 0);
        assert_eq!(receipt.buyback_fee_liability, 0);
        assert_eq!(receipt.unallocated_swap_fee_liability, 3_000);
        assert_eq!(receipt.swap_fee_vault_balance, 3_000);
        side.fees.assert_backed().unwrap();
    }

    #[test]
    fn distributed_surcharge_uses_the_current_swap_eligible_supply() {
        let mut side = MarketSide::default();
        let receipt = side
            .record_claimable_swap_fees(
                1_000,
                500,
                1_000,
                2_000,
                ProtocolAuctionSplit {
                    fee_auction_bps: 10_000,
                    buyback_auction_bps: 0,
                },
                1_000,
            )
            .unwrap();

        // Base LP fee is 700; all 500 surcharge units also go to yLPs.
        assert_eq!(
            receipt.swap_fee_growth_index_nad,
            1_200u128 * NAD as u128 / 1_000
        );
        assert_eq!(receipt.swap_fee_liability, 1_200);
        assert_eq!(receipt.unallocated_swap_fee_liability, 0);
        assert_eq!(receipt.manager_swap_fee_liability, 100);
        assert_eq!(receipt.protocol_fee_liability, 200);
        assert_eq!(receipt.swap_fee_vault_balance, 1_500);
        side.fees.assert_backed().unwrap();
    }

    #[test]
    fn interest_protocol_fee_splits_between_auction_lanes_at_accrual() {
        let mut side = MarketSide::default();
        let receipt = side
            .record_interest_credit(
            10_000,
            500,
            1_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 4_000,
                buyback_auction_bps: 6_000,
            },
            250,
        )
        .unwrap();

        assert_eq!(receipt.manager_swap_fee_liability, 0);
        assert_eq!(receipt.manager_interest_fee_liability, 500);
        assert_eq!(receipt.referral_interest_liability, 250);
        assert_eq!(receipt.protocol_fee_liability, 300);
        assert_eq!(receipt.buyback_fee_liability, 450);
        assert_eq!(receipt.unallocated_interest_liability, 8_500);
        assert_eq!(receipt.interest_vault_balance, 10_000);
        side.fees.assert_backed().unwrap();
    }

    #[test]
    fn invalid_auction_split_is_rejected_before_liabilities_move() {
        let mut side = MarketSide::default();
        let err = side
            .record_swap_fee_credit(
            10_000,
            0,
            1_000,
            ProtocolAuctionSplit {
                fee_auction_bps: 7_000,
                buyback_auction_bps: 4_000,
            },
        )
        .unwrap_err();

        assert_eq!(err, error!(ErrorCode::InvalidDistribution));
        assert_eq!(side.fees.swap_fee_vault_balance, 0);
        assert_eq!(side.fees.protocol_fee_liability, 0);
        assert_eq!(side.fees.buyback_fee_liability, 0);
    }
