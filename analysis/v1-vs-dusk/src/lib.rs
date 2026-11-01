//! Event-exact V1 history and Dusk counterfactual benchmark.
//!
//! All public records are versioned, raw token amounts remain integer strings,
//! and no counterfactual result is accepted before the factual V1 replay passes.

pub mod aggregate;
pub mod audit;
pub mod calibrate;
pub mod canonical;
pub mod cohort;
pub mod config;
pub mod counterfactual;
pub mod dataset_split;
pub mod decode;
pub mod evaluation;
pub mod extract;
pub mod factual_replay;
pub mod historical_price_sidecar;
pub mod leverage;
pub mod leverage_campaign;
pub mod metric_engine;
pub mod metrics;
pub mod normalize;
pub mod parquet;
pub mod per_market;
pub mod pipeline;
mod pubkey_encoding;
pub mod reconcile;
pub mod replay;
pub mod report;
pub mod result_manifest;
pub mod rpc;
pub mod sensitivity;
pub mod spot_probe;
pub mod stages;
pub mod terminal;
pub mod token_cpi;
pub mod transfer_fee;
pub mod transfer_fee_extract;
pub mod universe;
pub mod v1_engine;
pub mod v1_reconciliation;
pub mod v1_terminal_reconciliation;
pub mod window;
