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
pub const NORMALIZED_ORACLE_MAGIC: [u8; 8] = *b"DOPRV001";
pub const PYTH_RECEIVER: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");

/// Immutable creator disclosure. Zero weight makes oracle activation
/// impossible; positive bounds allow prospective parameter governance.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, InitSpace, PartialEq, Eq)]
pub struct OracleBounds {
    pub max_lambda_bps: u16,
    pub max_adjustment_nad: u64,
}
impl OracleBounds {
    pub fn validate(self) -> Result<()> {
        require!(self.max_lambda_bps < 10_000, ErrorCode::InvalidMarketConfig);
        require!(
            (self.max_lambda_bps == 0 && self.max_adjustment_nad == 0)
                || (self.max_lambda_bps > 0 && self.max_adjustment_nad > NAD),
            ErrorCode::InvalidMarketConfig
        );
        Ok(())
    }
}

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

    pub fn validate(self, bounds: OracleBounds) -> Result<()> {
        bounds.validate()?;
        require!(self.lambda_bps <= bounds.max_lambda_bps, ErrorCode::InvalidMarketConfig);
        if self == Self::default() {
            return Ok(());
        }
        require!(
            bounds.max_lambda_bps > 0
                && self.adjustment_cap_nad > NAD
                && self.adjustment_cap_nad <= bounds.max_adjustment_nad,
            ErrorCode::InvalidMarketConfig
        );
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
        match self.format {
            NORMALIZED_ORACLE_V1 => require!(
                self.quote_account == Pubkey::default()
                    && self.quote_feed_id == [0; 32]
                    && self.max_publish_skew_seconds == 0,
                ErrorCode::InvalidMarketConfig
            ),
            PYTH_RATIO_V2 => require!(
                self.provider == PYTH_RECEIVER
                    && self.quote_account != Pubkey::default()
                    && self.quote_feed_id != [0; 32]
                    && self.base_account != self.quote_account
                    && self.base_feed_id != self.quote_feed_id,
                ErrorCode::InvalidMarketConfig
            ),
            _ => return err!(ErrorCode::InvalidMarketConfig),
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
        let primary = accounts
            .iter()
            .find(|a| *a.key == self.base_account)
            .ok_or(ErrorCode::OracleAccountMissing)?;
        require_keys_eq!(*primary.owner, self.provider, ErrorCode::OracleAuthenticationFailed);
        require!(!primary.executable, ErrorCode::OracleAuthenticationFailed);
        let primary_data = primary.try_borrow_data()?;
        let (price, confidence, publish_time, posted_slot, sequence, hash, source_times, source_slots) =
            match self.format {
                NORMALIZED_ORACLE_V1 => {
                    let feed = NormalizedOracleV1::try_from_slice(&primary_data)
                        .map_err(|_| ErrorCode::OracleAuthenticationFailed)?;
                    require!(
                        feed.magic == NORMALIZED_ORACLE_MAGIC && feed.feed_id == self.base_feed_id,
                        ErrorCode::OracleAuthenticationFailed
                    );
                    require_keys_eq!(feed.base_mint, base_mint, ErrorCode::OracleAuthenticationFailed);
                    require_keys_eq!(feed.quote_mint, quote_mint, ErrorCode::OracleAuthenticationFailed);
                    require!(
                        feed.status == 1 && feed.sequence > 0,
                        ErrorCode::OracleInvalidObservation
                    );
                    (
                        feed.price_nad,
                        feed.confidence_nad,
                        feed.publish_time,
                        feed.publish_slot,
                        feed.sequence,
                        hashv(&[primary.key.as_ref(), &primary_data]).to_bytes(),
                        [feed.publish_time, 0],
                        [feed.publish_slot, 0],
                    )
                }
                PYTH_RATIO_V2 => {
                    require_keys_eq!(self.provider, PYTH_RECEIVER, ErrorCode::OracleAuthenticationFailed);
                    let secondary = accounts
                        .iter()
                        .find(|a| *a.key == self.quote_account)
                        .ok_or(ErrorCode::OracleAccountMissing)?;
                    require_keys_eq!(*secondary.owner, PYTH_RECEIVER, ErrorCode::OracleAuthenticationFailed);
                    require!(!secondary.executable, ErrorCode::OracleAuthenticationFailed);
                    let secondary_data = secondary.try_borrow_data()?;
                    let base = PythPrice::decode(&primary_data, self.base_feed_id)?;
                    let quote = PythPrice::decode(&secondary_data, self.quote_feed_id)?;
                    self.validate_age(base.publish_time, base.posted_slot, clock)?;
                    self.validate_age(quote.publish_time, quote.posted_slot, clock)?;
                    require!(
                        base.publish_time.abs_diff(quote.publish_time) <= u64::from(self.max_publish_skew_seconds),
                        ErrorCode::OracleInvalidObservation
                    );
                    let price = ratio_nad(base.price, quote.price, base.exponent - quote.exponent, false)?;
                    let lower = ratio_nad(
                        base.price - base.confidence,
                        quote
                            .price
                            .checked_add(quote.confidence)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                        base.exponent - quote.exponent,
                        false,
                    )?;
                    let upper = ratio_nad(
                        base.price
                            .checked_add(base.confidence)
                            .ok_or(ErrorCode::MarketMathOverflow)?,
                        quote.price - quote.confidence,
                        base.exponent - quote.exponent,
                        true,
                    )?;
                    let confidence = price.abs_diff(lower).max(price.abs_diff(upper));
                    let time = base.publish_time.min(quote.publish_time);
                    (
                        price,
                        confidence,
                        time,
                        base.posted_slot.min(quote.posted_slot),
                        u64::try_from(time).map_err(|_| ErrorCode::OracleInvalidObservation)?,
                        hashv(&[
                            primary.key.as_ref(),
                            &primary_data,
                            secondary.key.as_ref(),
                            &secondary_data,
                        ])
                        .to_bytes(),
                        [base.publish_time, quote.publish_time],
                        [base.posted_slot, quote.posted_slot],
                    )
                }
                _ => return err!(ErrorCode::OracleAuthenticationFailed),
            };
        self.validate_age(publish_time, posted_slot, clock)?;
        require!(price > 0 && confidence < price, ErrorCode::OracleInvalidObservation);
        require!(
            u128::from(confidence) * 10_000 <= u128::from(price) * u128::from(self.max_confidence_bps),
            ErrorCode::OracleInvalidObservation
        );
        Ok(OracleObservation {
            price_nad: price,
            confidence_nad: confidence,
            publish_time,
            posted_slot,
            sequence,
            hash,
            source_times,
            source_slots,
        })
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
}
impl OracleObservation {
    pub(crate) fn assert_not_before(self, previous: Self, format: u8) -> Result<()> {
        require!(self.sequence >= previous.sequence, ErrorCode::OracleReplay);
        for i in 0..2 {
            require!(
                self.source_times[i] >= previous.source_times[i] && self.source_slots[i] >= previous.source_slots[i],
                ErrorCode::OracleReplay
            );
        }
        if format == NORMALIZED_ORACLE_V1 && self.sequence == previous.sequence {
            require!(self.hash == previous.hash, ErrorCode::OracleReplay);
        }
        Ok(())
    }
}

// Pinned PriceUpdateV2 wire layout; full verification is mandatory. Upstream:
// https://github.com/pyth-network/pyth-crosschain/blob/main/target_chains/solana/pyth_solana_receiver_sdk/src/price_update.rs
// Parsing the small wire record avoids pulling a second Anchor/Solana version
// into Dusk. Tests cover the full and partial encodings and field offsets.
struct PythPrice {
    price: u64,
    confidence: u64,
    exponent: i32,
    publish_time: i64,
    posted_slot: u64,
}
impl PythPrice {
    fn decode(data: &[u8], id: [u8; 32]) -> Result<Self> {
        require!(
            data.len() == 133 || data.len() == 134,
            ErrorCode::OracleAuthenticationFailed
        );
        require!(
            data[..8] == hashv(&[b"account:PriceUpdateV2"]).to_bytes()[..8] && data[40] == 1 && data[41..73] == id,
            ErrorCode::OracleAuthenticationFailed
        );
        let i64_at = |offset: usize| i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        let price = u64::try_from(i64_at(73)).map_err(|_| ErrorCode::OracleInvalidObservation)?;
        let confidence = u64::from_le_bytes(data[81..89].try_into().unwrap());
        let exponent = i32::from_le_bytes(data[89..93].try_into().unwrap());
        require!(
            price > 0 && confidence < price && (-18..=18).contains(&exponent),
            ErrorCode::OracleInvalidObservation
        );
        Ok(Self {
            price,
            confidence,
            exponent,
            publish_time: i64_at(93),
            posted_slot: u64::from_le_bytes(data[125..133].try_into().unwrap()),
        })
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
