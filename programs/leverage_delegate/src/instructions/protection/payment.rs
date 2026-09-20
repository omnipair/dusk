use super::*;

impl<'info> ExecuteProtectionOrder<'info> {
    #[inline(never)]
    pub(super) fn make_payment(
        &self,
        amount: u64,
        mint: AccountInfo<'info>,
        remaining: &[AccountInfo<'info>],
    ) -> Result<()> {
        let referral_partner = self.referral_partner.as_ref().map(|p| p.to_account_info());
        let referral_accrual = self.referral_accrual.as_ref().map(|p| p.to_account_info());
        match self.order.action {
            0 => dusk::cpi::repay(
                CpiContext::new(
                    self.dusk_program.to_account_info(),
                    dusk::cpi::accounts::Repay {
                        market: self.market.to_account_info(),
                        futarchy_authority: self.futarchy_authority.to_account_info(),
                        owner: self.keeper.to_account_info(),
                        debt_asset_mint: mint,
                        reserve_vault: self.payment_vault.to_account_info(),
                        interest_vault: self.debt_interest_vault.to_account_info(),
                        owner_debt_account: self.keeper_payment_account.to_account_info(),
                        borrow_position: self
                            .borrow_position
                            .as_ref()
                            .ok_or(LeverageDelegateError::InvalidOrder)?
                            .to_account_info(),
                        referral_partner,
                        referral_accrual,
                        token_program: self.token_program.to_account_info(),
                        token_2022_program: self.token_2022_program.to_account_info(),
                        event_authority: self.dusk_event_authority.to_account_info(),
                        program: self.dusk_program.to_account_info(),
                    },
                )
                .with_remaining_accounts(remaining.to_vec()),
                dusk::instructions::RepayArgs {
                    repay_amount: amount,
                },
            ),
            1 => dusk::cpi::donate_collateral(
                CpiContext::new(
                    self.dusk_program.to_account_info(),
                    dusk::cpi::accounts::DonateCollateral {
                        market: self.market.to_account_info(),
                        owner: self.keeper.to_account_info(),
                        asset_mint: mint,
                        collateral_vault: self.payment_vault.to_account_info(),
                        owner_asset_account: self.keeper_payment_account.to_account_info(),
                        borrow_position: self
                            .borrow_position
                            .as_ref()
                            .ok_or(LeverageDelegateError::InvalidOrder)?
                            .to_account_info(),
                        token_program: self.token_program.to_account_info(),
                        token_2022_program: self.token_2022_program.to_account_info(),
                        event_authority: self.dusk_event_authority.to_account_info(),
                        program: self.dusk_program.to_account_info(),
                    },
                )
                .with_remaining_accounts(remaining.to_vec()),
                dusk::instructions::DonateCollateralArgs {
                    deposit_amount: amount,
                },
            ),
            2 => dusk::cpi::repay_leverage(
                CpiContext::new(
                    self.dusk_program.to_account_info(),
                    dusk::cpi::accounts::AddLeverageMargin {
                        market: self.market.to_account_info(),
                        futarchy_authority: self.futarchy_authority.to_account_info(),
                        position_owner: self.position_owner.to_account_info(),
                        leverage_position: self
                            .leverage_position
                            .as_ref()
                            .ok_or(LeverageDelegateError::InvalidOrder)?
                            .to_account_info(),
                        debt_mint: mint,
                        debt_reserve_vault: self.payment_vault.to_account_info(),
                        debt_interest_vault: self.debt_interest_vault.to_account_info(),
                        owner_debt_account: self.keeper_payment_account.to_account_info(),
                        referral_partner,
                        referral_accrual,
                        owner: self.keeper.to_account_info(),
                        token_program: self.token_program.to_account_info(),
                        token_2022_program: self.token_2022_program.to_account_info(),
                        event_authority: self.dusk_event_authority.to_account_info(),
                        program: self.dusk_program.to_account_info(),
                    },
                )
                .with_remaining_accounts(remaining.to_vec()),
                dusk::instructions::AddLeverageMarginArgs {
                    debt_asset: self.order.debt_asset,
                    amount,
                },
            ),
            _ => err!(LeverageDelegateError::InvalidOrder),
        }
    }
}
