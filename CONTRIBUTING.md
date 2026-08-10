# Contributing to poolwatch

## Dev loop

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

All three must be clean before a PR. Tests never touch the network —
adapter behavior is pinned by fixture files in `tests/fixtures/`.

Commit style: conventional commits (`feat(adapter): …`, `fix(tui): …`,
`test(tracker): …`).

## Adding a new chain

### Fork of a supported family → config only

If the daemon is a monerod-family (`cryptonote`) or bitcoind-family
(`bitcoind`) fork, **no code change is needed or accepted** — add a
`[[chain]]` block:

```toml
[[chain]]
id = "wownero-main"
display_name = "Wownero"
ticker = "WOW"
family = "cryptonote"
rpc_url = "http://127.0.0.1:34568"
decimals = 11
```

If you find a fork that requires code to work, that's an adapter bug —
open an issue with the RPC response that broke.

### New RPC family → new adapter

Checklist for a new `family` value:

1. New module in `crates/poolwatch-adapters/src/<family>.rs` implementing
   `ChainAdapter` (`fetch_snapshot`, `fetch_pool_stats`, `chain_meta`).
2. Map the daemon's reality into the domain model honestly:
   atomic units as `u128` (never float math on amounts), `size_bytes` =
   whatever the chain's fee market actually prices (vsize for
   segwit-style chains), daemon-reported receive time of `0` → `None`.
3. Errors: transport → `Http`, endpoint missing (404/405) →
   `Unsupported` (capability flag, never a crash), oversized pool
   response → `TooLarge` (drives stats-only degradation).
4. Wire the family name into config validation + adapter construction in
   `crates/poolwatch-tui/src/config.rs`.
5. Capture at least one real full-pool RPC response into
   `tests/fixtures/` (mark synthetic fixtures in the fixtures README) and
   assert exact parsed values in
   `crates/poolwatch-adapters/tests/adapter_tests.rs`.
6. Add a commented example block to `poolwatch.example.toml` and a row to
   the README's RPC table.

## Scope guard

poolwatch deliberately does **not** build: web UI, block-explorer
features, wallet features, tx broadcasting, historical databases, price
data. PRs in those directions will be closed with a friendly link to this
paragraph.
