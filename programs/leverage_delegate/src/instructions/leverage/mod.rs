use anchor_lang::{prelude::*, solana_program::program::set_return_data};
use anchor_spl::token_interface::{Mint, TokenAccount};
use dusk::{
    constants::{BPS_DENOMINATOR, MARKET_LAYOUT_VERSION, NAD},
    instructions::{
        quote_delegated_close_payout, LeverageDelegationApproval, LEVERAGE_DELEGATE_CLOSE,
        LEVERAGE_DELEGATE_CLOSE_SETTLED,
    },
    state::{LeverageDelegation, LeveragePosition, Market},
    token::get_transfer_fee,
};

use crate::{constants::*, errors::*, state::*};

mod after_close;
mod before_close;
mod cancel;
mod common;
mod create;
mod update;

pub use after_close::*;
pub use before_close::*;
pub use cancel::*;
pub use common::*;
pub use create::*;
pub use update::*;

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/leverage.rs");
}
