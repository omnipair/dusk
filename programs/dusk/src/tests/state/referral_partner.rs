use super::*;

fn partner(interest_share_bps: u16, active: bool) -> ReferralPartner {
    ReferralPartner {
        authority: Pubkey::new_unique(),
        recipient: Pubkey::new_unique(),
        interest_share_bps,
        active,
        bump: 1,
    }
}

#[test]
fn referral_share_is_taken_only_from_protocol_interest_revenue() {
    let quote = ReferralInterestQuote::new(100_000, 99_000, 2_000, Some(2_500)).unwrap();
    assert_eq!(quote.protocol_interest_revenue, 19_800);
    assert_eq!(quote.referral_amount, 4_950);
}

#[test]
fn runtime_cap_is_snapshotted_when_referral_is_bound() {
    let partner = partner(7_500, true);
    let bound_share_bps = partner.binding_interest_share_bps(4_000).unwrap();
    let quote = ReferralInterestQuote::new(100_000, 100_000, 2_000, Some(bound_share_bps)).unwrap();
    assert_eq!(quote.interest_share_bps, 4_000);
    assert_eq!(quote.referral_amount, 8_000);
}

#[test]
fn inactive_partner_cannot_be_bound() {
    let partner = partner(5_000, false);
    assert_eq!(
        partner.binding_interest_share_bps(10_000).unwrap_err(),
        error!(ErrorCode::ReferralPartnerNotActive)
    );
}

#[test]
fn bound_share_survives_partner_deactivation_and_rate_changes() {
    let mut partner = partner(5_000, true);
    let bound_share_bps = partner.binding_interest_share_bps(10_000).unwrap();
    partner.configure(partner.authority, 1_000, false).unwrap();

    let quote = ReferralInterestQuote::new(100_000, 100_000, 2_000, Some(bound_share_bps)).unwrap();
    assert_eq!(quote.interest_share_bps, 5_000);
    assert_eq!(quote.referral_amount, 10_000);
}

#[test]
fn tiny_payments_round_down() {
    let quote = ReferralInterestQuote::new(1, 1, 2_000, Some(5_000)).unwrap();
    assert_eq!(quote.protocol_interest_revenue, 0);
    assert_eq!(quote.referral_amount, 0);
}
