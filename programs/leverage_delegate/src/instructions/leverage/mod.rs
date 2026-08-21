use anchor_lang::{prelude::*, solana_program::program::set_return_data};
use anchor_spl::{
    token::Token,
    token_2022::Token2022,
    token_interface::{Mint, TokenAccount},
};
use dusk::{
    constants::{BPS_DENOMINATOR, MARKET_LAYOUT_VERSION, NAD},
    instructions::{
        LeverageDelegationApproval, LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_CLOSE_SETTLED,
    },
    math::numerics::ceil_div,
    state::{LeverageDelegation, LeveragePosition, Market},
    token::get_transfer_fee,
};
use std::cmp::min;

use crate::{constants::*, errors::*, state::*, token::*};

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
