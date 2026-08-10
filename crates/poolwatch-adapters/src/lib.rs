//! Concrete RPC adapters for poolwatch.
//!
//! - [`cryptonote`]: monerod / oxend / beldexd family (plain-HTTP POST
//!   endpoints + /json_rpc, optional digest auth).
//! - [`bitcoind`]: bitcoind family (JSON-RPC 1.0, basic auth via cookie
//!   file or user/pass).
//!
//! Adding a new fork of either family must require only a new `[[chain]]`
//! config block — never code changes here.

pub mod bitcoind;
pub mod cryptonote;
pub(crate) mod rpc;

pub use bitcoind::{BitcoindAdapter, BitcoindAuth, BitcoindConfig};
pub use cryptonote::{CryptonoteAdapter, CryptonoteConfig};
