//! Fixture-driven regression tests for the RPC adapters.
//!
//! Everything here is offline: the fixtures under `tests/fixtures/` at the
//! repo root are compiled in, and only the pure parsers are exercised. No
//! test opens a socket.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use poolwatch_adapters::bitcoind::{
    btc_decimal_to_atomic, parse_mempool_info, parse_node_info, parse_raw_mempool,
};
use poolwatch_adapters::cryptonote::{
    parse_get_info, parse_pool_stats, parse_transaction_pool, QuirkTable,
};
use poolwatch_adapters::rpc::{
    digest_response, parse_cookie, parse_digest_challenge, DigestAlgorithm, DigestInput,
};
use poolwatch_core::{AdapterError, InstantTxKind, PoolTx};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/",
            $name
        ))
    };
}

/// Fixed "fetch time" so `first_seen_local` is assertable.
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_786_320_100)
}

fn at(unix_secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(unix_secs)
}

fn find<'a>(txs: &'a [PoolTx], txid: &str) -> &'a PoolTx {
    txs.iter()
        .find(|tx| tx.txid == txid)
        .unwrap_or_else(|| panic!("no transaction {txid} in the parsed pool"))
}

const MONERO_TX1: &str = "4e91a2c74e91a2c74e91a2c74e91a2c74e91a2c74e91a2c74e91a2c74e91a2c7";
const MONERO_TX2: &str = "b70d3f18b70d3f18b70d3f18b70d3f18b70d3f18b70d3f18b70d3f18b70d3f18";
const MONERO_TX3: &str = "1cf5e6a91cf5e6a91cf5e6a91cf5e6a91cf5e6a91cf5e6a91cf5e6a91cf5e6a9";

// ---------------------------------------------------------------------------
// cryptonote: pool
// ---------------------------------------------------------------------------

#[test]
fn monerod_pool_parses_exact_values() {
    let quirks = QuirkTable::for_kind(None);
    let pool = parse_transaction_pool(fixture!("monerod_pool.json"), now(), &quirks).unwrap();

    assert_eq!(pool.txs.len(), 3);
    // No daemon-reported total for this endpoint, so it is the size sum.
    assert_eq!(pool.pool_bytes, 1526 + 2210 + 2857);

    let tx1 = find(&pool.txs, MONERO_TX1);
    assert_eq!(tx1.size_bytes, 1526);
    assert_eq!(tx1.fee_atomic, 30_520_000_000);
    assert_eq!(tx1.fee_per_byte, 20_000_000.0);
    assert_eq!(tx1.received_by_node, Some(at(1_786_320_005)));
    assert_eq!(tx1.first_seen_local, now());
    assert_eq!(tx1.in_out_counts, Some((2, 2)));
    assert!(!tx1.double_spend_seen);
    assert_eq!(tx1.instant, InstantTxKind::None);

    let tx2 = find(&pool.txs, MONERO_TX2);
    assert_eq!(tx2.size_bytes, 2210);
    assert_eq!(tx2.fee_atomic, 55_250_000_000);
    assert_eq!(tx2.fee_per_byte, 25_000_000.0);
    assert_eq!(tx2.in_out_counts, Some((1, 2)));
    assert!(tx2.double_spend_seen);

    let tx3 = find(&pool.txs, MONERO_TX3);
    assert_eq!(tx3.size_bytes, 2857);
    assert_eq!(tx3.fee_atomic, 42_855_000_000);
    assert_eq!(tx3.fee_per_byte, 15_000_000.0);
    assert_eq!(tx3.in_out_counts, Some((3, 2)));
    // receive_time 0 means "not reported", not 1970.
    assert_eq!(tx3.received_by_node, None);
}

#[test]
fn monerod_pool_stats_parses_exact_values() {
    let stats =
        parse_pool_stats(fixture!("monerod_pool_stats.json"), "monero-main", now()).unwrap();

    assert_eq!(stats.chain_id, "monero-main");
    assert_eq!(stats.taken_at, now());
    assert_eq!(stats.tx_count, 3);
    assert_eq!(stats.pool_bytes, 6593);
    assert_eq!(stats.fee_total, Some(128_625_000_000));
    assert_eq!(stats.double_spends, Some(1));
    assert_eq!(stats.oldest, Some(at(1_786_319_890)));
}

#[test]
fn absent_transactions_field_is_an_empty_pool() {
    // An idle daemon omits `transactions` entirely.
    let body = br#"{"credits":0,"status":"OK","top_hash":"","untrusted":false}"#;
    let quirks = QuirkTable::for_kind(None);
    let pool = parse_transaction_pool(body, now(), &quirks).unwrap();

    assert!(pool.txs.is_empty());
    assert_eq!(pool.pool_bytes, 0);
}

#[test]
fn malformed_tx_json_does_not_fail_the_snapshot() {
    let body = br#"{
      "status": "OK",
      "transactions": [
        {
          "id_hash": "aa",
          "blob_size": 1500,
          "fee": 3000000000,
          "receive_time": 1786320005,
          "double_spend_seen": false,
          "tx_json": "{ this is not json"
        },
        {
          "id_hash": "bb",
          "blob_size": 2000,
          "fee": 4000000000,
          "receive_time": 1786320006,
          "double_spend_seen": false,
          "tx_json": "{\"version\":2,\"vin\":[{}],\"vout\":[{},{}]}"
        }
      ],
      "untrusted": false
    }"#;
    let quirks = QuirkTable::for_kind(None);
    let pool = parse_transaction_pool(body, now(), &quirks).unwrap();

    assert_eq!(pool.txs.len(), 2);
    assert_eq!(pool.pool_bytes, 3500);
    // The unparseable one keeps its fee/size and just loses the counts.
    assert_eq!(find(&pool.txs, "aa").in_out_counts, None);
    assert_eq!(find(&pool.txs, "aa").fee_atomic, 3_000_000_000);
    assert_eq!(find(&pool.txs, "bb").in_out_counts, Some((1, 2)));
}

#[test]
fn non_ok_daemon_status_is_an_rpc_error() {
    let body = br#"{"status":"BUSY","transactions":[],"untrusted":false}"#;
    let quirks = QuirkTable::for_kind(None);
    let error = parse_transaction_pool(body, now(), &quirks).unwrap_err();

    match error {
        AdapterError::Rpc { message, .. } => assert!(message.contains("BUSY"), "{message}"),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// cryptonote: instant transactions
// ---------------------------------------------------------------------------

#[test]
fn oxend_blink_flag_classifies_instant_txs() {
    let quirks = QuirkTable::for_kind(Some(InstantTxKind::Blink));
    let pool = parse_transaction_pool(fixture!("oxend_pool_blink.json"), now(), &quirks).unwrap();

    let blink = find(
        &pool.txs,
        "9a3c71fe9a3c71fe9a3c71fe9a3c71fe9a3c71fe9a3c71fe9a3c71fe9a3c71fe",
    );
    assert_eq!(blink.instant, InstantTxKind::Blink);
    assert_eq!(blink.fee_atomic, 7_900_000);
    assert_eq!(blink.fee_per_byte, 5_000.0);
    assert_eq!(blink.in_out_counts, Some((2, 2)));

    let plain = find(
        &pool.txs,
        "2d40b85c2d40b85c2d40b85c2d40b85c2d40b85c2d40b85c2d40b85c2d40b85c",
    );
    assert_eq!(plain.instant, InstantTxKind::None);
    assert_eq!(plain.fee_atomic, 6_282_000);
    assert_eq!(plain.fee_per_byte, 3_000.0);
    assert_eq!(plain.in_out_counts, Some((3, 2)));
}

#[test]
fn beldexd_flash_flag_classifies_instant_txs() {
    let quirks = QuirkTable::for_kind(Some(InstantTxKind::Flash));
    let pool = parse_transaction_pool(fixture!("beldexd_pool_flash.json"), now(), &quirks).unwrap();

    let flash = find(
        &pool.txs,
        "6ef20a376ef20a376ef20a376ef20a376ef20a376ef20a376ef20a376ef20a37",
    );
    assert_eq!(flash.instant, InstantTxKind::Flash);
    assert_eq!(flash.fee_atomic, 8_560_000);
    assert_eq!(flash.fee_per_byte, 5_000.0);

    let plain = find(
        &pool.txs,
        "c815d94bc815d94bc815d94bc815d94bc815d94bc815d94bc815d94bc815d94b",
    );
    assert_eq!(plain.instant, InstantTxKind::None);
    assert_eq!(plain.fee_per_byte, 2_000.0);
    assert_eq!(plain.in_out_counts, Some((4, 2)));
}

#[test]
fn instant_detection_is_off_unless_configured() {
    // Same Oxen fixture, chain configured without instant_tx.
    let quirks = QuirkTable::for_kind(None);
    let pool = parse_transaction_pool(fixture!("oxend_pool_blink.json"), now(), &quirks).unwrap();
    assert!(pool.txs.iter().all(|tx| tx.instant == InstantTxKind::None));
}

#[test]
fn flash_chains_accept_the_legacy_blink_field() {
    // Beldex builds forked from Oxen before the blink -> flash rename.
    let body = br#"{"status":"OK","transactions":[
        {"id_hash":"aa","blob_size":1000,"fee":1000,"receive_time":1,"blink":true}
    ]}"#;
    let quirks = QuirkTable::for_kind(Some(InstantTxKind::Flash));
    let pool = parse_transaction_pool(body, now(), &quirks).unwrap();
    assert_eq!(pool.txs[0].instant, InstantTxKind::Flash);
}

#[test]
fn unexpected_instant_flag_type_degrades_to_none() {
    // A fork that turned the boolean into a string must not crash the poller.
    let body = br#"{"status":"OK","transactions":[
        {"id_hash":"aa","blob_size":1000,"fee":1000,"receive_time":1,"blink":"yes"}
    ]}"#;
    let quirks = QuirkTable::for_kind(Some(InstantTxKind::Blink));
    let pool = parse_transaction_pool(body, now(), &quirks).unwrap();
    assert_eq!(pool.txs[0].instant, InstantTxKind::None);
}

#[test]
fn missing_instant_flag_degrades_to_none() {
    // Instant detection configured, daemon does not report the flag at all.
    let quirks = QuirkTable::for_kind(Some(InstantTxKind::Blink));
    let pool = parse_transaction_pool(fixture!("monerod_pool.json"), now(), &quirks).unwrap();
    assert!(pool.txs.iter().all(|tx| tx.instant == InstantTxKind::None));
}

// ---------------------------------------------------------------------------
// cryptonote: get_info
// ---------------------------------------------------------------------------

#[test]
fn monerod_get_info_parses() {
    let body = br#"{"id":"0","jsonrpc":"2.0","result":{
        "block_weight_limit":600000,"height":3733712,
        "incoming_connections_count":26,"outgoing_connections_count":12,
        "status":"OK","synchronized":true,"target_height":0,
        "untrusted":false,"version":"0.18.4.0-release"}}"#;
    let info = parse_get_info(body).unwrap();

    assert_eq!(info.height, 3_733_712);
    assert_eq!(info.version, "0.18.4.0-release");
    assert_eq!(info.peer_count, Some(38));
}

#[test]
fn monerod_get_info_tolerates_a_numeric_version() {
    let body = br#"{"jsonrpc":"2.0","result":{"height":42,"version":131079,"status":"OK"}}"#;
    let info = parse_get_info(body).unwrap();

    assert_eq!(info.version, "131079");
    assert_eq!(info.peer_count, None);
}

#[test]
fn json_rpc_error_object_becomes_an_rpc_error() {
    let body =
        br#"{"id":"0","jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"}}"#;
    match parse_get_info(body).unwrap_err() {
        AdapterError::Rpc { code, message } => {
            assert_eq!(code, -32601);
            assert_eq!(message, "Method not found");
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// bitcoind
// ---------------------------------------------------------------------------

const BTC_TX_A: &str = "3f0b7c2a3f0b7c2a3f0b7c2a3f0b7c2a3f0b7c2a3f0b7c2a3f0b7c2a3f0b7c2a";
const BTC_TX_B: &str = "8d51e9408d51e9408d51e9408d51e9408d51e9408d51e9408d51e9408d51e940";
const BTC_TX_C: &str = "c2a4176bc2a4176bc2a4176bc2a4176bc2a4176bc2a4176bc2a4176bc2a4176b";
const BTC_TX_D: &str = "05e83d9f05e83d9f05e83d9f05e83d9f05e83d9f05e83d9f05e83d9f05e83d9f";

#[test]
fn bitcoind_rawmempool_parses_exact_values() {
    let pool = parse_raw_mempool(fixture!("bitcoind_rawmempool.json"), now()).unwrap();

    assert_eq!(pool.txs.len(), 4);
    assert_eq!(pool.pool_bytes, 141 + 285 + 1052 + 208);

    let a = find(&pool.txs, BTC_TX_A);
    // vsize, not size/weight — fee rates are quoted per vbyte.
    assert_eq!(a.size_bytes, 141);
    assert_eq!(a.fee_atomic, 1410);
    assert_eq!(a.fee_per_byte, 10.0);
    assert_eq!(a.received_by_node, Some(at(1_786_319_981)));
    assert_eq!(a.first_seen_local, now());
    assert_eq!(a.instant, InstantTxKind::None);
    assert_eq!(a.in_out_counts, None);
    assert!(!a.double_spend_seen);

    let b = find(&pool.txs, BTC_TX_B);
    assert_eq!(b.fee_atomic, 855);
    assert_eq!(b.fee_per_byte, 3.0);

    let c = find(&pool.txs, BTC_TX_C);
    assert_eq!(c.size_bytes, 1052);
    assert_eq!(c.fee_atomic, 26_300);
    assert_eq!(c.fee_per_byte, 25.0);

    let d = find(&pool.txs, BTC_TX_D);
    assert_eq!(d.fee_atomic, 416);
    assert_eq!(d.fee_per_byte, 2.0);
}

#[test]
fn bitcoind_mempool_info_parses() {
    let body = br#"{"result":{"loaded":true,"size":4,"bytes":1686,"usage":9280,
        "total_fee":0.00029005,"maxmempool":300000000,"mempoolminfee":0.00001000,
        "minrelaytxfee":0.00001000,"unbroadcastcount":0},"error":null,"id":"poolwatch"}"#;
    let stats = parse_mempool_info(body, "bitcoin-main", now()).unwrap();

    assert_eq!(stats.chain_id, "bitcoin-main");
    assert_eq!(stats.tx_count, 4);
    assert_eq!(stats.pool_bytes, 1686);
    assert_eq!(stats.fee_total, Some(29_005));
    assert_eq!(stats.double_spends, None);
    assert_eq!(stats.oldest, None);
}

#[test]
fn bitcoind_mempool_info_without_total_fee() {
    // total_fee only exists on Core 23+.
    let body = br#"{"result":{"size":2,"bytes":426},"error":null,"id":"poolwatch"}"#;
    let stats = parse_mempool_info(body, "bitcoin-main", now()).unwrap();

    assert_eq!(stats.tx_count, 2);
    assert_eq!(stats.fee_total, None);
}

#[test]
fn bitcoind_node_info_parses() {
    let chain = br#"{"result":{"chain":"main","blocks":962741,"headers":962741,
        "initialblockdownload":false},"error":null,"id":"poolwatch"}"#;
    let network = br#"{"result":{"version":280100,"subversion":"/Satoshi:28.1.0/",
        "protocolversion":70016,"connections":11,"connections_in":3,
        "connections_out":8},"error":null,"id":"poolwatch"}"#;
    let info = parse_node_info(chain, network).unwrap();

    assert_eq!(info.height, 962_741);
    assert_eq!(info.version, "Satoshi:28.1.0");
    assert_eq!(info.peer_count, Some(11));
}

#[test]
fn bitcoind_jsonrpc_error_becomes_an_rpc_error() {
    let body =
        br#"{"result":null,"error":{"code":-32601,"message":"Method not found"},"id":"poolwatch"}"#;
    match parse_raw_mempool(body, now()).unwrap_err() {
        AdapterError::Rpc { code, message } => {
            assert_eq!(code, -32601);
            assert_eq!(message, "Method not found");
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// bitcoind: BTC decimal -> satoshi, exactly
// ---------------------------------------------------------------------------

#[test]
fn btc_decimal_conversion_is_exact() {
    let table = [
        ("0.00012345", 12_345_u128),
        ("0.1", 10_000_000),
        ("1", 100_000_000),
        ("0.00000001", 1),
        ("0", 0),
        ("21000000.0", 2_100_000_000_000_000),
        // Shapes Core itself emits: always eight fractional digits.
        ("0.00001410", 1_410),
        ("0.00026300", 26_300),
        ("1.00000000", 100_000_000),
    ];
    for (text, expected) in table {
        assert_eq!(
            btc_decimal_to_atomic(text).unwrap(),
            expected,
            "converting {text}"
        );
    }
}

#[test]
fn btc_decimal_rejects_what_it_cannot_represent() {
    for bad in [
        "0.000000001", // nine fractional digits
        "1e8",         // exponent notation
        "1E-8",
        "-0.5",
        "+1.0",
        "",
        ".",
        "abc",
        "1.2.3",
        "1 000.0",
    ] {
        assert!(
            matches!(btc_decimal_to_atomic(bad), Err(AdapterError::Parse(_))),
            "expected {bad:?} to be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// auth
// ---------------------------------------------------------------------------

#[test]
fn digest_response_matches_the_rfc2617_example() {
    // RFC 2617 section 3.5.
    let response = digest_response(&DigestInput {
        username: "Mufasa",
        password: "Circle Of Life",
        realm: "testrealm@host.com",
        nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093",
        nc: "00000001",
        cnonce: "0a4f113b",
        qop: Some("auth"),
        method: "GET",
        uri: "/dir/index.html",
        algorithm: DigestAlgorithm::Md5,
    });
    assert_eq!(response, "6629fae49393a05397450978507c4ef1");
}

#[test]
fn digest_response_without_qop_uses_the_rfc2069_form() {
    let input = DigestInput {
        username: "Mufasa",
        password: "Circle Of Life",
        realm: "testrealm@host.com",
        nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093",
        nc: "00000001",
        cnonce: "0a4f113b",
        qop: None,
        method: "GET",
        uri: "/dir/index.html",
        algorithm: DigestAlgorithm::Md5,
    };
    // MD5(HA1:nonce:HA2), i.e. no nc/cnonce/qop in the digest.
    assert_eq!(digest_response(&input), "670fd8c2df070c60b045671b8b24ff02");
}

#[test]
fn digest_md5_sess_hashes_ha1_differently() {
    let base = DigestInput {
        username: "Mufasa",
        password: "Circle Of Life",
        realm: "testrealm@host.com",
        nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093",
        nc: "00000001",
        cnonce: "0a4f113b",
        qop: Some("auth"),
        method: "GET",
        uri: "/dir/index.html",
        algorithm: DigestAlgorithm::Md5,
    };
    let sess = DigestInput {
        algorithm: DigestAlgorithm::Md5Sess,
        ..base
    };
    assert_ne!(digest_response(&base), digest_response(&sess));
}

#[test]
fn parses_a_monerod_style_digest_challenge() {
    let header = "Digest qop=\"auth\", realm=\"monero-rpc\", \
                  nonce=\"1uMDJgPr4/pM7YtRxUOZ6A==\", stale=false, \
                  algorithm=MD5, opaque=\"d41d8cd98f00b204\"";
    let challenge = parse_digest_challenge(header).unwrap();

    assert_eq!(challenge.realm, "monero-rpc");
    assert_eq!(challenge.nonce, "1uMDJgPr4/pM7YtRxUOZ6A==");
    assert_eq!(challenge.qop.as_deref(), Some("auth"));
    assert_eq!(challenge.algorithm, DigestAlgorithm::Md5);
    assert_eq!(challenge.opaque.as_deref(), Some("d41d8cd98f00b204"));
    assert!(!challenge.stale);
}

#[test]
fn parses_digest_challenge_details() {
    // A comma inside the quoted qop list must not split the parameter, a
    // second scheme must be skipped, and stale/MD5-sess must be picked up.
    let header = "Basic realm=\"other\", Digest realm=\"r\", nonce=\"n\", \
                  qop=\"auth-int,auth\", algorithm=MD5-sess, stale=TRUE";
    let challenge = parse_digest_challenge(header).unwrap();

    assert_eq!(challenge.realm, "r");
    assert_eq!(challenge.qop.as_deref(), Some("auth"));
    assert_eq!(challenge.algorithm, DigestAlgorithm::Md5Sess);
    assert!(challenge.stale);
    assert_eq!(challenge.opaque, None);
}

#[test]
fn rejects_non_digest_challenges() {
    assert!(parse_digest_challenge("Basic realm=\"monero\"").is_none());
    // Digest without a nonce is unusable.
    assert!(parse_digest_challenge("Digest realm=\"r\"").is_none());
}

#[test]
fn cookie_file_parses() {
    let (user, token) = parse_cookie("__cookie__:8f3b0c1d2e4a5b6c7d8e9f00").unwrap();
    assert_eq!(user, "__cookie__");
    assert_eq!(token, "8f3b0c1d2e4a5b6c7d8e9f00");

    // Core writes no trailing newline, but tolerate one.
    let (_, token) = parse_cookie("__cookie__:abc\n").unwrap();
    assert_eq!(token, "abc");
}

#[test]
fn cookie_file_rejects_garbage() {
    for bad in ["", "no-separator-here", "__cookie__:", ":token"] {
        assert!(
            matches!(parse_cookie(bad), Err(AdapterError::Auth(_))),
            "expected {bad:?} to be rejected"
        );
    }
}
