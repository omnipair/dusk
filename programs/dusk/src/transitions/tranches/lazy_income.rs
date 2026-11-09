use crate::state::{LazyIncomeBooks, MarketIncomeBooks};
use anchor_lang::prelude::*;
use std::{cell::OnceCell, io, rc::Rc};

static EMPTY_INCOME_BYTES: [u8; MarketIncomeBooks::INIT_SPACE] = [0; MarketIncomeBooks::INIT_SPACE];

impl LazyIncomeBooks {
    /// Refresh after CPI without allocating a second income book on SBF's
    /// non-reclaiming heap. Shared snapshots retain copy-on-write isolation.
    #[inline(never)]
    pub(crate) fn reload(&mut self, input: &mut &[u8]) -> io::Result<()> {
        if input.len() < Self::INIT_SPACE {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (bytes, rest) = input.split_at(Self::INIT_SPACE);
        if let Some(books) = self.decoded.get_mut() {
            let target = Rc::make_mut(books);
            let input = &mut &bytes[..];
            // Reuse the existing nested boxes. Re-decoding the whole struct
            // allocates every book again on SBF's non-reclaiming heap.
            for book in &mut target.books {
                **book = AnchorDeserialize::deserialize(input)?;
            }
            for book in &mut target.revenue {
                **book = AnchorDeserialize::deserialize(input)?;
            }
            target.pending = AnchorDeserialize::deserialize(input)?;
            target.seed = AnchorDeserialize::deserialize(input)?;
            target.hlp = AnchorDeserialize::deserialize(input)?;
            target.junior_end = AnchorDeserialize::deserialize(input)?;
            target.split_carry_bps = AnchorDeserialize::deserialize(input)?;
        } else {
            *self = Self::deserialize(&mut &bytes[..])?;
        }
        *input = rest;
        Ok(())
    }

    pub fn get(&self) -> Result<&MarketIncomeBooks> {
        if self.decoded.get().is_none() {
            let books = MarketIncomeBooks::default();
            self.decoded
                .set(Rc::new(books))
                .map_err(|_| error!(crate::errors::ErrorCode::BrokenInvariant))?;
        }
        self.decoded
            .get()
            .map(Rc::as_ref)
            .ok_or_else(|| error!(crate::errors::ErrorCode::BrokenInvariant))
    }

    pub fn get_mut(&mut self) -> Result<&mut MarketIncomeBooks> {
        self.get()?;
        self.decoded
            .get_mut()
            .map(Rc::make_mut)
            .ok_or_else(|| error!(crate::errors::ErrorCode::BrokenInvariant))
    }
}

impl Space for LazyIncomeBooks {
    const INIT_SPACE: usize = MarketIncomeBooks::INIT_SPACE;
}

impl AnchorDeserialize for LazyIncomeBooks {
    fn deserialize(input: &mut &[u8]) -> io::Result<Self> {
        if input.len() < Self::INIT_SPACE {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (bytes, rest) = input.split_at(Self::INIT_SPACE);
        *input = rest;
        if anchor_lang::solana_program::program_memory::sol_memcmp(bytes, &EMPTY_INCOME_BYTES, Self::INIT_SPACE) == 0 {
            Ok(Self::default())
        } else {
            Ok(Self {
                decoded: OnceCell::from(Rc::new(MarketIncomeBooks::try_from_slice(bytes)?)),
            })
        }
    }

    #[inline(never)]
    fn deserialize_reader<R: io::Read>(reader: &mut R) -> io::Result<Self> {
        let mut encoded = [0; Self::INIT_SPACE];
        reader.read_exact(&mut encoded)?;
        Self::deserialize(&mut encoded.as_slice())
    }
}

impl AnchorSerialize for LazyIncomeBooks {
    fn serialize<W: io::Write>(&self, writer: &mut W) -> io::Result<()> {
        if let Some(books) = self.decoded.get() {
            books.serialize(writer)
        } else {
            // Every default field has an all-zero fixed Borsh encoding. Avoid
            // allocating all four books just to save an untouched new market.
            writer.write_all(&EMPTY_INCOME_BYTES)
        }
    }
}

#[cfg(feature = "idl-build")]
impl anchor_lang::IdlBuild for LazyIncomeBooks {
    fn create_type() -> Option<anchor_lang::idl::types::IdlTypeDef> {
        MarketIncomeBooks::create_type()
    }
    fn insert_types(types: &mut std::collections::BTreeMap<String, anchor_lang::idl::types::IdlTypeDef>) {
        MarketIncomeBooks::insert_types(types)
    }
    fn get_full_path() -> String {
        MarketIncomeBooks::get_full_path()
    }
}

#[cfg(test)]
mod tests {
    include!("../../tests/transitions/tranches/lazy_income.rs");
}
