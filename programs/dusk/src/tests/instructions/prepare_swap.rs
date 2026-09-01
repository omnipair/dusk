use super::*;
use anchor_lang::solana_program::sysvar::{
    self,
    instructions::{construct_instructions_data, store_current_index, BorrowedAccountMeta, BorrowedInstruction},
};

fn guarded_market() -> Market {
    let mut market = Market::default();
    market.config.start_time = 100;
    market.config.amm.launch_rate_limit_asset = crate::state::LAUNCH_RATE_LIMIT_ASSET_BASE;
    market.config.amm.launch_rate_limit_duration_seconds = 100;
    market
}

fn run_guard(
    market: &Market,
    market_key: Pubkey,
    instructions: &[(Pubkey, Pubkey, &'static [u8])],
    current_index: u16,
    unix_timestamp: i64,
) -> Result<()> {
    let borrowed: Vec<_> = instructions
        .iter()
        .map(|(program_id, account_key, data)| BorrowedInstruction {
            program_id,
            accounts: vec![BorrowedAccountMeta {
                pubkey: account_key,
                is_signer: false,
                is_writable: true,
            }],
            data,
        })
        .collect();
    let mut data = construct_instructions_data(&borrowed);
    store_current_index(&mut data, current_index);
    let mut lamports = 0;
    let key = INSTRUCTIONS_SYSVAR_ID;
    let owner = sysvar::id();
    let account = AccountInfo::new(&key, false, false, &mut lamports, &mut data, &owner, false, 0);
    enforce_launch_same_transaction_guard(market, market_key, MarketAsset::Quote, unix_timestamp, &account)
}

#[test]
fn launch_guard_allows_one_market_action_and_rejects_same_transaction_splits() {
    let market = guarded_market();
    let market_key = Pubkey::new_unique();
    let swap = crate::instruction::Swap::DISCRIMINATOR;
    assert!(run_guard(&market, market_key, &[(crate::ID, market_key, swap)], 0, 100).is_ok());
    assert!(run_guard(
        &market,
        market_key,
        &[(crate::ID, market_key, &[]), (crate::ID, market_key, swap)],
        1,
        100,
    )
    .is_ok());
    assert!(run_guard(
        &market,
        market_key,
        &[(crate::ID, market_key, swap), (crate::ID, market_key, swap)],
        0,
        100,
    )
    .is_err());
}

#[test]
fn launch_guard_rejects_aggregator_cpi_but_is_inert_after_expiry() {
    let market = guarded_market();
    let market_key = Pubkey::new_unique();
    let aggregator = Pubkey::new_unique();
    let swap = crate::instruction::Swap::DISCRIMINATOR;
    assert!(run_guard(&market, market_key, &[(aggregator, market_key, swap)], 0, 100).is_err());
    assert!(run_guard(
        &market,
        market_key,
        &[(crate::ID, market_key, swap), (crate::ID, market_key, swap)],
        0,
        200,
    )
    .is_ok());
}
