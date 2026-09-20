pub const ORDER_SEED_PREFIX: &[u8] = b"leverage_order";
pub const EXECUTOR_INCENTIVE_BPS: u64 = 500;
pub const ORDER_KIND_TAKE_PROFIT: u8 = 1;
pub const ORDER_KIND_STOP_LOSS: u8 = 2;
pub const HLP_ORDER_SEED_PREFIX: &[u8] = b"hlp_order";
pub const HLP_ORDER_KIND_STOP_LOSS: u8 = 1;
pub const HLP_ORDER_KIND_STOP_RATE: u8 = 2;
pub const HLP_ORDER_STATUS_ACTIVE: u8 = 0;
pub const HLP_ORDER_STATUS_CANCELLED: u8 = 1;
pub const HLP_ORDER_STATUS_EXECUTED: u8 = 2;
pub const ENTRY_ORDER_SEED_PREFIX: &[u8] = b"leverage_entry_order";
/// Additional protocol service fee on successful voluntary order execution.
pub const ORDER_PROTOCOL_FEE_BPS: u64 = 10;

pub const PROTECTION_ORDER_SEED_PREFIX: &[u8] = b"protection_order";
