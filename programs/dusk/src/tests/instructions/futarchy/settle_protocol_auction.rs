use super::*;
    use crate::state::{ProtocolAuctionConfig, ProtocolAuctionParams, ProtocolAuctionRecipients};

    fn auction() -> ProtocolAuctionConfig {
        ProtocolAuctionConfig {
            accepted_mint: Pubkey::new_unique(),
            recipients: ProtocolAuctionRecipients::treasury_only(
                Pubkey::new_unique(),
                Pubkey::new_unique(),
            ),
            params: ProtocolAuctionParams {
                start_multiplier_bps: 12_000,
                floor_multiplier_bps: 8_000,
                duration_slots: 100,
                max_reference_age_slots: 10,
            },
        }
    }

    #[test]
    fn dutch_price_decays_linearly_to_floor() {
        let auction = auction();

        let start = decayed_auction_price_nad(&auction, 10, NAD, 10).unwrap();
        let halfway = decayed_auction_price_nad(&auction, 10, NAD, 60).unwrap();
        let floor = decayed_auction_price_nad(&auction, 10, NAD, 110).unwrap();
        let after_floor = decayed_auction_price_nad(&auction, 10, NAD, 210).unwrap();

        assert_eq!(start, 1_200_000_000);
        assert_eq!(halfway, 1_000_000_000);
        assert_eq!(floor, 800_000_000);
        assert_eq!(after_floor, 800_000_000);
    }

    #[test]
    fn auction_payment_amount_normalizes_decimals_and_rounds_up() {
        let exact = auction_payment_amount(1_000_000, 6, 2 * NAD, 6).unwrap();
        let rounded = auction_payment_amount(1, 0, NAD / 3, 6).unwrap();

        assert_eq!(exact, 2_000_000);
        assert_eq!(rounded, 333_334);
    }

    #[test]
    fn payment_split_sends_rounding_remainder_to_treasury() {
        let (treasury, staking_vault) = split_payment(101, 3_333).unwrap();
        let (treasury_only, staking_zero) = split_payment(101, 0).unwrap();

        assert_eq!(treasury, 68);
        assert_eq!(staking_vault, 33);
        assert_eq!(treasury_only, 101);
        assert_eq!(staking_zero, 0);
    }

    #[test]
    fn reference_snapshot_must_be_fresh() {
        assert_fresh_reference(100, 110, 10).unwrap();

        let missing = assert_fresh_reference(0, 110, 10).unwrap_err();
        let stale = assert_fresh_reference(99, 110, 10).unwrap_err();

        assert_eq!(missing, error!(ErrorCode::StaleAuctionReference));
        assert_eq!(stale, error!(ErrorCode::StaleAuctionReference));
    }

    #[test]
    fn default_route_allows_only_the_sold_markets_direct_pair() {
        let market_key = Pubkey::new_unique();
        let sold_mint = Pubkey::new_unique();
        let direct_accepted_mint = Pubkey::new_unique();
        let unrelated_accepted_mint = Pubkey::new_unique();
        let market = Market {
            base_side: crate::state::MarketSide {
                asset_mint: sold_mint,
                ..crate::state::MarketSide::default()
            },
            quote_side: crate::state::MarketSide {
                asset_mint: direct_accepted_mint,
                ..crate::state::MarketSide::default()
            },
            ..Market::default()
        };

        assert_eq!(
            expected_reference_market(
                &market,
                market_key,
                ProtocolAuctionLane::Fee,
                sold_mint,
                direct_accepted_mint,
            )
            .unwrap(),
            market_key
        );
        assert_eq!(
            expected_reference_market(
                &market,
                market_key,
                ProtocolAuctionLane::Fee,
                sold_mint,
                unrelated_accepted_mint,
            )
            .unwrap_err(),
            error!(ErrorCode::InvalidMarket)
        );
    }

    #[test]
    fn approved_route_is_lane_specific_and_overrides_direct_pair() {
        let market_key = Pubkey::new_unique();
        let sold_mint = Pubkey::new_unique();
        let accepted_mint = Pubkey::new_unique();
        let fee_reference = Pubkey::new_unique();
        let buyback_reference = Pubkey::new_unique();
        let mut base_side = crate::state::MarketSide {
            asset_mint: sold_mint,
            ..crate::state::MarketSide::default()
        };
        base_side
            .fees
            .set_protocol_auction_reference_market(ProtocolAuctionLane::Fee, fee_reference);
        base_side
            .fees
            .set_protocol_auction_reference_market(ProtocolAuctionLane::Buyback, buyback_reference);
        let market = Market {
            base_side,
            quote_side: crate::state::MarketSide {
                asset_mint: accepted_mint,
                ..crate::state::MarketSide::default()
            },
            ..Market::default()
        };

        assert_eq!(
            expected_reference_market(
                &market,
                market_key,
                ProtocolAuctionLane::Fee,
                sold_mint,
                accepted_mint,
            )
            .unwrap(),
            fee_reference
        );
        assert_eq!(
            expected_reference_market(
                &market,
                market_key,
                ProtocolAuctionLane::Buyback,
                sold_mint,
                accepted_mint,
            )
            .unwrap(),
            buyback_reference
        );
    }
