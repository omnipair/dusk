use anchor_lang::{prelude::*, solana_program::hash::hashv};

use crate::{
    constants::{
        BPS_DENOMINATOR, MAX_PROPOSAL_DESCRIPTION_BYTES, MAX_PROPOSAL_DESCRIPTION_URI_BYTES, MAX_PROPOSAL_TITLE_BYTES,
        PARAMETER_PROPOSAL_EXECUTION_WINDOW_SECONDS, PARAMETER_PROPOSAL_SPONSOR_BPS, PARAMETER_PROPOSAL_SUPPORT_BPS,
        PARAMETER_PROPOSAL_TIMELOCK_SECONDS, PROPOSAL_METADATA_VERSION,
    },
    errors::ErrorCode,
};

use super::{FeeProfile, IrmConfig};

/// Domain separator for the immutable, client-verifiable proposal digest.
pub const PARAMETER_PROPOSAL_DIGEST_DOMAIN: &[u8] = b"DUSK_PARAMETER_PROPOSAL_V1";

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum ParameterFamily {
    Fee,
    Concentration,
    Irm,
    EmaHalfLives,
    DailyBorrowLimit,
}

impl ParameterFamily {
    pub fn code(self) -> u8 {
        match self {
            Self::Fee => 0,
            Self::Concentration => 1,
            Self::Irm => 2,
            Self::EmaHalfLives => 3,
            Self::DailyBorrowLimit => 4,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, InitSpace, PartialEq, Eq)]
pub enum MarketParameterUpdate {
    Fee(FeeProfile),
    Concentration {
        peak_depth_nad: u64,
        fade_scale_nad: u64,
        ramp_duration_slots: u64,
    },
    Irm(IrmConfig),
    EmaHalfLives {
        price_ms: u64,
        directional_price_ms: u64,
        q_ms: u64,
        center_price_ms: u64,
    },
    DailyBorrowLimit {
        max_daily_borrow_bps: u16,
    },
}

impl MarketParameterUpdate {
    pub fn family(&self) -> ParameterFamily {
        match self {
            Self::Fee(_) => ParameterFamily::Fee,
            Self::Concentration { .. } => ParameterFamily::Concentration,
            Self::Irm(_) => ParameterFamily::Irm,
            Self::EmaHalfLives { .. } => ParameterFamily::EmaHalfLives,
            Self::DailyBorrowLimit { .. } => ParameterFamily::DailyBorrowLimit,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, InitSpace, PartialEq, Eq)]
pub struct ProposalMetadataV1 {
    pub version: u8,
    #[max_len(96)]
    pub title: String,
    #[max_len(200)]
    pub description_uri: String,
    pub description_sha256: [u8; 32],
    pub description_len: u32,
}

impl ProposalMetadataV1 {
    pub fn validate(&self) -> Result<()> {
        require_eq!(
            self.version,
            PROPOSAL_METADATA_VERSION,
            ErrorCode::InvalidProposalMetadata
        );
        require!(
            !self.title.is_empty()
                && self.title.len() <= MAX_PROPOSAL_TITLE_BYTES
                && self.title == self.title.trim()
                && !self.title.chars().any(char::is_control),
            ErrorCode::InvalidProposalMetadata
        );
        require!(
            !self.description_uri.is_empty()
                && self.description_uri.len() <= MAX_PROPOSAL_DESCRIPTION_URI_BYTES
                && self.description_uri.is_ascii()
                && !self
                    .description_uri
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()),
            ErrorCode::InvalidProposalUri
        );
        let uri_payload = self
            .description_uri
            .strip_prefix("ipfs://")
            .or_else(|| self.description_uri.strip_prefix("ar://"))
            .or_else(|| self.description_uri.strip_prefix("https://"));
        require!(
            uri_payload.is_some_and(|payload| !payload.is_empty()),
            ErrorCode::InvalidProposalUri
        );
        require!(
            (1..=MAX_PROPOSAL_DESCRIPTION_BYTES).contains(&self.description_len) && self.description_sha256 != [0; 32],
            ErrorCode::InvalidProposalMetadata
        );
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace, PartialEq, Eq)]
pub enum ParameterProposalStatus {
    Collecting,
    Queued,
    Executed,
    Cancelled,
    Expired,
    Stale,
}

impl ParameterProposalStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Executed | Self::Cancelled | Self::Expired | Self::Stale)
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Collecting => 0,
            Self::Queued => 1,
            Self::Executed => 2,
            Self::Cancelled => 3,
            Self::Expired => 4,
            Self::Stale => 5,
        }
    }
}

#[account]
#[derive(InitSpace)]
pub struct ParameterProposal {
    pub market: Pubkey,
    pub proposer: Pubkey,
    pub nonce: u64,
    pub family: ParameterFamily,
    pub family_revision: u64,
    pub update: MarketParameterUpdate,
    pub metadata: ProposalMetadataV1,
    pub digest: [u8; 32],
    pub status: ParameterProposalStatus,
    /// Frozen minimum support needed to keep this proposal alive after its
    /// initial 1% sponsorship burn-lock.
    pub sponsorship_floor: u64,
    pub total_locked: u64,
    /// Immutable queue-time numerator and direct-yLP denominator.
    pub queued_support: u64,
    pub queued_eligible_ylp: u64,
    pub created_at: i64,
    pub queued_at: i64,
    pub execute_after: i64,
    pub execution_deadline: i64,
    pub bump: u8,
}

impl ParameterProposal {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        &mut self,
        market: Pubkey,
        proposer: Pubkey,
        nonce: u64,
        family_revision: u64,
        update: MarketParameterUpdate,
        metadata: ProposalMetadataV1,
        eligible_supply: u64,
        created_at: i64,
        bump: u8,
    ) -> Result<()> {
        metadata.validate()?;
        require!(eligible_supply > 0, ErrorCode::ProposalSponsorshipTooLow);
        let family = update.family();
        let digest =
            parameter_proposal_digest(crate::ID, market, proposer, nonce, family_revision, &update, &metadata)?;
        self.market = market;
        self.proposer = proposer;
        self.nonce = nonce;
        self.family = family;
        self.family_revision = family_revision;
        self.update = update;
        self.metadata = metadata;
        self.digest = digest;
        self.status = ParameterProposalStatus::Collecting;
        self.sponsorship_floor = sponsorship_floor(eligible_supply)?;
        self.total_locked = 0;
        self.queued_support = 0;
        self.queued_eligible_ylp = 0;
        self.created_at = created_at;
        self.queued_at = 0;
        self.execute_after = 0;
        self.execution_deadline = 0;
        self.bump = bump;
        Ok(())
    }

    pub fn assert_account(&self, market: Pubkey, proposal_key: Pubkey) -> Result<()> {
        require_keys_eq!(self.market, market, ErrorCode::InvalidParameterProposal);
        let nonce = self.nonce.to_le_bytes();
        let bump = [self.bump];
        let expected = Pubkey::create_program_address(
            &[
                crate::constants::PARAMETER_PROPOSAL_SEED_PREFIX,
                self.market.as_ref(),
                self.proposer.as_ref(),
                &nonce,
                &bump,
            ],
            &crate::ID,
        )
        .map_err(|_| error!(ErrorCode::InvalidParameterProposal))?;
        require_keys_eq!(proposal_key, expected, ErrorCode::InvalidParameterProposal);
        require!(self.update.family() == self.family, ErrorCode::InvalidParameterProposal);
        self.assert_digest()
    }

    pub fn assert_digest(&self) -> Result<()> {
        let expected = parameter_proposal_digest(
            crate::ID,
            self.market,
            self.proposer,
            self.nonce,
            self.family_revision,
            &self.update,
            &self.metadata,
        )?;
        require!(self.digest == expected, ErrorCode::InvalidProposalDigest);
        Ok(())
    }

    pub fn queue_if_supported(&mut self, eligible_supply: u64, now: i64) -> Result<bool> {
        require!(
            self.status == ParameterProposalStatus::Collecting,
            ErrorCode::ProposalNotCollecting
        );
        if !has_strict_support_majority(self.total_locked, eligible_supply)? {
            return Ok(false);
        }
        self.queued_support = self.total_locked;
        self.queued_eligible_ylp = eligible_supply;
        self.status = ParameterProposalStatus::Queued;
        self.queued_at = now;
        self.execute_after = now
            .checked_add(PARAMETER_PROPOSAL_TIMELOCK_SECONDS)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        self.execution_deadline = self
            .execute_after
            .checked_add(PARAMETER_PROPOSAL_EXECUTION_WINDOW_SECONDS)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        Ok(true)
    }

    pub fn mark_stale_if_revision_changed(&mut self, current_revision: u64) -> bool {
        if matches!(
            self.status,
            ParameterProposalStatus::Collecting | ParameterProposalStatus::Queued
        ) && current_revision != self.family_revision
        {
            self.status = ParameterProposalStatus::Stale;
            return true;
        }
        false
    }

    pub fn mark_expired_if_past_deadline(&mut self, now: i64) -> bool {
        if self.status == ParameterProposalStatus::Queued && now > self.execution_deadline {
            self.status = ParameterProposalStatus::Expired;
            return true;
        }
        false
    }

    pub fn cancel_if_below_sponsorship_floor(&mut self) -> bool {
        if self.status == ParameterProposalStatus::Collecting && self.total_locked < self.sponsorship_floor {
            self.status = ParameterProposalStatus::Cancelled;
            return true;
        }
        false
    }
}

pub fn sponsorship_floor(eligible_supply: u64) -> Result<u64> {
    let numerator = (eligible_supply as u128)
        .checked_mul(PARAMETER_PROPOSAL_SPONSOR_BPS as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let denominator = BPS_DENOMINATOR as u128;
    let rounded = numerator
        .checked_add(denominator - 1)
        .and_then(|value| value.checked_div(denominator))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    u64::try_from(rounded).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

pub fn has_strict_support_majority(total_locked: u64, eligible_supply: u64) -> Result<bool> {
    let support = (total_locked as u128)
        .checked_mul(BPS_DENOMINATOR as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let threshold = (eligible_supply as u128)
        .checked_mul(PARAMETER_PROPOSAL_SUPPORT_BPS as u128)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(support > threshold)
}

#[allow(clippy::too_many_arguments)]
pub fn parameter_proposal_digest(
    program_id: Pubkey,
    market: Pubkey,
    proposer: Pubkey,
    nonce: u64,
    family_revision: u64,
    update: &MarketParameterUpdate,
    metadata: &ProposalMetadataV1,
) -> Result<[u8; 32]> {
    let update_bytes = update
        .try_to_vec()
        .map_err(|_| error!(ErrorCode::InvalidProposalDigest))?;
    let metadata_bytes = metadata
        .try_to_vec()
        .map_err(|_| error!(ErrorCode::InvalidProposalDigest))?;
    let nonce = nonce.to_le_bytes();
    let family_revision = family_revision.to_le_bytes();
    Ok(hashv(&[
        PARAMETER_PROPOSAL_DIGEST_DOMAIN,
        program_id.as_ref(),
        market.as_ref(),
        proposer.as_ref(),
        &nonce,
        &family_revision,
        &update_bytes,
        &metadata_bytes,
    ])
    .to_bytes())
}

#[cfg(test)]
mod tests {
    include!("../tests/state/parameter_proposal.rs");
}
