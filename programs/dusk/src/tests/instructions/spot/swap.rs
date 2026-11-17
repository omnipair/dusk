use super::*;

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
    let mut market = Market {
        srlp_mint: Pubkey::new_unique(),
        ..Market::current_layout()
    };
    market.base_hlp_vault.srlp_vault = Pubkey::new_unique();
    market.quote_hlp_vault.srlp_vault = Pubkey::new_unique();
    market.base_hlp_vault.hlp_supply = 1;
    market
}

fn canonical_hlp_swap_accounts(market: &Market) -> Vec<TestAccount> {
    vec![
        TestAccount::new(market.srlp_mint, Token2022::id(), true),
        TestAccount::new(market.base_hlp_vault.srlp_vault, Token2022::id(), true),
        TestAccount::new(market.quote_hlp_vault.srlp_vault, Token2022::id(), true),
    ]
}

#[test]
fn inactive_hlp_leaves_every_remaining_account_available_to_transfer_hooks() {
    let market = Market::current_layout();
    let hook_key = Pubkey::new_unique();
    let mut accounts = [TestAccount::new(hook_key, Pubkey::new_unique(), false)];
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
    accounts.swap(BASE_HLP_SRLP_VAULT_INDEX, QUOTE_HLP_SRLP_VAULT_INDEX);
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
    accounts[QUOTE_HLP_SRLP_VAULT_INDEX].owner = anchor_lang::system_program::ID;
    let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();
    HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap();

    let mut accounts = canonical_hlp_swap_accounts(&market);
    accounts[BASE_HLP_SRLP_VAULT_INDEX].owner = anchor_lang::system_program::ID;
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
    accounts[BASE_HLP_SRLP_VAULT_INDEX].writable = false;
    let infos: Vec<_> = accounts.iter_mut().map(TestAccount::account_info).collect();

    assert_eq!(
        HlpSwapAccountLayout::try_from((&market, infos.as_slice())).unwrap_err(),
        error!(ErrorCode::InvalidVault)
    );
}

#[test]
fn legacy_spl_custody_projection_uses_the_full_input_credit() {
    let projected = projected_reserve_vault_balance(1_000_000, 100_000, 30_000).unwrap();
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
    let projected = projected_reserve_vault_balance(1_000_000, gross_input - transfer_fee, 30_000).unwrap();
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
fn reserve_custody_covers_booked_and_pending_protocol_interest() {
    let mut side = crate::state::MarketSide::default();
    side.reserves.cash_reserve = 1_000;
    side.fees.interest_protocol_fee_liability = 30;
    side.fees.interest_buyback_fee_liability = 20;
    side.fees.referral_interest_liability = 10;
    side.fees.ledger.protocol_pending = 5;
    require_reserve_custody(1_065, &side).unwrap();
    assert_eq!(
        require_reserve_custody(1_064, &side).unwrap_err(),
        error!(ErrorCode::UnbackedFeeLiability)
    );
}
