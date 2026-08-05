use super::*;
use spl_token_2022::extension::transfer_fee::TransferFee;

fn active_transfer_fee() -> TransferFee {
    TransferFee {
        epoch: 0_u64.into(),
        maximum_fee: u64::MAX.into(),
        transfer_fee_basis_points: 300_u16.into(),
    }
}

#[test]
fn token_2022_fee_reconciles_quote_insurance_to_actual_credit() {
    let gross_funding = 10_000;
    let actual_credit = active_transfer_fee().calculate_post_fee_amount(gross_funding).unwrap();
    let mut insurance = Insurance {
        // State after liquidation math added 10_000 gross to a prior 40_000.
        quote_available: 50_000,
        ..Insurance::default()
    };

    reconcile_insurance_funding_credit(&mut insurance, MarketAsset::Base, gross_funding, actual_credit).unwrap();

    assert_eq!(actual_credit, 9_700);
    assert_eq!(insurance.quote_available, 49_700);
}

#[test]
fn token_2022_fee_reconciles_base_insurance_to_actual_credit() {
    let gross_funding = 10_000;
    let actual_credit = active_transfer_fee().calculate_post_fee_amount(gross_funding).unwrap();
    let mut insurance = Insurance {
        // State after liquidation math added 10_000 gross to a prior 40_000.
        base_available: 50_000,
        ..Insurance::default()
    };

    reconcile_insurance_funding_credit(&mut insurance, MarketAsset::Quote, gross_funding, actual_credit).unwrap();

    assert_eq!(actual_credit, 9_700);
    assert_eq!(insurance.base_available, 49_700);
}

#[test]
fn insurance_credit_cannot_exceed_gross_funding() {
    let mut insurance = Insurance {
        quote_available: 500,
        ..Insurance::default()
    };

    assert!(reconcile_insurance_funding_credit(&mut insurance, MarketAsset::Base, 100, 101).is_err());
}
