use super::*;
use anchor_lang::solana_program::program_option::COption;
use spl_token_2022::{
    extension::{
        transfer_fee::{TransferFee, TransferFeeConfig},
        BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
    },
    state::Mint as SplToken2022Mint,
};

#[test]
fn leverage_market_pda_runtime_guard_matches_canonical_market_seeds() {
    let mut market = Market::default();
    market.base_side.asset_mint = Pubkey::new_unique();
    market.quote_side.asset_mint = Pubkey::new_unique();
    market.params_hash = [7; 32];
    let (market_key, bump) = Pubkey::find_program_address(
        &[
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        &crate::ID,
    );
    market.bump = bump;

    validate_leverage_market_pda(&market, market_key).unwrap();
    assert!(validate_leverage_market_pda(&market, Pubkey::new_unique()).is_err());
}

#[test]
fn leverage_futarchy_pda_runtime_guard_matches_canonical_authority_seed() {
    let (futarchy_authority_key, bump) = Pubkey::find_program_address(&[FUTARCHY_AUTHORITY_SEED_PREFIX], &crate::ID);

    validate_leverage_futarchy_pda(bump, futarchy_authority_key).unwrap();
    assert!(validate_leverage_futarchy_pda(bump, Pubkey::new_unique()).is_err());
}

#[test]
fn leverage_public_paths_validate_runtime_control_pdas_before_mutation() {
    let open = include_str!("../../../instructions/leverage/open_leverage.rs");
    let close = include_str!("../../../instructions/leverage/close_leverage.rs");
    let decrease = include_str!("../../../instructions/leverage/decrease_leverage.rs");
    let liquidate = include_str!("../../../instructions/leverage/liquidate_leverage.rs");
    let lib = include_str!("../../../lib.rs");
    let boxed_runtime_guard = "validate_leverage_market_pda(&self.market, self.market.key())?;";
    let futarchy_guard =
        "validate_leverage_futarchy_pda(self.futarchy_authority.bump, self.futarchy_authority.key())?;";
    let boxed_account = "#[account(mut)]\n    pub market: Box<Account<'info, Market>>";

    for (source, validator, handler) in [
        (open, "pub fn validate_at", "pub fn handle_open"),
        (liquidate, "pub fn validate_at", "pub fn handle_liquidate"),
    ] {
        assert!(source.contains(boxed_account));
        let validator_start = source.find(validator).unwrap();
        let guard = source.find(boxed_runtime_guard).unwrap();
        let futarchy_guard = source.find(futarchy_guard).unwrap();
        let handler_start = source.find(handler).unwrap();
        assert!(validator_start < guard && guard < handler_start);
        assert!(validator_start < futarchy_guard && futarchy_guard < handler_start);
    }

    assert!(decrease.contains(boxed_account));
    let validator_start = decrease.find("pub fn validate_at").unwrap();
    let guard = decrease.find(boxed_runtime_guard).unwrap();
    let futarchy = decrease.find(futarchy_guard).unwrap();
    let handler_start = decrease.find("pub fn handle_decrease").unwrap();
    assert!(validator_start < guard && guard < handler_start);
    assert!(validator_start < futarchy && futarchy < handler_start);

    assert!(close.contains(boxed_account));
    assert_eq!(close.matches("self.validate_common").count(), 2);
    let common_start = close.find("fn validate_common").unwrap();
    let guard = close.find(boxed_runtime_guard).unwrap();
    let futarchy_guard = close.find(futarchy_guard).unwrap();
    let owner_validation = close.find("pub fn validate_at").unwrap();
    let delegated_validation = close.find("pub fn validate_delegated_at").unwrap();
    let handler_start = close.find("pub fn handle_close").unwrap();
    assert!(common_start < guard);
    assert!(common_start < futarchy_guard);
    assert!(guard < owner_validation && owner_validation < handler_start);
    assert!(futarchy_guard < owner_validation && owner_validation < handler_start);
    assert!(guard < delegated_validation && delegated_validation < handler_start);
    assert!(futarchy_guard < delegated_validation && delegated_validation < handler_start);

    for (entrypoint, validation, handler) in [
        (
            "pub fn open_leverage<'info>",
            "ctx.accounts.validate_at",
            "OpenLeverage::handle_open",
        ),
        (
            "pub fn close_leverage<'info>",
            "ctx.accounts.validate_at",
            "CloseLeverage::handle_close",
        ),
        (
            "pub fn delegated_close_leverage<'info>",
            "ctx.accounts.validate_delegated_at",
            "CloseLeverage::handle_delegated_close",
        ),
        (
            "pub fn decrease_leverage<'info>",
            "ctx.accounts.validate_at",
            "DecreaseLeverage::handle_decrease",
        ),
        (
            "pub fn liquidate_leverage<'info>",
            "ctx.accounts.validate_at",
            "LiquidateLeverage::handle_liquidate",
        ),
    ] {
        let entrypoint_start = lib.find(entrypoint).unwrap();
        let body = &lib[entrypoint_start..];
        let validation = body.find(validation).unwrap();
        let handler = body.find(handler).unwrap();
        assert!(validation < handler);
    }
}

#[test]
fn approved_for_requires_action_bit() {
    let approved_for = |approved_actions: u32, action: u32| -> Result<()> {
        require!(
            approved_actions & action == action,
            ErrorCode::InvalidLeverageDelegation
        );
        Ok(())
    };
    assert!(approved_for(LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_CLOSE).is_ok());
    assert!(approved_for(LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_INCREASE).is_err());
}

#[test]
fn delegation_approval_binds_close_context() {
    let program = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let position = Pubkey::new_unique();
    let delegation = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let approval = LeverageDelegationApproval::new(
        LEVERAGE_DELEGATE_CLOSE,
        market,
        owner,
        position,
        delegation,
        MarketAsset::Base,
        recipient,
        mint,
        123,
    );
    let mut data = Vec::new();
    approval.serialize(&mut data).unwrap();

    assert!(validate_delegation_approval(
        program,
        &data,
        program,
        LEVERAGE_DELEGATE_CLOSE,
        market,
        owner,
        position,
        delegation,
        MarketAsset::Base,
        recipient,
        mint,
        123,
    )
    .is_ok());
    assert!(validate_delegation_approval(
        program,
        &data,
        program,
        LEVERAGE_DELEGATE_CLOSE_SETTLED,
        market,
        owner,
        position,
        delegation,
        MarketAsset::Base,
        recipient,
        mint,
        123,
    )
    .is_err());
}

#[test]
fn token_2022_claimable_credit_is_split_proportionally() {
    let quote = LeverageSwapQuote {
        fee_credit: 100,
        fee_breakdown: crate::state::SwapFeeBreakdown {
            base_fee_debit: 60,
            distributed_surcharge_debit: 40,
            claimable_fee_debit: 100,
            ..crate::state::SwapFeeBreakdown::default()
        },
        ..LeverageSwapQuote::default()
    };

    let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, 97).unwrap();

    assert_eq!(credit.base, 58);
    assert_eq!(credit.distributed_surcharge, 39);
}

#[test]
fn retained_surcharge_never_enters_claimable_fee_credit() {
    let quote = LeverageSwapQuote {
        fee_credit: 30,
        fee_breakdown: crate::state::SwapFeeBreakdown {
            base_fee_debit: 30,
            dynamic_surcharge_debit: 70,
            retained_surcharge: 70,
            distributed_surcharge_debit: 0,
            claimable_fee_debit: 30,
            ..crate::state::SwapFeeBreakdown::default()
        },
        ..LeverageSwapQuote::default()
    };

    let credit = LeverageSwapFeeCredit::from_total_actual_credit(&quote, 29).unwrap();

    assert_eq!(credit.base, 29);
    assert_eq!(credit.distributed_surcharge, 0);
}

#[test]
fn ten_percent_transfer_fee_collateral_is_rejected_before_health_admission() {
    let mint_len =
        ExtensionType::try_calculate_account_len::<SplToken2022Mint>(&[ExtensionType::TransferFeeConfig]).unwrap();
    let mut mint_data = vec![0_u8; mint_len];
    {
        let mut mint = StateWithExtensionsMut::<SplToken2022Mint>::unpack_uninitialized(&mut mint_data).unwrap();
        let fee = TransferFee {
            epoch: 0_u64.into(),
            maximum_fee: u64::MAX.into(),
            transfer_fee_basis_points: 1_000_u16.into(),
        };
        let config = mint.init_extension::<TransferFeeConfig>(true).unwrap();
        config.older_transfer_fee = fee;
        config.newer_transfer_fee = fee;
        mint.base = SplToken2022Mint {
            mint_authority: COption::None,
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        };
        mint.pack_base();
        mint.init_account_type().unwrap();
    }

    let mint_key = Pubkey::new_unique();
    let owner = spl_token_2022::ID;
    let mut lamports = 1;
    let mint_info = AccountInfo::new(&mint_key, false, false, &mut lamports, &mut mint_data, &owner, false, 0);
    let mint = InterfaceAccount::<Mint>::try_from(&mint_info).unwrap();

    // The old health path stored the first net credit (900), then quoted
    // all 900 as unwind input even though the second transfer credits 810.
    let configured_fee = TransferFee {
        epoch: 0_u64.into(),
        maximum_fee: u64::MAX.into(),
        transfer_fee_basis_points: 1_000_u16.into(),
    };
    let gross_swap_output = 1_000;
    let stored_collateral = configured_fee.calculate_post_fee_amount(gross_swap_output).unwrap();
    let actual_unwind_credit = configured_fee.calculate_post_fee_amount(stored_collateral).unwrap();
    assert_eq!(stored_collateral, 900);
    assert_eq!(actual_unwind_credit, 810);
    assert_eq!(
        validate_leverage_collateral_risk_mint(&mint).unwrap_err(),
        error!(ErrorCode::InvalidLeverageCollateralMint)
    );
}

#[test]
fn token_2022_collateral_without_transfer_fee_extension_remains_supported() {
    let mint_len = ExtensionType::try_calculate_account_len::<SplToken2022Mint>(&[]).unwrap();
    let mut mint_data = vec![0_u8; mint_len];
    {
        let mut mint = StateWithExtensionsMut::<SplToken2022Mint>::unpack_uninitialized(&mut mint_data).unwrap();
        mint.base = SplToken2022Mint {
            mint_authority: COption::None,
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        };
        mint.pack_base();
        mint.init_account_type().unwrap();
    }

    let mint_key = Pubkey::new_unique();
    let owner = spl_token_2022::ID;
    let mut lamports = 1;
    let mint_info = AccountInfo::new(&mint_key, false, false, &mut lamports, &mut mint_data, &owner, false, 0);
    let mint = InterfaceAccount::<Mint>::try_from(&mint_info).unwrap();

    validate_leverage_collateral_risk_mint(&mint).unwrap();
}
