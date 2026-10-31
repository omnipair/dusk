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
            0 => {
                use anchor_lang::{
                    solana_program::{
                        instruction::{AccountMeta, Instruction},
                        program::invoke_signed,
                    },
                    InstructionData,
                };
                let accounts = dusk::cpi::accounts::Repay {
                    market: self.market.to_account_info(),
                    futarchy_authority: self.futarchy_authority.to_account_info(),
                    owner: self.keeper.to_account_info(),
                    debt_asset_mint: mint,
                    collateral_asset_mint: if self.order.debt_asset == MarketAsset::Base.code() {
                        self.quote_mint.to_account_info()
                    } else {
                        self.base_mint.to_account_info()
                    },
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
                };
                // Build one CPI account buffer. Anchor's generated adapter
                // allocates a temporary Vec for every fixed account and another
                // for the settlement suffix; none are reclaimed on SBF.
                let fixed = [
                    accounts.market.clone(),
                    accounts.futarchy_authority.clone(),
                    accounts.owner.clone(),
                    accounts.debt_asset_mint.clone(),
                    accounts.collateral_asset_mint.clone(),
                    accounts.reserve_vault.clone(),
                    accounts.interest_vault.clone(),
                    accounts.owner_debt_account.clone(),
                    accounts.borrow_position.clone(),
                    accounts
                        .referral_partner
                        .clone()
                        .unwrap_or_else(|| self.dusk_program.to_account_info()),
                    accounts
                        .referral_accrual
                        .clone()
                        .unwrap_or_else(|| self.dusk_program.to_account_info()),
                    accounts.token_program.clone(),
                    accounts.token_2022_program.clone(),
                    accounts.event_authority.clone(),
                    accounts.program.clone(),
                ];
                let mut infos = Vec::with_capacity(fixed.len() + remaining.len());
                infos.extend(fixed);
                infos.extend_from_slice(remaining);
                let mut metas = accounts.to_account_metas(None);
                metas.reserve_exact(remaining.len());
                metas.extend(remaining.iter().map(|info| AccountMeta {
                    pubkey: info.key(),
                    is_signer: info.is_signer,
                    is_writable: info.is_writable,
                }));
                let instruction = Instruction {
                    program_id: dusk::ID,
                    accounts: metas,
                    data: dusk::instruction::Repay {
                        args: dusk::instructions::RepayArgs {
                            repay_amount: amount,
                        },
                    }
                    .data(),
                };
                invoke_signed(&instruction, &infos, &[]).map_err(Into::into)
            }
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
                        collateral_mint: if self.order.debt_asset == MarketAsset::Base.code() {
                            self.quote_mint.to_account_info()
                        } else {
                            self.base_mint.to_account_info()
                        },
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
