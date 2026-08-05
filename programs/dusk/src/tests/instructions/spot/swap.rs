use super::*;
use anchor_lang::solana_program::program_option::COption;
use spl_token_2022::{
    extension::{
        transfer_fee::{TransferFee, TransferFeeAmount},
        BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
    },
    state::{Account as SplToken2022Account, AccountState},
};

struct TestAccount {
    key: Pubkey,
    owner: Pubkey,
    writable: bool,
    lamports: u64,
    data: Vec<u8>,
}

impl TestAccount {
    fn new(key: Pubkey, owner: Pubkey, writable: bool) -> Self {
        Self {
            key,
            owner,
            writable,
            lamports: 1,
            data: vec![],
        }
    }

    fn account_info(&mut self) -> AccountInfo<'_> {
        AccountInfo::new(
            &self.key,
            false,
            self.writable,
            &mut self.lamports,
            self.data.as_mut_slice(),
            &self.owner,
            false,
            0,
        )
    }
}

fn market_requiring_hlp_swap_accounts() -> Market {
    let mut market = Market::default();
    market.ylp_mint = Pubkey::new_unique();
    market.base_hlp_vault.ylp_vault = Pubkey::new_unique();
    market.quote_hlp_vault.ylp_vault = Pubkey::new_unique();
    market.base_side.interest_vault = Pubkey::new_unique();
    market.quote_side.interest_vault = Pubkey::new_unique();
    market.base_hlp_vault.hlp_supply = 1;
    market
}

fn canonical_hlp_swap_accounts(market: &Market) -> Vec<TestAccount> {
    vec![
        TestAccount::new(market.ylp_mint, Token2022::id(), true),
        TestAccount::new(market.base_hlp_vault.ylp_vault, Token2022::id(), true),
        TestAccount::new(market.quote_hlp_vault.ylp_vault, Token2022::id(), true),
        TestAccount::new(market.base_side.interest_vault, Token::id(), true),
        TestAccount::new(market.quote_side.interest_vault, Token2022::id(), true),
    ]
}

#[test]
fn inactive_hlp_leaves_every_remaining_account_available_to_transfer_hooks() {
    let market = Market::default();
    let hook_key = Pubkey::new_unique();
    let mut accounts = vec![TestAccount::new(hook_key, Pubkey::new_unique(), false)];
    let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();

    let layout = HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap();

    assert_eq!(layout.prefix_len, 0);
    assert_eq!(layout.hook_accounts(&infos)[0].key(), hook_key);
}

#[test]
fn residual_only_hlp_rejects_a_missing_or_reordered_prefix_before_math() {
    let mut market = market_requiring_hlp_swap_accounts();
    market.base_hlp_vault.hlp_supply = 0;
    market.base_hlp_vault.residual_exposure = 1;
    let no_accounts: &[AccountInfo<'_>] = &[];
    assert_eq!(
        HlpSwapAccountLayout::try_from((&market, no_accounts)).unwrap_err(),
        error!(ErrorCode::NotEnoughAccounts)
    );

    let mut accounts = canonical_hlp_swap_accounts(&market);
    accounts.swap(BASE_HLP_YLP_VAULT_INDEX, QUOTE_HLP_YLP_VAULT_INDEX);
    let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();
    assert_eq!(
        HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap_err(),
        error!(ErrorCode::InvalidVault)
    );
}

#[test]
fn canonical_hlp_prefix_is_consumed_once_and_preserves_hook_tail_order() {
    let market = market_requiring_hlp_swap_accounts();
    let first_hook = Pubkey::new_unique();
    let second_hook = Pubkey::new_unique();
    let mut accounts = canonical_hlp_swap_accounts(&market);
    accounts.push(TestAccount::new(first_hook, Pubkey::new_unique(), false));
    accounts.push(TestAccount::new(second_hook, Pubkey::new_unique(), true));
    let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();

    let layout = HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap();
    let hook_accounts = layout.hook_accounts(&infos);

    assert_eq!(layout.prefix_len, HLP_SWAP_ACCOUNT_PREFIX_LEN);
    assert_eq!(hook_accounts.len(), 2);
    assert_eq!(hook_accounts[0].key(), first_hook);
    assert_eq!(hook_accounts[1].key(), second_hook);
}

#[test]
fn inactive_hlp_side_accepts_its_uninitialized_derived_vault_but_active_side_does_not() {
    let market = market_requiring_hlp_swap_accounts();
    let mut accounts = canonical_hlp_swap_accounts(&market);
    accounts[QUOTE_HLP_YLP_VAULT_INDEX].owner = anchor_lang::system_program::ID;
    let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();
    HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap();

    let mut accounts = canonical_hlp_swap_accounts(&market);
    accounts[BASE_HLP_YLP_VAULT_INDEX].owner = anchor_lang::system_program::ID;
    let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();
    assert_eq!(
        HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap_err(),
        error!(ErrorCode::InvalidTokenProgram)
    );
}

#[test]
fn canonical_hlp_prefix_rejects_readonly_settlement_accounts() {
    let market = market_requiring_hlp_swap_accounts();
    let mut accounts = canonical_hlp_swap_accounts(&market);
    accounts[BASE_INTEREST_VAULT_INDEX].writable = false;
    let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();

    assert_eq!(
        HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap_err(),
        error!(ErrorCode::InvalidVault)
    );
}

#[test]
fn legacy_spl_custody_projection_uses_the_full_input_credit() {
    let projected = projected_reserve_vault_balance(1_000_000, 100_000, 25_000, 5_000).unwrap();
    let mut side = crate::state::MarketSide::default();
    side.reserves.cash_reserve = 1_050_000;
    side.fees.swap_fee_custody_balance = 20_000;

    assert_eq!(projected, 1_070_000);
    require_reserve_custody(projected, &side).unwrap();
}

#[test]
fn token_2022_custody_projection_uses_net_input_and_gross_debits() {
    let gross_input = 100_000;
    let transfer_fee = 3_000;
    let projected = projected_reserve_vault_balance(1_000_000, gross_input - transfer_fee, 25_000, 5_000).unwrap();
    let mut side = crate::state::MarketSide::default();
    side.reserves.cash_reserve = 1_047_000;
    side.fees.swap_fee_custody_balance = 20_000;

    assert_eq!(projected, 1_067_000);
    require_reserve_custody(projected, &side).unwrap();
    assert_eq!(
        require_reserve_custody(projected - 1, &side).unwrap_err(),
        error!(ErrorCode::UnbackedFeeLiability)
    );
}

#[test]
fn token_2022_hlp_interest_reads_net_credit_from_remaining_vault_data() {
    let transfer_fee = TransferFee {
        epoch: 0_u64.into(),
        maximum_fee: u64::MAX.into(),
        transfer_fee_basis_points: 300_u16.into(),
    };
    let gross_interest_paid = 10_000;
    let balance_before = 40_000;
    let actual_credit = transfer_fee.calculate_post_fee_amount(gross_interest_paid).unwrap();
    let balance_after = balance_before + actual_credit;
    let account_len =
        ExtensionType::try_calculate_account_len::<SplToken2022Account>(&[ExtensionType::TransferFeeAmount]).unwrap();
    let mut account_data = vec![0_u8; account_len];
    {
        let mut account =
            StateWithExtensionsMut::<SplToken2022Account>::unpack_uninitialized(&mut account_data).unwrap();
        account.init_extension::<TransferFeeAmount>(true).unwrap();
        account.base = SplToken2022Account {
            mint: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            amount: balance_after,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        account.pack_base();
        account.init_account_type().unwrap();
    }
    let account_key = Pubkey::new_unique();
    let token_program = spl_token_2022::ID;
    let mut lamports = 1;
    let account_info = AccountInfo::new(
        &account_key,
        false,
        true,
        &mut lamports,
        &mut account_data,
        &token_program,
        false,
        0,
    );

    assert_eq!(
        token_account_info_credit(balance_before, &account_info).unwrap(),
        actual_credit
    );
    assert_eq!(actual_credit, 9_700);
    assert!(actual_credit < gross_interest_paid);
}
