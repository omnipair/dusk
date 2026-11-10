use super::*;
use proptest::prelude::*;

/// A pair with real mint decimals: whole quote tokens per whole base token.
#[derive(Clone, Copy, Debug)]
struct ExitPair {
    name: &'static str,
    base_decimals: u8,
    quote_decimals: u8,
    price: f64,
}

/// Coarse atoms are where a skipped debt atom is worth the most: TICKET is a
/// zero-decimal token at $10, so one atom of its debt is ten dollars.
const EXIT_PAIRS: [ExitPair; 5] = [
    ExitPair { name: "BTC/USDC", base_decimals: 8, quote_decimals: 6, price: 100_000.0 },
    ExitPair { name: "SOL/USDC", base_decimals: 9, quote_decimals: 6, price: 150.0 },
    ExitPair { name: "SOL/BONK", base_decimals: 9, quote_decimals: 5, price: 7_500_000.0 },
    ExitPair { name: "TICKET/USDC", base_decimals: 0, quote_decimals: 6, price: 10.0 },
    ExitPair { name: "EUR2/USDC", base_decimals: 2, quote_decimals: 6, price: 1.1 },
];

/// Seeds `quote_tokens` of quote and its value in base through
/// `add_liquidity`, then opens both hLP vaults with a twentieth of each
/// reserve so later entries join existing holders.
fn hlp_market(pair: ExitPair, quote_tokens: f64, concentrated: bool) -> Option<Market> {
    let mut market = Market { version: crate::constants::MARKET_LAYOUT_VERSION, ..Market::default() };
    market.base_side.asset_mint = Pubkey::new_unique();
    market.quote_side.asset_mint = Pubkey::new_unique();
    market.base_side.asset_decimals = pair.base_decimals;
    market.quote_side.asset_decimals = pair.quote_decimals;
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.config.swap_fee_bps = 30;
    market.config.target_hlp_leverage_bps = 2 * BPS_DENOMINATOR;
    market.config.settlement_divergence_bps = BPS_DENOMINATOR;
    if concentrated {
        market.config.amm.peak_amplification_nad = 20 * NAD;
        market.config.amm.core_half_width_bps = 250;
        market.config.amm.fade_width_bps = 500;
        market.config.amm.adjustment_step_nad = NAD / 1_000;
    }
    let base = quote_tokens / pair.price * 10_f64.powi(i32::from(pair.base_decimals));
    let quote = quote_tokens * 10_f64.powi(i32::from(pair.quote_decimals));
    if !(1.0..u64::MAX as f64).contains(&base) || !(1.0..u64::MAX as f64).contains(&quote) {
        return None;
    }
    market.add_liquidity(base as u64, quote as u64).ok()?;
    market.prepare_amm_for_swap(1).ok()?;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let amount = market.side(asset).reserves.cash_reserve / 20;
        market.deposit_single_sided(asset, amount, 1).ok()?;
    }
    Some(market)
}

/// Enters an hLP vault and immediately redeems every minted token. New
/// shares are priced so the entry's own rounding surplus stays with existing
/// holders, and exiting holders take floored asset claims and at least their
/// share of the vault's debt, so the round trip cannot return more than the
/// deposit. A dust position whose debt share rounds up to an atom it cannot
/// cover fails closed instead of leaving that debt with other holders; it can
/// exit once topped up.
#[test]
fn hlp_entry_then_exit_returns_at_most_the_deposit() {
    let (mut trips, mut gains, mut errors) = (0, vec![], vec![]);
    for pair in EXIT_PAIRS {
        for concentrated in [false, true] {
            for quote_tokens in [5_000.0, 50_000_000.0] {
                let Some(market) = hlp_market(pair, quote_tokens, concentrated) else {
                    continue;
                };
                for target in [MarketAsset::Base, MarketAsset::Quote] {
                    let reserve = market.side(target).reserves.cash_reserve;
                    for (deposit, dust) in [(1, true), (7, true), (reserve / 10_000, true), (reserve / 100, false), (reserve / 20, false)] {
                        if deposit == 0 {
                            continue;
                        }
                        let context = format!("{} {quote_tokens} concentrated={concentrated} {target:?} {deposit}", pair.name);
                        let mut entered = market.clone();
                        let Ok(entry) = entered.deposit_single_sided(target, deposit, 1) else {
                            continue;
                        };
                        match entered.withdraw_single_sided(target, entry.hlp_amount) {
                            Ok(exit) => {
                                entered.assert_market_invariants().unwrap();
                                trips += 1;
                                if exit.target_amount_out > deposit {
                                    gains.push(format!("{context}: back {}", exit.target_amount_out));
                                }
                            }
                            Err(anchor_lang::error::Error::AnchorError(error))
                                if dust && error.error_name == "HlpSettlementUnavailable" => {}
                            Err(error) => errors.push(format!("{context}: {error:?}")),
                        }
                    }
                }
            }
        }
    }
    assert!(trips > 100, "{trips}");
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(gains.is_empty(), "{gains:#?}");
}

proptest! {
    /// The exit burns the fewest shares, starting from the floored
    /// proportional count, whose debt reduction covers the exiting fraction
    /// of the vault's indexed debt, rounded up and capped at the floored total.
    #[test]
    fn hlp_exit_burns_at_least_its_share_of_vault_debt(
        debt_shares in 1_u128..=1_000_000_000_000,
        supply in 1_u64..=1_000_000_000_000,
        exiting in any::<u64>(),
        index_growth_nad in 0_u128..=200 * NAD as u128,
    ) {
        let hlp_amount = 1 + exiting % supply;
        let index = NAD as u128 + index_growth_nad;
        let burned = hlp_exit_debt_shares(debt_shares, hlp_amount, supply, index).unwrap();
        let debt = Debt::shares_to_debt(debt_shares, index).unwrap();
        let owed = mul_div_ceil_u128(debt_shares * index, hlp_amount as u128, supply as u128 * NAD as u128)
            .unwrap()
            .min(debt);
        let reduction = |shares: u128| debt - Debt::shares_to_debt(debt_shares - shares, index).unwrap();
        let floored = debt_shares * hlp_amount as u128 / supply as u128;
        prop_assert!(burned >= floored && burned <= debt_shares);
        prop_assert!(reduction(burned) >= owed);
        prop_assert!(burned == floored || reduction(burned - 1) < owed);
        if hlp_amount == supply {
            prop_assert_eq!(burned, debt_shares);
        }
    }
}
