use super::*;

#[test]
fn event_authority_bump_matches_build_identity() {
    let (_, bump) = Pubkey::find_program_address(&[b"__event_authority"], &crate::ID);
    assert_eq!(bump, EVENT_AUTHORITY_BUMP);
}
