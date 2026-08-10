# RPC fixtures

Recorded response bodies used by `crates/poolwatch-adapters/tests/adapter_tests.rs`.
They are the regression net against RPC field drift: if a daemon renames or
retypes a field, a fixture test fails instead of the TUI silently showing
zeros.

**None of these were captured from a live node.** No daemon of any of these
families was available in the development environment, so every file was
*constructed* by reading the daemons' own RPC definitions and filling in
plausible, internally consistent values. JSON has no comments, hence this
file.

Ground rules for every fixture here:

- Field **names, types and nesting** follow the daemon source cited below and
  are the part the tests actually pin down.
- Field **values** are invented. Transaction ids, key images, one-time output
  keys and block hashes are well-formed hex of the right length but are not
  real; they do not correspond to anything on any chain.
- Magnitudes are chosen to look like current mainnet traffic (mid-2026
  heights, plausible pool sizes and fee rates), and the numbers are
  internally consistent — e.g. `monerod_pool_stats.json` totals really are
  the sum of the transactions in `monerod_pool.json`.
- `tx_blob` is set to `""` and Monero's `rctsig_prunable` section is omitted.
  Both are large, unused by poolwatch, and would dwarf the rest of the file.
- Fee amounts and sizes are picked so `fee / size` is an exact number, which
  lets the tests assert `fee_per_byte` without float slop.

## Files

### `monerod_pool.json` — CONSTRUCTED (upstream shape)

`POST /get_transaction_pool` on monerod 0.18.x. Three transactions:

| txid prefix | blob_size | fee (piconero) | pN/B | notes |
| --- | --- | --- | --- | --- |
| `4e91a2c7…` | 1526 | 30520000000 | 20000000 | 2 in / 2 out |
| `b70d3f18…` | 2210 | 55250000000 | 25000000 | 1 in / 2 out, `double_spend_seen: true` |
| `1cf5e6a9…` | 2857 | 42855000000 | 15000000 | 3 in / 2 out, `receive_time: 0` → `received_by_node: None` |

Shape follows the `tx_info` and response structs in
[monero-project/monero `src/rpc/core_rpc_server_commands_defs.h`](https://raw.githubusercontent.com/monero-project/monero/master/src/rpc/core_rpc_server_commands_defs.h)
(`id_hash`, `tx_json`, `blob_size`, `weight`, `fee`, `receive_time`,
`double_spend_seen`, …). Note the txid field is `id_hash`, not `tx_hash`.

### `monerod_pool_stats.json` — CONSTRUCTED (upstream shape)

`POST /get_transaction_pool_stats`. `pool_stats` fields per the `txpool_stats`
struct in the same header (`bytes_total`, `fee_total`, `oldest`, `txs_total`,
`num_double_spends`, `histo`, …). Totals match `monerod_pool.json`.

### `oxend_pool_blink.json` — **SYNTHETIC**

`POST /get_transaction_pool` on oxend, with Oxen's per-transaction instant-tx
flag. Two transactions, `blink: true` (1580 B, 7900000 atomic, 5000/B) and
`blink: false` (2094 B, 6282000 atomic, 3000/B).

The `blink` field is real: `bool blink; // True if this is a signed blink
transaction` in the `tx_info` struct of
[oxen-io/oxen-core `src/rpc/core_rpc_server_commands_defs.h` @ v8.1.3](https://raw.githubusercontent.com/oxen-io/oxen-core/v8.1.3/src/rpc/core_rpc_server_commands_defs.h).
Everything else about this file — values, and the exact `tx_json` rendering of
Oxen's extra `type` / `output_unlock_times` fields — is reconstructed, not
observed. Treat it as a pin for *our parser*, not as evidence of what a real
oxend emits byte for byte.

### `beldexd_pool_flash.json` — **SYNTHETIC**

Same as above for beldexd, using Beldex's rename of the flag: `bool flash; //
True if this is a signed flash transaction` in the `tx_info` struct of
[Beldex-Coin/beldex `src/rpc/core_rpc_server_commands_defs.h` @ v4.0.0](https://raw.githubusercontent.com/Beldex-Coin/beldex/v4.0.0/src/rpc/core_rpc_server_commands_defs.h).
Two transactions, `flash: true` (1712 B, 8560000 atomic, 5000/B) and
`flash: false` (2350 B, 4700000 atomic, 2000/B). Same caveat as the Oxen file.

Neither daemon has a Blink/Flash transaction *type*: `txtype` is
`{standard, state_change, key_image_unlock, stake, oxen_name_system, …}` for
Oxen and `{standard, state_change, key_image_unlock, stake,
beldex_name_system, coin_burn}` for Beldex (`src/cryptonote_basic/txtypes.h`
in both). Instant-ness is a mempool property, so `tx_json.type` is `0`
(standard) even on the instant transactions here.

### `bitcoind_rawmempool.json` — CONSTRUCTED (upstream shape)

`getrawmempool true` on Bitcoin Core 28-era, wrapped in the JSON-RPC 1.0
envelope (`result` / `error` / `id`) so the envelope handling is covered too.
Four entries:

| txid prefix | vsize | `fees.base` | sat | sat/vB |
| --- | --- | --- | --- | --- |
| `3f0b7c2a…` | 141 | 0.00001410 | 1410 | 10 |
| `8d51e940…` | 285 | 0.00000855 | 855 | 3 |
| `c2a4176b…` | 1052 | 0.00026300 | 26300 | 25 |
| `05e83d9f…` | 208 | 0.00000416 | 416 | 2 |

`05e83d9f…` spends `3f0b7c2a…`, so it carries `depends` and a larger
`ancestorsize`/`fees.ancestor`.

This file must be edited by hand, never round-tripped through a JSON
pretty-printer: Core prints amounts with `%d.%08d` (always eight decimal
places, never an exponent) and the adapter converts that decimal *text* to
satoshis with string math. A formatter that rewrites `0.00001410` as `1.41e-5`
would both change the meaning and trip the adapter's deliberate refusal to
parse exponent notation.
