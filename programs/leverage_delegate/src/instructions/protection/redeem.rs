use super::*;

impl<'info> ExecuteProtectionOrder<'info> {
    #[inline(never)]
    pub(super) fn redeem_lp(
        &self,
        lp_amount: u64,
        payment_asset: MarketAsset,
        min_payment_out: u64,
        signer: &[&[&[u8]]],
        remaining: &[AccountInfo<'info>],
    ) -> Result<()> {
        if self.lp_mint.key() == self.market.ylp_mint {
            dusk::cpi::remove_liquidity(
                CpiContext::new_with_signer(
                    self.dusk_program.to_account_info(),
                    dusk::cpi::accounts::RemoveLiquidity {
                        market: self.market.to_account_info(),
                        owner: self.order.to_account_info(),
                        base_mint: self.base_mint.to_account_info(),
                        quote_mint: self.quote_mint.to_account_info(),
                        ylp_mint: self.ylp_mint.to_account_info(),
                        base_reserve_vault: self.base_reserve_vault.to_account_info(),
                        quote_reserve_vault: self.quote_reserve_vault.to_account_info(),
                        owner_base_account: self.custody_base_account.to_account_info(),
                        owner_quote_account: self.custody_quote_account.to_account_info(),
                        owner_ylp_account: self.custody_lp_account.to_account_info(),
                        base_yield_account: self.base_yield_account.to_account_info(),
                        quote_yield_account: self.quote_yield_account.to_account_info(),
                        token_program: self.token_program.to_account_info(),
                        token_2022_program: self.token_2022_program.to_account_info(),
                        event_authority: self.dusk_event_authority.to_account_info(),
                        program: self.dusk_program.to_account_info(),
                    },
                    signer,
                )
                .with_remaining_accounts(remaining.to_vec()),
                dusk::instructions::RemoveLiquidityArgs {
                    ylp_amount: lp_amount,
                    min_base_amount_out: if payment_asset == MarketAsset::Base {
                        min_payment_out
                    } else {
                        0
                    },
                    min_quote_amount_out: if payment_asset == MarketAsset::Quote {
                        min_payment_out
                    } else {
                        0
                    },
                },
            )
        } else {
            require!(
                self.market.asset_for_hlp_mint(self.lp_mint.key())? == payment_asset,
                LeverageDelegateError::InvalidOrder
            );
            dusk::cpi::withdraw_single_sided(
                CpiContext::new_with_signer(
                    self.dusk_program.to_account_info(),
                    dusk::cpi::accounts::WithdrawSingleSided {
                        market: self.market.to_account_info(),
                        futarchy_authority: self.futarchy_authority.to_account_info(),
                        owner: self.order.to_account_info(),
                        base_mint: self.base_mint.to_account_info(),
                        quote_mint: self.quote_mint.to_account_info(),
                        ylp_mint: self.ylp_mint.to_account_info(),
                        target_hlp_mint: self.lp_mint.to_account_info(),
                        base_reserve_vault: self.base_reserve_vault.to_account_info(),
                        quote_reserve_vault: self.quote_reserve_vault.to_account_info(),
                        borrowed_interest_vault: self
                            .borrowed_interest_vault
                            .as_ref()
                            .ok_or(LeverageDelegateError::InvalidOrder)?
                            .to_account_info(),
                        owner_target_account: if payment_asset == MarketAsset::Base {
                            self.custody_base_account.to_account_info()
                        } else {
                            self.custody_quote_account.to_account_info()
                        },
                        owner_hlp_account: self.custody_lp_account.to_account_info(),
                        hlp_ylp_account: self
                            .hlp_ylp_account
                            .as_ref()
                            .ok_or(LeverageDelegateError::InvalidOrder)?
                            .to_account_info(),
                        base_yield_account: self.base_yield_account.to_account_info(),
                        quote_yield_account: self.quote_yield_account.to_account_info(),
                        token_program: self.token_program.to_account_info(),
                        token_2022_program: self.token_2022_program.to_account_info(),
                        event_authority: self.dusk_event_authority.to_account_info(),
                        program: self.dusk_program.to_account_info(),
                    },
                    signer,
                )
                .with_remaining_accounts(remaining.to_vec()),
                dusk::instructions::WithdrawSingleSidedArgs {
                    hlp_amount: lp_amount,
                    min_target_amount_out: min_payment_out,
                },
            )
        }
    }
}
