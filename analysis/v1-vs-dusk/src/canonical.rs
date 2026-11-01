//! Canonical, versioned records for the V1 historical replay dataset.
//!
//! Raw token quantities are deliberately decimal strings. This prevents JSON,
//! CSV, JavaScript, or database clients from silently rounding `u64`/`u128`
//! protocol values. Chain time and transaction order are required on canonical
//! events; database ingestion timestamps never enter this schema.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt::{self, Display, Formatter},
    str::FromStr,
};

pub const CANONICAL_SCHEMA_VERSION: u16 = 1;
const EVENT_ID_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:event-id:v1";
const DATASET_ID_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:dataset-id:v1";
const BLOCK_MANIFEST_DOMAIN: &[u8] = b"omnipair:v1-vs-dusk:block-manifest:v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegerStringError {
    Empty,
    NonDecimal,
    LeadingZero,
    NegativeUnsigned,
    NegativeZero,
}

impl Display for IntegerStringError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "integer string is empty",
            Self::NonDecimal => "integer string contains a non-decimal character",
            Self::LeadingZero => "integer string has a non-canonical leading zero",
            Self::NegativeUnsigned => "unsigned token amount cannot be negative",
            Self::NegativeZero => "signed token amount cannot be negative zero",
        };
        f.write_str(message)
    }
}

impl Error for IntegerStringError {}

fn validate_unsigned(value: &str) -> Result<(), IntegerStringError> {
    if value.is_empty() {
        return Err(IntegerStringError::Empty);
    }
    if value.starts_with('-') {
        return Err(IntegerStringError::NegativeUnsigned);
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(IntegerStringError::NonDecimal);
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(IntegerStringError::LeadingZero);
    }
    Ok(())
}

fn validate_signed(value: &str) -> Result<(), IntegerStringError> {
    if value.is_empty() {
        return Err(IntegerStringError::Empty);
    }
    let digits = value.strip_prefix('-').unwrap_or(value);
    validate_unsigned(digits)?;
    if value.starts_with('-') && digits == "0" {
        return Err(IntegerStringError::NegativeZero);
    }
    Ok(())
}

/// A non-negative raw token or share quantity encoded as canonical base-10.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TokenAmount(String);

impl TokenAmount {
    pub fn new(value: impl Into<String>) -> Result<Self, IntegerStringError> {
        let value = value.into();
        validate_unsigned(&value)?;
        Ok(Self(value))
    }

    pub fn zero() -> Self {
        Self("0".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TokenAmount {
    type Error = IntegerStringError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<TokenAmount> for String {
    fn from(value: TokenAmount) -> Self {
        value.0
    }
}

impl FromStr for TokenAmount {
    type Err = IntegerStringError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Display for TokenAmount {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A signed raw token delta encoded as canonical base-10.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SignedTokenAmount(String);

impl SignedTokenAmount {
    pub fn new(value: impl Into<String>) -> Result<Self, IntegerStringError> {
        let value = value.into();
        validate_signed(&value)?;
        Ok(Self(value))
    }

    pub fn zero() -> Self {
        Self("0".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SignedTokenAmount {
    type Error = IntegerStringError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SignedTokenAmount> for String {
    fn from(value: SignedTokenAmount) -> Self {
        value.0
    }
}

impl FromStr for SignedTokenAmount {
    type Err = IntegerStringError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Display for SignedTokenAmount {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecimalStringError {
    Empty,
    Negative,
    NonDecimal,
    LeadingZero,
    EmptyFraction,
}

impl Display for DecimalStringError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "decimal string is empty",
            Self::Negative => "unsigned decimal string cannot be negative",
            Self::NonDecimal => "decimal string is not fixed-point base-10",
            Self::LeadingZero => "decimal string has a non-canonical leading zero",
            Self::EmptyFraction => "decimal string has an empty fractional part",
        };
        f.write_str(message)
    }
}

impl Error for DecimalStringError {}

fn validate_unsigned_decimal(value: &str) -> Result<(), DecimalStringError> {
    if value.is_empty() {
        return Err(DecimalStringError::Empty);
    }
    if value.starts_with('-') {
        return Err(DecimalStringError::Negative);
    }
    let mut parts = value.split('.');
    let integer = parts.next().ok_or(DecimalStringError::Empty)?;
    let fraction = parts.next();
    if parts.next().is_some()
        || integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(DecimalStringError::NonDecimal);
    }
    if integer.len() > 1 && integer.starts_with('0') {
        return Err(DecimalStringError::LeadingZero);
    }
    if let Some(fraction) = fraction {
        if fraction.is_empty() {
            return Err(DecimalStringError::EmptyFraction);
        }
        if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(DecimalStringError::NonDecimal);
        }
    }
    Ok(())
}

/// An exact, non-negative fixed-point decimal encoded without exponent notation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExactDecimal(String);

impl ExactDecimal {
    pub fn new(value: impl Into<String>) -> Result<Self, DecimalStringError> {
        let value = value.into();
        validate_unsigned_decimal(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0
            .bytes()
            .filter(|byte| *byte != b'.')
            .all(|byte| byte == b'0')
    }
}

impl TryFrom<String> for ExactDecimal {
    type Error = DecimalStringError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ExactDecimal> for String {
    fn from(value: ExactDecimal) -> Self {
        value.0
    }
}

impl FromStr for ExactDecimal {
    type Err = DecimalStringError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Display for ExactDecimal {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoricalPriceQuality {
    Historical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoricalPriceMarkV1 {
    pub mint: String,
    /// Provider observation bucket, not Solana chain time.
    pub bucket_unix_seconds: i64,
    pub price_usd: ExactDecimal,
    pub token_decimals: Option<u8>,
    pub provider: String,
    pub quality: HistoricalPriceQuality,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoricalPriceMarkError {
    EmptyMint,
    EmptyProvider,
    NonHourlyBucket,
    NonPositivePrice,
}

impl Display for HistoricalPriceMarkError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyMint => "historical price mint is empty",
            Self::EmptyProvider => "historical price provider is empty",
            Self::NonHourlyBucket => "historical price bucket is not hour-aligned",
            Self::NonPositivePrice => "historical price must be positive",
        };
        f.write_str(message)
    }
}

impl Error for HistoricalPriceMarkError {}

impl HistoricalPriceMarkV1 {
    pub fn validate(&self) -> Result<(), HistoricalPriceMarkError> {
        if self.mint.is_empty() {
            return Err(HistoricalPriceMarkError::EmptyMint);
        }
        if self.provider.is_empty() {
            return Err(HistoricalPriceMarkError::EmptyProvider);
        }
        if self.bucket_unix_seconds.rem_euclid(3_600) != 0 {
            return Err(HistoricalPriceMarkError::NonHourlyBucket);
        }
        if self.price_usd.is_zero() {
            return Err(HistoricalPriceMarkError::NonPositivePrice);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema_version", content = "record")]
pub enum VersionedHistoricalPriceMark {
    #[serde(rename = "1")]
    V1(HistoricalPriceMarkV1),
}

/// Total order for one decoded event in Solana execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChainOrder {
    pub slot: u64,
    pub transaction_index: u32,
    pub outer_instruction_index: u32,
    /// Monotonic position in `meta.innerInstructions` execution order.
    /// `None` denotes the top-level instruction itself.
    pub inner_execution_index: Option<u32>,
    /// Distinguishes multiple events emitted at the same instruction location.
    pub event_sequence: u32,
}

impl ChainOrder {
    fn update_digest(&self, digest: &mut Sha256) {
        digest.update(self.slot.to_be_bytes());
        digest.update(self.transaction_index.to_be_bytes());
        digest.update(self.outer_instruction_index.to_be_bytes());
        match self.inner_execution_index {
            Some(index) => {
                digest.update([1]);
                digest.update(index.to_be_bytes());
            }
            None => digest.update([0]),
        }
        digest.update(self.event_sequence.to_be_bytes());
    }
}

fn digest_field(digest: &mut Sha256, value: &str) {
    let bytes = value.as_bytes();
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

/// Stable across decoder changes because identity depends only on chain location.
pub fn deterministic_event_id(
    cluster: &str,
    program_id: &str,
    signature: &str,
    order: ChainOrder,
) -> String {
    let mut digest = Sha256::new();
    digest.update(EVENT_ID_DOMAIN);
    digest_field(&mut digest, cluster);
    digest_field(&mut digest, program_id);
    digest_field(&mut digest, signature);
    order.update_digest(&mut digest);
    encode_hex(&digest.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiquidityAction {
    Mint,
    Burn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapPayloadV1 {
    pub token0_in: bool,
    pub amount_in: TokenAmount,
    pub amount_out: TokenAmount,
    pub amount_in_after_fee: TokenAmount,
    pub lp_fee: TokenAmount,
    pub protocol_fee: TokenAmount,
    pub reserve0_after: TokenAmount,
    pub reserve1_after: TokenAmount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiquidityPayloadV1 {
    pub action: LiquidityAction,
    pub amount0: TokenAmount,
    pub amount1: TokenAmount,
    pub liquidity: TokenAmount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpPositionPayloadV1 {
    pub lp_amount: TokenAmount,
    pub amount0: TokenAmount,
    pub amount1: TokenAmount,
    pub cash_reserve0: Option<TokenAmount>,
    pub cash_reserve1: Option<TokenAmount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairDeltaPayloadV1 {
    pub amount0: SignedTokenAmount,
    pub amount1: SignedTokenAmount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionPayloadV1 {
    pub position: String,
    pub collateral0: TokenAmount,
    pub collateral1: TokenAmount,
    pub debt0_shares: TokenAmount,
    pub debt1_shares: TokenAmount,
    pub collateral0_max_cf_bps: u16,
    pub collateral1_max_cf_bps: u16,
    pub collateral0_liquidation_cf_bps: u16,
    pub collateral1_liquidation_cf_bps: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiquidationPayloadV1 {
    pub position: String,
    pub liquidator: String,
    pub collateral0_liquidated: TokenAmount,
    pub collateral1_liquidated: TokenAmount,
    pub debt0_liquidated: TokenAmount,
    pub debt1_liquidated: TokenAmount,
    pub collateral_price_nad: TokenAmount,
    pub shortfall: TokenAmount,
    pub liquidation_bonus: TokenAmount,
    pub k_before: TokenAmount,
    pub k_after: TokenAmount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairUpdatePayloadV1 {
    pub price0_ema_nad: TokenAmount,
    pub price1_ema_nad: TokenAmount,
    pub rate0_nad: TokenAmount,
    pub rate1_nad: TokenAmount,
    pub accrued_interest0: TokenAmount,
    pub accrued_interest1: TokenAmount,
    pub lp_interest0: TokenAmount,
    pub lp_interest1: TokenAmount,
    pub protocol_interest0: TokenAmount,
    pub protocol_interest1: TokenAmount,
    pub cash_reserve0: TokenAmount,
    pub cash_reserve1: TokenAmount,
    pub reserve0_after_interest: TokenAmount,
    pub reserve1_after_interest: TokenAmount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashloanPayloadV1 {
    pub amount0: TokenAmount,
    pub amount1: TokenAmount,
    pub fee0: TokenAmount,
    pub fee1: TokenAmount,
    pub receiver: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolFeeClaimPayloadV1 {
    pub token0: String,
    pub token1: String,
    pub futarchy_treasury_amount0: TokenAmount,
    pub futarchy_treasury_amount1: TokenAmount,
    pub buybacks_vault_amount0: TokenAmount,
    pub buybacks_vault_amount1: TokenAmount,
    pub team_treasury_amount0: TokenAmount,
    pub team_treasury_amount1: TokenAmount,
}

/// Decoder-independent payload variants needed by replay and revenue accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum EventPayloadV1 {
    Swap(SwapPayloadV1),
    Liquidity(LiquidityPayloadV1),
    LpPosition(LpPositionPayloadV1),
    CollateralDelta(PairDeltaPayloadV1),
    DebtDelta(PairDeltaPayloadV1),
    Position(PositionPayloadV1),
    Liquidation(LiquidationPayloadV1),
    PairUpdate(PairUpdatePayloadV1),
    Flashloan(FlashloanPayloadV1),
    ProtocolFeeClaim(ProtocolFeeClaimPayloadV1),
    /// Initialization and administrative instructions use canonical string fields.
    Config(BTreeMap<String, String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbSourceRef {
    pub table: String,
    pub row_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalEventV1 {
    pub event_id: String,
    pub cluster: String,
    pub program_id: String,
    pub pair: Option<String>,
    pub actor: Option<String>,
    pub signature: String,
    pub order: ChainOrder,
    /// Indexer-compatible absolute CPI execution path proven by finalized RPC
    /// stack metadata. It is retained separately from the canonical total
    /// order so database instruction paths can be reconciled exactly.
    pub execution_path: Vec<u32>,
    /// Authoritative Solana block time. Never populated from a DB timestamp.
    pub block_time_unix_seconds: i64,
    pub decoder_version_id: String,
    pub raw_payload_sha256: String,
    pub db_source: Option<DbSourceRef>,
    pub payload: EventPayloadV1,
}

impl CanonicalEventV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cluster: String,
        program_id: String,
        pair: Option<String>,
        actor: Option<String>,
        signature: String,
        order: ChainOrder,
        execution_path: Vec<u32>,
        block_time_unix_seconds: i64,
        decoder_version_id: String,
        raw_payload: &[u8],
        db_source: Option<DbSourceRef>,
        payload: EventPayloadV1,
    ) -> Self {
        let event_id = deterministic_event_id(&cluster, &program_id, &signature, order);
        Self {
            event_id,
            cluster,
            program_id,
            pair,
            actor,
            signature,
            order,
            execution_path,
            block_time_unix_seconds,
            decoder_version_id,
            raw_payload_sha256: sha256_hex(raw_payload),
            db_source,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema_version", content = "record")]
pub enum VersionedCanonicalEvent {
    #[serde(rename = "1")]
    V1(CanonicalEventV1),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigScope {
    ProgramBinary,
    Pair,
    RateModel,
    ProtocolRevenue,
    RevenueRecipients,
    ReduceOnly,
}

/// Slowly-changing configuration with an exclusive upper order bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigVersionV1 {
    pub config_id: String,
    pub scope: ConfigScope,
    pub entity_address: String,
    pub valid_from: ChainOrder,
    pub valid_to_exclusive: Option<ChainOrder>,
    pub source_signature: String,
    pub decoder_version_id: String,
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema_version", content = "record")]
pub enum VersionedConfig {
    #[serde(rename = "1")]
    V1(ConfigVersionV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecoderVersion {
    pub version_id: String,
    pub valid_from_slot: u64,
    pub valid_to_slot_exclusive: Option<u64>,
    pub git_commit: String,
    pub idl_sha256: String,
    pub program_binary_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactChecksum {
    pub logical_name: String,
    pub byte_length: u64,
    pub sha256: String,
}

impl ArtifactChecksum {
    pub fn from_bytes(logical_name: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            logical_name: logical_name.into(),
            byte_length: bytes.len() as u64,
            sha256: sha256_hex(bytes),
        }
    }

    pub fn matches_bytes(&self, bytes: &[u8]) -> bool {
        self.byte_length == bytes.len() as u64 && self.sha256 == sha256_hex(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockArtifactChecksumV1 {
    pub slot: u64,
    pub blockhash: String,
    pub parent_slot: u64,
    pub previous_blockhash: String,
    pub transaction_count: u64,
    pub artifact: ArtifactChecksum,
}

impl BlockArtifactChecksumV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn from_bytes(
        slot: u64,
        blockhash: impl Into<String>,
        parent_slot: u64,
        previous_blockhash: impl Into<String>,
        transaction_count: u64,
        logical_name: impl Into<String>,
        bytes: &[u8],
    ) -> Self {
        Self {
            slot,
            blockhash: blockhash.into(),
            parent_slot,
            previous_blockhash: previous_blockhash.into(),
            transaction_count,
            artifact: ArtifactChecksum::from_bytes(logical_name, bytes),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockArtifactManifestV1 {
    pub schema_version: u16,
    pub finalized_slot_start: u64,
    pub finalized_slot_end: u64,
    pub manifest_sha256: String,
    pub blocks: Vec<BlockArtifactChecksumV1>,
}

impl BlockArtifactManifestV1 {
    pub fn new(
        finalized_slot_start: u64,
        finalized_slot_end: u64,
        mut blocks: Vec<BlockArtifactChecksumV1>,
    ) -> Result<Self, ManifestError> {
        if finalized_slot_start > finalized_slot_end {
            return Err(ManifestError::InvalidSlotRange);
        }
        blocks.sort_by_key(|block| block.slot);
        for (index, block) in blocks.iter().enumerate() {
            if block.slot < finalized_slot_start || block.slot > finalized_slot_end {
                return Err(ManifestError::BlockOutsideRange(block.slot));
            }
            if index > 0 && blocks[index - 1].slot == block.slot {
                return Err(ManifestError::DuplicateBlockSlot(block.slot));
            }
        }
        let mut manifest = Self {
            schema_version: CANONICAL_SCHEMA_VERSION,
            finalized_slot_start,
            finalized_slot_end,
            manifest_sha256: String::new(),
            blocks,
        };
        manifest.manifest_sha256 = manifest.compute_manifest_sha256();
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != CANONICAL_SCHEMA_VERSION {
            return Err(ManifestError::SchemaVersionMismatch);
        }
        if self.finalized_slot_start > self.finalized_slot_end {
            return Err(ManifestError::InvalidSlotRange);
        }
        for (index, block) in self.blocks.iter().enumerate() {
            if block.slot < self.finalized_slot_start || block.slot > self.finalized_slot_end {
                return Err(ManifestError::BlockOutsideRange(block.slot));
            }
            if index > 0 && self.blocks[index - 1].slot >= block.slot {
                return Err(ManifestError::DuplicateBlockSlot(block.slot));
            }
        }
        if self.manifest_sha256 != self.compute_manifest_sha256() {
            return Err(ManifestError::ManifestChecksumMismatch);
        }
        Ok(())
    }

    pub fn compute_manifest_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(BLOCK_MANIFEST_DOMAIN);
        digest.update(self.schema_version.to_be_bytes());
        digest.update(self.finalized_slot_start.to_be_bytes());
        digest.update(self.finalized_slot_end.to_be_bytes());
        for block in &self.blocks {
            digest.update(block.slot.to_be_bytes());
            digest_field(&mut digest, &block.blockhash);
            digest.update(block.parent_slot.to_be_bytes());
            digest_field(&mut digest, &block.previous_blockhash);
            digest.update(block.transaction_count.to_be_bytes());
            digest_field(&mut digest, &block.artifact.logical_name);
            digest.update(block.artifact.byte_length.to_be_bytes());
            digest_field(&mut digest, &block.artifact.sha256);
        }
        encode_hex(&digest.finalize())
    }

    pub fn verify_block_bytes(&self, slot: u64, bytes: &[u8]) -> bool {
        self.blocks
            .iter()
            .find(|block| block.slot == slot)
            .is_some_and(|block| block.artifact.matches_bytes(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema_version", content = "record")]
pub enum VersionedBlockArtifactManifest {
    #[serde(rename = "1")]
    V1(BlockArtifactManifestV1),
}

/// A source reference safe to serialize or log. It contains no URL or credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizedSource {
    pub kind: String,
    pub label: String,
    pub secret_fingerprint_sha256: String,
}

impl SanitizedSource {
    pub fn from_secret(
        kind: impl Into<String>,
        label: impl Into<String>,
        secret: &str,
    ) -> Result<Self, ManifestError> {
        let kind = kind.into();
        let label = label.into();
        if kind.contains("://")
            || label.contains("://")
            || label.contains('@')
            || label.contains('=')
        {
            return Err(ManifestError::UnsafeSourceLabel);
        }
        Ok(Self {
            kind,
            label,
            secret_fingerprint_sha256: sha256_hex(secret.as_bytes()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    UnsafeSourceLabel,
    InvalidSlotRange,
    SchemaVersionMismatch,
    BlockOutsideRange(u64),
    DuplicateBlockSlot(u64),
    ManifestChecksumMismatch,
}

impl Display for ManifestError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsafeSourceLabel => "manifest source label may contain connection material",
            Self::InvalidSlotRange => "manifest finalized slot range is invalid",
            Self::SchemaVersionMismatch => "manifest schema version is unsupported",
            Self::BlockOutsideRange(_) => "block artifact lies outside the manifest slot range",
            Self::DuplicateBlockSlot(_) => "block artifact slots are duplicated or unsorted",
            Self::ManifestChecksumMismatch => "block artifact manifest checksum does not match",
        };
        f.write_str(message)
    }
}

impl Error for ManifestError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetManifestV1 {
    pub schema_version: u16,
    pub dataset_id: String,
    /// Supplied by the caller as RFC3339 UTC; never inferred from source rows.
    pub generated_at_utc: String,
    pub cluster: String,
    pub program_id: String,
    pub finalized_slot_start: u64,
    pub finalized_slot_end: u64,
    pub sources: Vec<SanitizedSource>,
    pub decoder_versions: Vec<DecoderVersion>,
    pub artifacts: Vec<ArtifactChecksum>,
}

impl DatasetManifestV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        generated_at_utc: String,
        cluster: String,
        program_id: String,
        finalized_slot_start: u64,
        finalized_slot_end: u64,
        mut sources: Vec<SanitizedSource>,
        mut decoder_versions: Vec<DecoderVersion>,
        mut artifacts: Vec<ArtifactChecksum>,
    ) -> Result<Self, ManifestError> {
        if finalized_slot_start > finalized_slot_end {
            return Err(ManifestError::InvalidSlotRange);
        }
        sources.sort_by(|left, right| (&left.kind, &left.label).cmp(&(&right.kind, &right.label)));
        decoder_versions.sort_by_key(|version| version.valid_from_slot);
        artifacts.sort_by(|left, right| left.logical_name.cmp(&right.logical_name));
        let mut manifest = Self {
            schema_version: CANONICAL_SCHEMA_VERSION,
            dataset_id: String::new(),
            generated_at_utc,
            cluster,
            program_id,
            finalized_slot_start,
            finalized_slot_end,
            sources,
            decoder_versions,
            artifacts,
        };
        manifest.dataset_id = manifest.compute_dataset_id();
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != CANONICAL_SCHEMA_VERSION {
            return Err(ManifestError::SchemaVersionMismatch);
        }
        if self.finalized_slot_start > self.finalized_slot_end {
            return Err(ManifestError::InvalidSlotRange);
        }
        if self.sources.iter().any(|source| {
            source.kind.contains("://")
                || source.label.contains("://")
                || source.label.contains('@')
                || source.label.contains('=')
        }) {
            return Err(ManifestError::UnsafeSourceLabel);
        }
        Ok(())
    }

    pub fn compute_dataset_id(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(DATASET_ID_DOMAIN);
        digest.update(self.schema_version.to_be_bytes());
        digest_field(&mut digest, &self.cluster);
        digest_field(&mut digest, &self.program_id);
        digest.update(self.finalized_slot_start.to_be_bytes());
        digest.update(self.finalized_slot_end.to_be_bytes());
        for source in &self.sources {
            digest_field(&mut digest, &source.kind);
            digest_field(&mut digest, &source.label);
            digest_field(&mut digest, &source.secret_fingerprint_sha256);
        }
        for version in &self.decoder_versions {
            digest_field(&mut digest, &version.version_id);
            digest.update(version.valid_from_slot.to_be_bytes());
            digest_field(&mut digest, &version.idl_sha256);
        }
        for artifact in &self.artifacts {
            digest_field(&mut digest, &artifact.logical_name);
            digest.update(artifact.byte_length.to_be_bytes());
            digest_field(&mut digest, &artifact.sha256);
        }
        encode_hex(&digest.finalize())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema_version", content = "record")]
pub enum VersionedManifest {
    #[serde(rename = "1")]
    V1(DatasetManifestV1),
}
