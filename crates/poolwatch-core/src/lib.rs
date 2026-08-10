//! poolwatch-core: chain-agnostic domain model, adapter contract, first-seen
//! tracking, and pool statistics.
//!
//! Nothing in this crate knows about HTTP, JSON-RPC, or any specific daemon.
//! Adapters (poolwatch-adapters) produce [`model::MempoolSnapshot`]s; the TUI
//! consumes them. All amounts are atomic units (`u128`); formatting to
//! XMR/BDX/OXEN/BTC happens only in the UI layer.

pub mod adapter;
pub mod model;
pub mod stats;
pub mod tracker;

pub use adapter::{AdapterError, ChainAdapter};
pub use model::{ChainMeta, FeeBand, InstantTxKind, MempoolSnapshot, PoolStats, PoolTx};
pub use tracker::{ChainTracker, TrackerStats};
