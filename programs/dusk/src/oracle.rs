//! Market-selected on-chain oracle accounts. Authentication and freshness are
//! separate from the bounded market price; a provider report never values a
//! loan independently of executable market pricing.
use crate::{
    constants::NAD,
    errors::ErrorCode,
    math::{mul_div_ceil_u128, mul_div_u128},
};
use anchor_lang::{prelude::*, solana_program::hash::hashv};

pub const NORMALIZED_ORACLE_V1: u8 = 1;
pub const PYTH_RATIO_V2: u8 = 2;
pub const PYTH_DIRECT_V3: u8 = 3;
pub const NORMALIZED_RATIO_V4: u8 = 4;
pub const NORMALIZED_ORACLE_MAGIC: [u8; 8] = *b"DOPRV001";
pub const PYTH_RECEIVER: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");

/// Creation selects an optional policy atomically; omission uses native pricing.
/// Subsequent governance can change the policy within the same mathematical
/// limits. Every native market supports later oracle activation.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct OraclePolicy {
    pub format: u8,
    pub provider: Pubkey,
    pub base_account: Pubkey,
    pub quote_account: Pubkey,
    pub base_feed_id: [u8; 32],
    pub quote_feed_id: [u8; 32],
    pub lambda_bps: u16,
    pub adjustment_cap_nad: u64,
    pub max_age_seconds: u32,
    pub max_age_slots: u64,
    pub max_confidence_bps: u16,
    pub max_publish_skew_seconds: u32,
}
impl OraclePolicy {
    pub(crate) fn same_source(self, other: Self) -> bool {
        self.format == other.format
            && self.provider == other.provider
            && self.base_account == other.base_account
            && self.quote_account == other.quote_account
            && self.base_feed_id == other.base_feed_id
            && self.quote_feed_id == other.quote_feed_id
    }

    pub fn validate(self) -> Result<()> {
        require!(self.lambda_bps < 10_000, ErrorCode::InvalidMarketConfig);
        if self == Self::default() {
            return Ok(());
        }
        require!(self.adjustment_cap_nad > NAD, ErrorCode::InvalidMarketConfig);
        require!(
            self.max_age_seconds > 0 && self.max_age_slots > 0 && self.max_confidence_bps < 10_000,
            ErrorCode::InvalidMarketConfig
        );
        require!(
            self.provider != Pubkey::default()
                && self.base_account != Pubkey::default()
                && self.base_feed_id != [0; 32],
            ErrorCode::InvalidMarketConfig
        );
        require!(
            matches!(
                self.format,
                NORMALIZED_ORACLE_V1 | PYTH_RATIO_V2 | PYTH_DIRECT_V3 | NORMALIZED_RATIO_V4
            ),
            ErrorCode::InvalidMarketConfig
        );
        if matches!(self.format, PYTH_RATIO_V2 | PYTH_DIRECT_V3) {
            require_keys_eq!(self.provider, PYTH_RECEIVER, ErrorCode::InvalidMarketConfig);
        }
        if self.two_feeds() {
            require!(
                self.quote_account != Pubkey::default()
                    && self.quote_feed_id != [0; 32]
                    && self.base_account != self.quote_account
                    && self.base_feed_id != self.quote_feed_id,
                ErrorCode::InvalidMarketConfig
            );
        } else {
            require!(
                self.quote_account == Pubkey::default()
                    && self.quote_feed_id == [0; 32]
                    && self.max_publish_skew_seconds == 0,
                ErrorCode::InvalidMarketConfig
            );
        }
        Ok(())
    }

    pub(crate) fn read(
        self,
        accounts: &[AccountInfo],
        clock: &Clock,
        base_mint: Pubkey,
        quote_mint: Pubkey,
    ) -> Result<OracleObservation> {
        let base = self.read_source(accounts, self.base_account, self.base_feed_id)?;
        let quote = if self.two_feeds() {
            let quote = self.read_source(accounts, self.quote_account, self.quote_feed_id)?;
            self.validate_age(quote.publish_time, quote.posted_slot, clock)?;
            require!(
                base.publish_time.abs_diff(quote.publish_time) <= u64::from(self.max_publish_skew_seconds),
                ErrorCode::OracleInvalidObservation
            );
            if self.format == NORMALIZED_RATIO_V4 {
                require!(
                    base.base_mint == base_mint
                        && quote.base_mint == quote_mint
                        && base.quote_mint != Pubkey::default()
                        && base.quote_mint == quote.quote_mint,
                    ErrorCode::OracleAuthenticationFailed
                );
            }
            Some(quote)
        } else {
            if self.format == NORMALIZED_ORACLE_V1 {
                require_keys_eq!(base.base_mint, base_mint, ErrorCode::OracleAuthenticationFailed);
                require_keys_eq!(base.quote_mint, quote_mint, ErrorCode::OracleAuthenticationFailed);
            }
            None
        };
        self.validate_age(base.publish_time, base.posted_slot, clock)?;
        // A direct report is already denominated in the market quote asset.
        // Two asset reports must share a denomination; no stablecoin peg is assumed.
        let denominator = quote.map_or(1, |q| q.price);
        let denominator_confidence = quote.map_or(0, |q| q.confidence);
        let exponent = base.exponent - quote.map_or(0, |q| q.exponent);
        let price = ratio_nad(base.price, denominator, exponent, false)?;
        let lower = ratio_nad(
            base.price - base.confidence,
            denominator
                .checked_add(denominator_confidence)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            exponent,
            false,
        )?;
        let upper = ratio_nad(
            base.price
                .checked_add(base.confidence)
                .ok_or(ErrorCode::MarketMathOverflow)?,
            denominator - denominator_confidence,
            exponent,
            true,
        )?;
        let confidence = price.abs_diff(lower).max(price.abs_diff(upper));
        require!(price > 0 && confidence < price, ErrorCode::OracleInvalidObservation);
        require!(
            u128::from(confidence) * 10_000 <= u128::from(price) * u128::from(self.max_confidence_bps),
            ErrorCode::OracleInvalidObservation
        );
        Ok(OracleObservation {
            price_nad: price,
            confidence_nad: confidence,
            publish_time: quote.map_or(base.publish_time, |q| base.publish_time.min(q.publish_time)),
            posted_slot: quote.map_or(base.posted_slot, |q| base.posted_slot.min(q.posted_slot)),
            sequence: quote.map_or(base.sequence, |q| base.sequence.min(q.sequence)),
            hash: quote.map_or(base.hash, |q| hashv(&[&base.hash, &q.hash]).to_bytes()),
            source_times: [base.publish_time, quote.map_or(0, |q| q.publish_time)],
            source_slots: [base.posted_slot, quote.map_or(0, |q| q.posted_slot)],
            source_sequences: [base.sequence, quote.map_or(0, |q| q.sequence)],
            source_hashes: [base.hash, quote.map_or([0; 32], |q| q.hash)],
        })
    }

    fn two_feeds(self) -> bool {
        matches!(self.format, PYTH_RATIO_V2 | NORMALIZED_RATIO_V4)
    }

    fn read_source(self, accounts: &[AccountInfo], key: Pubkey, id: [u8; 32]) -> Result<OracleSource> {
        let account = accounts
            .iter()
            .find(|a| *a.key == key)
            .ok_or(ErrorCode::OracleAccountMissing)?;
        require_keys_eq!(*account.owner, self.provider, ErrorCode::OracleAuthenticationFailed);
        require!(!account.executable, ErrorCode::OracleAuthenticationFailed);
        let data = account.try_borrow_data()?;
        let hash = hashv(&[account.key.as_ref(), &data]).to_bytes();
        let source = match self.format {
            NORMALIZED_ORACLE_V1 | NORMALIZED_RATIO_V4 => {
                let feed =
                    NormalizedOracleV1::try_from_slice(&data).map_err(|_| ErrorCode::OracleAuthenticationFailed)?;
                require!(
                    feed.magic == NORMALIZED_ORACLE_MAGIC && feed.feed_id == id,
                    ErrorCode::OracleAuthenticationFailed
                );
                require!(
                    feed.status == 1 && feed.sequence > 0,
                    ErrorCode::OracleInvalidObservation
                );
                OracleSource {
                    price: feed.price_nad,
                    confidence: feed.confidence_nad,
                    exponent: -9,
                    publish_time: feed.publish_time,
                    posted_slot: feed.publish_slot,
                    sequence: feed.sequence,
                    base_mint: feed.base_mint,
                    quote_mint: feed.quote_mint,
                    hash,
                }
            }
            PYTH_RATIO_V2 | PYTH_DIRECT_V3 => {
                require_keys_eq!(self.provider, PYTH_RECEIVER, ErrorCode::OracleAuthenticationFailed);
                // Pinned PriceUpdateV2 wire layout. Full verification is mandatory.
                // https://github.com/pyth-network/pyth-crosschain/blob/main/target_chains/solana/pyth_solana_receiver_sdk/src/price_update.rs
                require!(
                    data.len() == 133 || data.len() == 134,
                    ErrorCode::OracleAuthenticationFailed
                );
                require!(
                    data[..8] == hashv(&[b"account:PriceUpdateV2"]).to_bytes()[..8]
                        && data[40] == 1
                        && data[41..73] == id,
                    ErrorCode::OracleAuthenticationFailed
                );
                let i64_at = |offset: usize| i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                let price = u64::try_from(i64_at(73)).map_err(|_| ErrorCode::OracleInvalidObservation)?;
                let exponent = i32::from_le_bytes(data[89..93].try_into().unwrap());
                require!((-18..=18).contains(&exponent), ErrorCode::OracleInvalidObservation);
                let publish_time = i64_at(93);
                OracleSource {
                    price,
                    confidence: u64::from_le_bytes(data[81..89].try_into().unwrap()),
                    exponent,
                    publish_time,
                    posted_slot: u64::from_le_bytes(data[125..133].try_into().unwrap()),
                    sequence: u64::try_from(publish_time).map_err(|_| ErrorCode::OracleInvalidObservation)?,
                    base_mint: Pubkey::default(),
                    quote_mint: Pubkey::default(),
                    hash,
                }
            }
            _ => return err!(ErrorCode::OracleAuthenticationFailed),
        };
        require!(
            source.price > 0 && source.confidence < source.price,
            ErrorCode::OracleInvalidObservation
        );
        Ok(source)
    }

    fn validate_age(self, time: i64, slot: u64, clock: &Clock) -> Result<()> {
        require!(
            time > 0 && time <= clock.unix_timestamp && slot <= clock.slot,
            ErrorCode::OracleInvalidObservation
        );
        require!(
            clock.unix_timestamp - time <= i64::from(self.max_age_seconds) && clock.slot - slot <= self.max_age_slots,
            ErrorCode::OracleStale
        );
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct OracleSource {
    price: u64,
    confidence: u64,
    exponent: i32,
    publish_time: i64,
    posted_slot: u64,
    sequence: u64,
    base_mint: Pubkey,
    quote_mint: Pubkey,
    hash: [u8; 32],
}

/// Provider-owned ABI for a directly denominated base/quote report. A provider
/// or adapter may write this format; Dusk never accepts a caller-owned copy.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct NormalizedOracleV1 {
    pub magic: [u8; 8],
    pub feed_id: [u8; 32],
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub price_nad: u64,
    pub confidence_nad: u64,
    /// Underlying observation time, never adapter refresh time.
    pub publish_time: i64,
    pub publish_slot: u64,
    pub sequence: u64,
    /// 1=open/valid; every other value rejects price-sensitive execution.
    pub status: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct OracleObservation {
    pub price_nad: u64,
    pub confidence_nad: u64,
    pub publish_time: i64,
    pub posted_slot: u64,
    pub sequence: u64,
    pub hash: [u8; 32],
    pub source_times: [i64; 2],
    pub source_slots: [u64; 2],
    /// Independent counters retain replay protection when only one leg updates.
    pub source_sequences: [u64; 2],
    /// Each normalized report is immutable at an already observed counter.
    pub source_hashes: [[u8; 32]; 2],
}
impl OracleObservation {
    pub(crate) fn assert_not_before(self, previous: Self, format: u8) -> Result<()> {
        require!(self.sequence >= previous.sequence, ErrorCode::OracleReplay);
        for i in 0..2 {
            require!(
                self.source_times[i] >= previous.source_times[i] && self.source_slots[i] >= previous.source_slots[i],
                ErrorCode::OracleReplay
            );
            require!(
                self.source_sequences[i] >= previous.source_sequences[i],
                ErrorCode::OracleReplay
            );
            if matches!(format, NORMALIZED_ORACLE_V1 | NORMALIZED_RATIO_V4)
                && self.source_sequences[i] == previous.source_sequences[i]
            {
                require!(
                    self.source_hashes[i] == previous.source_hashes[i],
                    ErrorCode::OracleReplay
                );
            }
        }
        Ok(())
    }
}

fn ratio_nad(base: u64, quote: u64, exponent: i32, up: bool) -> Result<u64> {
    require!(quote > 0, ErrorCode::OracleInvalidObservation);
    let scale = 10_u128
        .checked_pow(exponent.unsigned_abs())
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let divide = if up { mul_div_ceil_u128 } else { mul_div_u128 };
    let value = if exponent >= 0 {
        divide(u128::from(base) * u128::from(NAD), scale, u128::from(quote))?
    } else {
        let unscaled = divide(u128::from(base), u128::from(NAD), u128::from(quote))?;
        if up {
            unscaled.div_ceil(scale)
        } else {
            unscaled / scale
        }
    };
    u64::try_from(value).map_err(|_| ErrorCode::MarketMathOverflow.into())
}

/// Persisted policy and replay protection. `runtime` occupies zero wire bytes
/// and is reset on every account deserialization, including a second CPI.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Default, InitSpace)]
pub struct OracleState {
    pub policy: OraclePolicy,
    pub last_observation: OracleObservation,
    pub revision: u64,
    /// Recognized native-tail depth cannot grow merely because an OP trade
    /// moves to a different native invariant. Real ordinary LP capital scales
    /// the ceiling through its share supply; losses reduce it immediately.
    pub recognized_tail_liquidity: u128,
    pub recognized_ordinary_supply: u64,
    pub runtime: RuntimePricing,
}
impl Space for Box<OracleState> {
    const INIT_SPACE: usize = OracleState::INIT_SPACE;
}

/// An instruction-local validated capability. It is never recoverable from
/// serialized market bytes, even in the same slot. Every price-sensitive
/// instruction must authenticate the configured on-chain accounts itself.
#[derive(Clone, Default)]
pub struct RuntimePricing {
    // Native account reads allocate no price context. A bound context is
    // shared only by clones within this invocation and has zero wire bytes.
    bound: Option<std::rc::Rc<RuntimeOracleContext>>,
}
type GeometryMemo = std::rc::Rc<
    std::cell::RefCell<
        Option<(
            crate::state::ConcentratedCurveCache,
            crate::transitions::amm::ConcentratedCurveGeometry,
        )>,
    >,
>;
struct RuntimeOracleContext {
    observation: OracleObservation,
    policy: OraclePolicy,
    curve: std::cell::RefCell<Option<RuntimeCurve>>,
    geometry: std::cell::OnceCell<GeometryMemo>,
}
#[derive(Clone)]
struct RuntimeCurve {
    parameters: crate::transitions::amm::ConcentratedCurveParameters,
    center: u64,
    policy: OraclePolicy,
    curve: std::rc::Rc<crate::math::op_gamm::BoundedOracleCurve>,
}
impl Space for RuntimePricing {
    const INIT_SPACE: usize = 0;
}
impl AnchorSerialize for RuntimePricing {
    fn serialize<W: std::io::Write>(&self, _: &mut W) -> std::io::Result<()> {
        Ok(())
    }
}
impl AnchorDeserialize for RuntimePricing {
    fn deserialize_reader<R: std::io::Read>(_: &mut R) -> std::io::Result<Self> {
        Ok(Self::default())
    }
}
#[cfg(feature = "idl-build")]
#[derive(AnchorSerialize, AnchorDeserialize)]
struct RuntimePricingWire {}
#[cfg(feature = "idl-build")]
impl anchor_lang::idl::IdlBuild for RuntimePricing {
    fn create_type() -> Option<anchor_lang::idl::types::IdlTypeDef> {
        <RuntimePricingWire as anchor_lang::idl::IdlBuild>::create_type().map(|mut value| {
            value.name = Self::get_full_path();
            value
        })
    }
}
impl RuntimePricing {
    /// Memoize only pure geometry, keyed by every serialized cache field.
    /// This is neither an observation nor permission to price a transaction.
    #[inline(always)]
    pub(crate) fn geometry(
        &self,
        cache: crate::state::ConcentratedCurveCache,
    ) -> Result<crate::transitions::amm::ConcentratedCurveGeometry> {
        let Some(bound) = self.bound.as_ref() else {
            return cache.geometry();
        };
        let memo = bound.geometry.get_or_init(Default::default);
        if let Some((_, geometry)) = memo.borrow().as_ref().filter(|(key, _)| *key == cache) {
            return Ok(*geometry);
        }
        let geometry = cache.geometry()?;
        self.remember_geometry(cache, geometry);
        Ok(geometry)
    }

    pub(crate) fn remember_geometry(
        &self,
        cache: crate::state::ConcentratedCurveCache,
        geometry: crate::transitions::amm::ConcentratedCurveGeometry,
    ) {
        if let Some(bound) = self.bound.as_ref() {
            *bound.geometry.get_or_init(Default::default).borrow_mut() = Some((cache, geometry));
        }
    }

    pub(crate) fn bind(policy: OraclePolicy, observation: OracleObservation) -> Self {
        Self {
            bound: Some(std::rc::Rc::new(RuntimeOracleContext {
                observation,
                policy,
                curve: Default::default(),
                geometry: Default::default(),
            })),
        }
    }
    pub(crate) fn observation(&self) -> Result<OracleObservation> {
        self.bound
            .as_ref()
            .map(|bound| bound.observation)
            .ok_or_else(|| ErrorCode::OracleAccountMissing.into())
    }
    pub(crate) fn curve(
        &self,
        parameters: crate::transitions::amm::ConcentratedCurveParameters,
        center: u64,
        policy: OraclePolicy,
    ) -> Result<std::rc::Rc<crate::math::op_gamm::BoundedOracleCurve>> {
        let bound = self.bound.as_ref().ok_or(ErrorCode::OracleAccountMissing)?;
        require!(bound.policy == policy, ErrorCode::OracleAuthenticationFailed);
        let observation = bound.observation;
        let memo = &bound.curve;
        if let Some(cached) = memo.borrow().as_ref() {
            if cached.parameters == parameters && cached.center == center && cached.policy == policy {
                return Ok(cached.curve.clone());
            }
        }
        let curve = std::rc::Rc::new(crate::math::op_gamm::BoundedOracleCurve::new(
            parameters,
            center,
            observation.price_nad,
            policy.lambda_bps,
            policy.adjustment_cap_nad,
        )?);
        *memo.borrow_mut() = Some(RuntimeCurve {
            parameters,
            center,
            policy,
            curve: curve.clone(),
        });
        Ok(curve)
    }
}

#[cfg(test)]
mod tests {
    include!("tests/state/oracle.rs");
}
