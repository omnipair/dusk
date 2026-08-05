use anchor_lang::{
    prelude::*,
    solana_program::{program::invoke, program::invoke_signed, system_instruction},
};
use anchor_spl::token_interface::Mint;
use spl_tlv_account_resolution::{account::ExtraAccountMeta, seeds::Seed, state::ExtraAccountMetaList};
use spl_transfer_hook_interface::{
    collect_extra_account_metas_signer_seeds, get_extra_account_metas_address, instruction::ExecuteInstruction,
};

use crate::{
    constants::{TRANSFER_HOOK_EXTRA_ACCOUNT_METAS_SEED_PREFIX, YIELD_ACCOUNT_SEED_PREFIX},
    errors::ErrorCode,
    instructions::common::validate_lp_mint,
    state::{Market, MarketAsset, YieldTokenKind},
};

const LP_TRANSFER_HOOK_META_COUNT: usize = 7;
const TRANSFER_HOOK_MARKET_INDEX: u8 = 5;
const TRANSFER_HOOK_BASE_MINT_INDEX: u8 = 6;
const TRANSFER_HOOK_QUOTE_MINT_INDEX: u8 = 7;
const TOKEN_ACCOUNT_OWNER_OFFSET: u8 = 32;

#[derive(Accounts)]
pub struct InitializeLpTransferHook<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Standard SPL transfer-hook validation PDA, derived and verified
    /// against `lp_mint` and owned by Dusk after initialization.
    #[account(mut)]
    pub validation_account: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

impl<'info> InitializeLpTransferHook<'info> {
    pub fn validate(&self) -> Result<()> {
        let lp_mint = self.lp_mint.key();
        let lp_decimals = if lp_mint == self.market.ylp_mint {
            self.market.base_side.asset_decimals
        } else {
            match self.market.asset_for_hlp_mint(lp_mint)? {
                MarketAsset::Base => self.market.base_side.asset_decimals,
                MarketAsset::Quote => self.market.quote_side.asset_decimals,
            }
        };
        validate_lp_mint(&self.lp_mint, self.market.key(), lp_decimals)?;
        require_keys_eq!(
            self.validation_account.key(),
            get_extra_account_metas_address(&lp_mint, &crate::ID),
            ErrorCode::InvalidArgument
        );
        canonical_lp_transfer_hook_metas(self.market.key(), &self.market, lp_mint)?;
        Ok(())
    }

    pub fn handle_initialize(ctx: Context<'_, '_, '_, 'info, Self>) -> Result<()> {
        let lp_mint = ctx.accounts.lp_mint.key();
        let validation_info = ctx.accounts.validation_account.to_account_info();
        let account_size = ExtraAccountMetaList::size_of(LP_TRANSFER_HOOK_META_COUNT)
            .map_err(|_| error!(ErrorCode::MarketMathOverflow))?;
        if validation_info.owner == &System::id() {
            require_eq!(validation_info.data_len(), 0, ErrorCode::InvalidArgument);
            let rent = Rent::get()?;
            let required_lamports = rent.minimum_balance(account_size);
            let bump = Pubkey::find_program_address(
                &[TRANSFER_HOOK_EXTRA_ACCOUNT_METAS_SEED_PREFIX, lp_mint.as_ref()],
                &crate::ID,
            )
            .1;
            let bump_seed = [bump];
            let signer_seeds = collect_extra_account_metas_signer_seeds(&lp_mint, &bump_seed);
            if validation_info.lamports() == 0 {
                invoke_signed(
                    &system_instruction::create_account(
                        &ctx.accounts.payer.key(),
                        &validation_info.key(),
                        required_lamports,
                        account_size as u64,
                        &crate::ID,
                    ),
                    &[
                        ctx.accounts.payer.to_account_info(),
                        validation_info.clone(),
                        ctx.accounts.system_program.to_account_info(),
                    ],
                    &[&signer_seeds],
                )?;
            } else {
                let top_up = required_lamports.saturating_sub(validation_info.lamports());
                if top_up > 0 {
                    invoke(
                        &system_instruction::transfer(&ctx.accounts.payer.key(), &validation_info.key(), top_up),
                        &[
                            ctx.accounts.payer.to_account_info(),
                            validation_info.clone(),
                            ctx.accounts.system_program.to_account_info(),
                        ],
                    )?;
                }
                invoke_signed(
                    &system_instruction::allocate(&validation_info.key(), account_size as u64),
                    &[validation_info.clone(), ctx.accounts.system_program.to_account_info()],
                    &[&signer_seeds],
                )?;
                invoke_signed(
                    &system_instruction::assign(&validation_info.key(), &crate::ID),
                    &[validation_info.clone(), ctx.accounts.system_program.to_account_info()],
                    &[&signer_seeds],
                )?;
            }
        }
        require_keys_eq!(*validation_info.owner, crate::ID, ErrorCode::InvalidArgument);
        require_eq!(validation_info.data_len(), account_size, ErrorCode::InvalidArgument);

        let extra_metas = canonical_lp_transfer_hook_metas(ctx.accounts.market.key(), &ctx.accounts.market, lp_mint)?;
        let mut expected = vec![0_u8; account_size];
        ExtraAccountMetaList::init::<ExecuteInstruction>(&mut expected, &extra_metas)
            .map_err(|_| error!(ErrorCode::InvalidArgument))?;
        let mut data = validation_info.try_borrow_mut_data()?;
        if data.iter().all(|byte| *byte == 0) {
            data.copy_from_slice(&expected);
        } else {
            require!(data.as_ref() == expected.as_slice(), ErrorCode::InvalidArgument);
        }
        Ok(())
    }
}

pub(crate) fn canonical_lp_transfer_hook_metas(
    market_key: Pubkey,
    market: &Market,
    lp_mint: Pubkey,
) -> Result<[ExtraAccountMeta; LP_TRANSFER_HOOK_META_COUNT]> {
    let token_kind = if lp_mint == market.ylp_mint {
        YieldTokenKind::Ylp
    } else {
        market.asset_for_hlp_mint(lp_mint)?;
        YieldTokenKind::Hlp
    };
    Ok([
        ExtraAccountMeta::new_with_pubkey(&market_key, false, true).map_err(|_| error!(ErrorCode::InvalidArgument))?,
        ExtraAccountMeta::new_with_pubkey(&market.base_side.asset_mint, false, false)
            .map_err(|_| error!(ErrorCode::InvalidArgument))?,
        ExtraAccountMeta::new_with_pubkey(&market.quote_side.asset_mint, false, false)
            .map_err(|_| error!(ErrorCode::InvalidArgument))?,
        yield_account_extra_meta(0, TRANSFER_HOOK_BASE_MINT_INDEX, token_kind)?,
        yield_account_extra_meta(2, TRANSFER_HOOK_BASE_MINT_INDEX, token_kind)?,
        yield_account_extra_meta(0, TRANSFER_HOOK_QUOTE_MINT_INDEX, token_kind)?,
        yield_account_extra_meta(2, TRANSFER_HOOK_QUOTE_MINT_INDEX, token_kind)?,
    ])
}

fn yield_account_extra_meta(
    owner_token_account_index: u8,
    asset_mint_index: u8,
    token_kind: YieldTokenKind,
) -> Result<ExtraAccountMeta> {
    ExtraAccountMeta::new_with_seeds(
        &[
            Seed::Literal {
                bytes: YIELD_ACCOUNT_SEED_PREFIX.to_vec(),
            },
            Seed::AccountKey {
                index: TRANSFER_HOOK_MARKET_INDEX,
            },
            Seed::AccountData {
                account_index: owner_token_account_index,
                data_index: TOKEN_ACCOUNT_OWNER_OFFSET,
                length: 32,
            },
            Seed::AccountKey { index: 1 },
            Seed::AccountKey {
                index: asset_mint_index,
            },
            Seed::Literal {
                bytes: vec![token_kind.code()],
            },
        ],
        false,
        true,
    )
    .map_err(|_| error!(ErrorCode::InvalidArgument))
}
