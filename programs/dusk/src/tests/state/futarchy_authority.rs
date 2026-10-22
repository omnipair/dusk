use super::*;

    #[test]
    fn protocol_share_defaults_preserve_explicit_overrides_and_validate_bounds() {
        let initialize = |swap_bps, interest_bps| {
            FutarchyAuthority::initialize(
                Pubkey::new_unique(),
                swap_bps,
                interest_bps,
                0,
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                BPS_DENOMINATOR,
                0,
                0,
                1,
            )
        };
        for (swap, interest, expected_swap, expected_interest) in [
            (None, None, 2_000, 2_000),
            (Some(0), None, 0, 2_000),
            (None, Some(0), 2_000, 0),
            (Some(125), Some(250), 125, 250),
            (Some(BPS_DENOMINATOR), Some(BPS_DENOMINATOR), BPS_DENOMINATOR, BPS_DENOMINATOR),
        ] {
            let authority = initialize(swap, interest).unwrap();
            assert_eq!(authority.revenue_share.swap_bps, expected_swap);
            assert_eq!(authority.revenue_share.interest_bps, expected_interest);
        }
        assert_eq!(
            initialize(Some(BPS_DENOMINATOR + 1), None).unwrap_err(),
            error!(ErrorCode::InvalidSwapFeeBps)
        );
        assert_eq!(
            initialize(None, Some(BPS_DENOMINATOR + 1)).unwrap_err(),
            error!(ErrorCode::InvalidInterestFeeBps)
        );
    }

    #[test]
    fn default_auction_split_routes_all_protocol_revenue_to_fee_lane() {
        let split = ProtocolAuctionSplit::default();

        assert_eq!(split.fee_auction_bps, BPS_DENOMINATOR);
        assert_eq!(split.buyback_auction_bps, 0);
        assert!(split.is_valid());
    }

    #[test]
    fn auction_params_reject_invalid_curve_shapes() {
        let mut params = ProtocolAuctionParams::default_epoch();
        params.validate().unwrap();

        params.floor_multiplier_bps = params.start_multiplier_bps + 1;
        assert_eq!(
            params.validate().unwrap_err(),
            error!(ErrorCode::InvalidAuctionConfig)
        );

        params = ProtocolAuctionParams::default_epoch();
        params.duration_ms = 0;
        assert_eq!(
            params.validate().unwrap_err(),
            error!(ErrorCode::InvalidAuctionConfig)
        );
    }

    #[test]
    fn initialized_authority_uses_treasury_only_auction_recipients() {
        let authority = Pubkey::new_unique();
        let treasury = Pubkey::new_unique();
        let buybacks_vault = Pubkey::new_unique();
        let team_treasury = Pubkey::new_unique();
        let staking_vault = Pubkey::new_unique();
        let fee_accepted_mint = Pubkey::new_unique();
        let buyback_accepted_mint = Pubkey::new_unique();

        let mut futarchy = FutarchyAuthority::initialize(
            authority,
            None,
            None,
            2_500,
            treasury,
            buybacks_vault,
            team_treasury,
            staking_vault,
            fee_accepted_mint,
            buyback_accepted_mint,
            BPS_DENOMINATOR,
            0,
            0,
            42,
        )
        .unwrap();

        assert_eq!(futarchy.revenue_share.swap_bps, DEFAULT_PROTOCOL_REVENUE_SHARE_BPS);
        assert_eq!(futarchy.revenue_share.interest_bps, DEFAULT_PROTOCOL_REVENUE_SHARE_BPS);
        assert_eq!(futarchy.fee_auction.accepted_mint, fee_accepted_mint);
        assert_eq!(futarchy.version, FutarchyAuthority::CURRENT_VERSION);
        assert_eq!(futarchy.max_referral_interest_share_bps, 2_500);
        assert_eq!(
            futarchy.buyback_auction.accepted_mint,
            buyback_accepted_mint
        );
        assert_eq!(futarchy.fee_auction.recipients.treasury, treasury);
        assert_eq!(futarchy.fee_auction.recipients.staking_vault, staking_vault);
        assert_eq!(
            futarchy.fee_auction.recipients.treasury_bps,
            BPS_DENOMINATOR
        );
        assert_eq!(futarchy.fee_auction.recipients.staking_vault_bps, 0);
        futarchy.validate().unwrap();

        futarchy.max_referral_interest_share_bps = 0;
        futarchy.validate().unwrap();
        futarchy.max_referral_interest_share_bps = BPS_DENOMINATOR;
        futarchy.validate().unwrap();
        futarchy.max_referral_interest_share_bps = BPS_DENOMINATOR + 1;
        assert_eq!(
            futarchy.validate().unwrap_err(),
            error!(ErrorCode::InvalidReferralInterestShareBps)
        );
    }
