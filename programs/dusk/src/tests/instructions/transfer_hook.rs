use super::*;
use crate::constants::{MARKET_LAYOUT_VERSION, YIELD_GROWTH_SCALE_Q64};
use crate::instructions::liquidity::canonical_lp_transfer_hook_metas;

fn pre_transfer_balances(
    source_post_balance: u64,
    destination_post_balance: u64,
    amount: u64,
) -> Result<TransferBalances> {
    let source_pre_balance = source_post_balance
        .checked_add(amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let destination_pre_balance = destination_post_balance
        .checked_sub(amount)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(TransferBalances {
        source_pre_balance,
        destination_pre_balance,
    })
}

fn checkpoint_yield_account_state(
    yield_account: &mut YieldAccount,
    yield_context: YieldContext,
    pre_transfer_balance: u64,
) -> Result<()> {
    yield_account.accrue(
        pre_transfer_balance,
        yield_context.swap_fee_growth_index_q64,
        yield_context.interest_growth_index_q64,
    )
}

#[test]
fn reconstructs_pre_transfer_balances_from_post_transfer_state() {
    let balances = pre_transfer_balances(700, 350, 50).unwrap();
    assert_eq!(
        balances,
        TransferBalances {
            source_pre_balance: 750,
            destination_pre_balance: 300
        }
    );
}

#[test]
fn accepts_only_the_owner_token_2022_lp_ata() {
    let owner = Pubkey::new_unique();
    let lp_mint = Pubkey::new_unique();
    let canonical =
        anchor_spl::associated_token::get_associated_token_address_with_program_id(&owner, &lp_mint, &Token2022::id());

    validate_canonical_lp_token_account_key(canonical, owner, lp_mint).unwrap();
    assert_eq!(
        validate_canonical_lp_token_account_key(Pubkey::new_unique(), owner, lp_mint).unwrap_err(),
        error!(ErrorCode::InvalidTokenAccount)
    );
}

#[test]
fn rejects_post_transfer_destination_underflow() {
    let result = pre_transfer_balances(700, 49, 50);
    assert!(result.is_err());
}

#[test]
fn checkpoints_yield_account_with_pre_transfer_balance() {
    let owner = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let lp_mint = Pubkey::new_unique();
    let asset_mint = Pubkey::new_unique();
    let mut yield_account = YieldAccount {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        lp_mint: Pubkey::default(),
        asset_mint: Pubkey::default(),
        token_kind: 0,
        recipient: Pubkey::default(),
        swap_fee_checkpoint_q64: 0,
        interest_checkpoint_q64: 0,
        accrued_swap_fee_amount: 0,
        accrued_interest_amount: 0,
        swap_fee_remainder_q64: 0,
        interest_remainder_q64: 0,
        bump: 0,
    };
    yield_account.initialize(owner, market, lp_mint, asset_mint, YieldTokenKind::Ylp, owner, 255);
    let yield_context = YieldContext {
        lp_mint,
        asset_mint,
        token_kind: YieldTokenKind::Ylp,
        swap_fee_growth_index_q64: 3 * YIELD_GROWTH_SCALE_Q64,
        interest_growth_index_q64: 2 * YIELD_GROWTH_SCALE_Q64,
    };

    checkpoint_yield_account_state(&mut yield_account, yield_context, 10).unwrap();

    assert_eq!(yield_account.accrued_swap_fee_amount, 30);
    assert_eq!(yield_account.accrued_interest_amount, 20);
    assert_eq!(
        yield_account.swap_fee_checkpoint_q64,
        yield_context.swap_fee_growth_index_q64
    );
    assert_eq!(
        yield_account.interest_checkpoint_q64,
        yield_context.interest_growth_index_q64
    );
}

#[test]
fn virtual_hlp_context_checkpoints_pending_yield_before_transfer() {
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let ylp_mint = Pubkey::new_unique();
    let base_hlp_mint = Pubkey::new_unique();
    let quote_hlp_mint = Pubkey::new_unique();
    let market_key = Pubkey::new_unique();
    let mut market = Market {
        version: MARKET_LAYOUT_VERSION,
        ylp_mint,
        base_side: crate::state::MarketSide {
            asset_mint: base_mint,
            hlp_mint: base_hlp_mint,
            ..Default::default()
        },
        quote_side: crate::state::MarketSide {
            asset_mint: quote_mint,
            hlp_mint: quote_hlp_mint,
            ..Default::default()
        },
        config: Default::default(),
        amm: Default::default(),
        debt: Default::default(),
        base_hlp_vault: crate::state::HlpVault {
            base_swap_fee_growth_index_q64: 10,
            base_interest_growth_index_q64: 11,
            quote_swap_fee_growth_index_q64: 20,
            quote_interest_growth_index_q64: 21,
            ..Default::default()
        },
        quote_hlp_vault: crate::state::HlpVault {
            base_swap_fee_growth_index_q64: 30,
            base_interest_growth_index_q64: 31,
            quote_swap_fee_growth_index_q64: 40,
            quote_interest_growth_index_q64: 41,
            ..Default::default()
        },
        risk: Default::default(),
        insurance: Default::default(),
        params_hash: [0; 32],
        initial_liquidity_authority: Pubkey::default(),
        governance_locked_ylp: 0,
        parameter_revisions: [0; 7],
        last_marginal_observation_nad: 0,
        curve_revision: 0,
        risk_revision: 0,
        last_update_slot: 0,
        reduce_only: false,
        bump: 0,
    };
    market.base_side.shares.ylp_supply = 100;
    market.base_side.fees.unallocated_interest_liability = 100;
    market.base_hlp_vault.ylp_shares = 100;
    market.base_hlp_vault.hlp_supply = 100;

    let base_contexts = current_yield_contexts(&mut market, base_hlp_mint).unwrap().unwrap();
    assert_eq!(
        base_contexts.items,
        [
            Some(YieldContext {
                lp_mint: base_hlp_mint,
                asset_mint: base_mint,
                token_kind: YieldTokenKind::Hlp,
                swap_fee_growth_index_q64: 10,
                interest_growth_index_q64: YIELD_GROWTH_SCALE_Q64 + 11,
            }),
            Some(YieldContext {
                lp_mint: base_hlp_mint,
                asset_mint: quote_mint,
                token_kind: YieldTokenKind::Hlp,
                swap_fee_growth_index_q64: 20,
                interest_growth_index_q64: 21,
            }),
        ]
    );

    let quote_contexts = current_yield_contexts(&mut market, quote_hlp_mint).unwrap().unwrap();
    assert_eq!(
        quote_contexts.items,
        [
            Some(YieldContext {
                lp_mint: quote_hlp_mint,
                asset_mint: base_mint,
                token_kind: YieldTokenKind::Hlp,
                swap_fee_growth_index_q64: 30,
                interest_growth_index_q64: 31,
            }),
            Some(YieldContext {
                lp_mint: quote_hlp_mint,
                asset_mint: quote_mint,
                token_kind: YieldTokenKind::Hlp,
                swap_fee_growth_index_q64: 40,
                interest_growth_index_q64: 41,
            }),
        ]
    );

    let owner = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let mut source_yield = YieldAccount {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        lp_mint: Pubkey::default(),
        asset_mint: Pubkey::default(),
        token_kind: 0,
        recipient: Pubkey::default(),
        swap_fee_checkpoint_q64: 0,
        interest_checkpoint_q64: 0,
        accrued_swap_fee_amount: 0,
        accrued_interest_amount: 0,
        swap_fee_remainder_q64: 0,
        interest_remainder_q64: 0,
        bump: 0,
    };
    let mut destination_yield = YieldAccount {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        lp_mint: Pubkey::default(),
        asset_mint: Pubkey::default(),
        token_kind: 0,
        recipient: Pubkey::default(),
        swap_fee_checkpoint_q64: 0,
        interest_checkpoint_q64: 0,
        accrued_swap_fee_amount: 0,
        accrued_interest_amount: 0,
        swap_fee_remainder_q64: 0,
        interest_remainder_q64: 0,
        bump: 0,
    };
    source_yield.initialize(
        owner,
        market_key,
        base_hlp_mint,
        base_mint,
        YieldTokenKind::Hlp,
        owner,
        1,
    );
    destination_yield.initialize(
        recipient,
        market_key,
        base_hlp_mint,
        base_mint,
        YieldTokenKind::Hlp,
        recipient,
        2,
    );
    let context = base_contexts.items[0].unwrap();

    // Hook execution uses pre-transfer balances: all pending yield remains
    // with the source, and the zero-balance destination starts at the same
    // virtual checkpoint.
    checkpoint_yield_account_state(&mut source_yield, context, 100).unwrap();
    checkpoint_yield_account_state(&mut destination_yield, context, 0).unwrap();
    checkpoint_yield_account_state(&mut destination_yield, context, 100).unwrap();

    assert_eq!(source_yield.accrued_interest_amount, 100);
    assert_eq!(destination_yield.accrued_interest_amount, 0);
}

#[test]
fn lp_mint_separates_canonical_yield_account_pdas() {
    let program_id = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let asset_mint = Pubkey::new_unique();
    let base_hlp_mint = Pubkey::new_unique();
    let quote_hlp_mint = Pubkey::new_unique();
    let base_key = Pubkey::find_program_address(
        &[
            YIELD_ACCOUNT_SEED_PREFIX,
            market.as_ref(),
            owner.as_ref(),
            base_hlp_mint.as_ref(),
            asset_mint.as_ref(),
            &[YieldTokenKind::Hlp.code()],
        ],
        &program_id,
    )
    .0;
    let quote_key = Pubkey::find_program_address(
        &[
            YIELD_ACCOUNT_SEED_PREFIX,
            market.as_ref(),
            owner.as_ref(),
            quote_hlp_mint.as_ref(),
            asset_mint.as_ref(),
            &[YieldTokenKind::Hlp.code()],
        ],
        &program_id,
    )
    .0;
    assert_ne!(base_key, quote_key);
}

#[test]
fn canonical_extra_meta_schema_contains_both_asset_streams() {
    let market = Pubkey::new_unique();
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let lp_mint = Pubkey::new_unique();
    let state = Market {
        ylp_mint: lp_mint,
        base_side: crate::state::MarketSide {
            asset_mint: base_mint,
            ..Default::default()
        },
        quote_side: crate::state::MarketSide {
            asset_mint: quote_mint,
            ..Default::default()
        },
        ..Market::default()
    };
    let metas = canonical_lp_transfer_hook_metas(market, &state, lp_mint).unwrap();
    assert_eq!(metas.len(), 7);
    assert_eq!(metas[0].address_config, market.to_bytes());
    assert!(bool::from(metas[0].is_writable));
    assert_eq!(metas[1].address_config, base_mint.to_bytes());
    assert_eq!(metas[2].address_config, quote_mint.to_bytes());
    for meta in &metas[3..] {
        assert_eq!(meta.discriminator, 1);
        assert!(bool::from(meta.is_writable));
        assert!(!bool::from(meta.is_signer));
    }
}

#[test]
fn transfer_checkpoint_rejects_readonly_yield_accounts_before_deserializing() {
    let program_id = Pubkey::new_unique();
    let base_key = Pubkey::new_unique();
    let quote_key = Pubkey::new_unique();
    let mut base_lamports = 0;
    let mut quote_lamports = 0;
    let mut base_data = [];
    let mut quote_data = [];
    let base_account = AccountInfo::new(
        &base_key,
        false,
        false,
        &mut base_lamports,
        &mut base_data,
        &program_id,
        false,
        0,
    );
    let quote_account = AccountInfo::new(
        &quote_key,
        false,
        true,
        &mut quote_lamports,
        &mut quote_data,
        &program_id,
        false,
        0,
    );
    let context = YieldContext {
        lp_mint: Pubkey::new_unique(),
        asset_mint: Pubkey::new_unique(),
        token_kind: YieldTokenKind::Ylp,
        swap_fee_growth_index_q64: 0,
        interest_growth_index_q64: 0,
    };

    assert_eq!(
        checkpoint_transfer_party(
            &base_account,
            &quote_account,
            &program_id,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            context,
            context,
            0,
        )
        .unwrap_err(),
        error!(ErrorCode::InvalidYieldAccount)
    );
}
