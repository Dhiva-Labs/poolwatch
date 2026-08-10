# poolwatch

**htop for your mempool.**

A single static binary that watches the mempools of your own blockchain
daemons — Monero, Oxen, Beldex, Bitcoin, and any compatible fork — side by
side, in the terminal, over their native RPC. No external APIs, no database,
no web frontend, nothing phones home.

```
┌ poolwatch ──────────────────────────────────────────────────────────┐
│ CHAIN     TXS   POOL SIZE   IN/MIN   MED FEE/B    INSTANT   HEIGHT  │
│ Monero    412   18.4 MB     3.2      31 pN/B      —         3281044 │
│ Oxen       12   210 KB      0.4      15 nO/B      ⚡ 2 blink 1204882 │
│ Beldex      7   96 KB       0.2      12 nB/B      ⚡ 1 flash 4881202 │
│ Bitcoin  8241   64.1 MB     41.7     4.2 sat/vB   —          908114 │
└─────────────────────────────────────────────────────────────────────┘
```

## Why

Existing mempool tools are either heavy full-stack explorers
(mempool.space) or thin clients that query public APIs. poolwatch talks
directly to **your** node and renders everything in the terminal — built
for node operators, miners, and privacy-conscious users on headless boxes.

- **Multi-chain in one binary** — monerod + oxend + beldexd + bitcoind
  side by side, each polled independently.
- **Instant-tx awareness** — Oxen **Blink** and Beldex **Flash**
  transactions are detected and flagged (⚡). No other monitor surfaces
  this.
- **First-seen tracking** — poolwatch timestamps every tx on first
  appearance and derives pool residency time and inflow rate (tx/min over
  1m/5m/15m), which daemons don't expose.
- **Zero runtime dependencies** — static musl build; `scp` it to a bare
  server and run.

## Install

### Release binary (recommended)

Grab the static `poolwatch` binary from the
[latest release](https://github.com/Dhiva-Labs/poolwatch/releases), then:

```sh
chmod +x poolwatch
./poolwatch --config poolwatch.toml
```

It runs on any x86_64 Linux — no libraries needed.

### From source

```sh
git clone https://github.com/Dhiva-Labs/poolwatch
cd poolwatch
cargo install --path crates/poolwatch-tui
```

For the fully static binary:

```sh
cargo build --release --target x86_64-unknown-linux-musl
# → target/x86_64-unknown-linux-musl/release/poolwatch
```

## Quickstart

Copy [poolwatch.example.toml](poolwatch.example.toml) to `poolwatch.toml`,
keep the `[[chain]]` blocks for the daemons you run, and start:

```sh
poolwatch --config poolwatch.toml
```

One-shot scriptable mode (no TUI, plain text to stdout):

```sh
poolwatch --config poolwatch.toml --once
```

### Enabling node RPC

| Daemon | What you need |
|---|---|
| monerod | Default unrestricted RPC on `127.0.0.1:18081` already works. With `--rpc-login user:pass`, set `rpc_login` in the chain block (digest auth is handled). |
| oxend | Default RPC on `127.0.0.1:22023`. Set `instant_tx = "blink"` to flag Blink txs. |
| beldexd | Default RPC on `127.0.0.1:19091`. Set `instant_tx = "flash"` to flag Flash txs. |
| bitcoind | `server=1` in bitcoin.conf. Point `rpc_cookie_file` at `~/.bitcoin/.cookie` (default), or set `rpc_user`/`rpc_password`. |

Adding another fork of a supported family (Wownero, Litecoin, …) is a
config-only change — add a `[[chain]]` block with the right `family`,
port, and `decimals`. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Keybindings

| Key | Action |
|---|---|
| `q` | quit |
| `d` | dashboard (all chains) |
| `1`–`9` / `Enter` | chain detail view |
| `Tab` | cycle chains |
| `↑↓` / `j k` | scroll tx table |
| `f` / `s` / `t` | sort by fee, size, age |
| `/` | filter by txid prefix |
| `p` | pause polling |
| `?` | help overlay |

## FAQ

**Why do fee numbers look so different across chains?**
Each chain is shown in its native atomic rate unit: piconero/byte for
Monero, satoshi/vbyte for Bitcoin, and so on (configurable via
`fee_rate_unit`). Absolute values aren't comparable across chains —
that's inherent to different fee markets, not a display bug.

**A chain shows a yellow ● and "stats-only".**
Its full-pool RPC response exceeded the size guard
(`max_pool_response_mb`, default 64) or the endpoint isn't supported by
that daemon. poolwatch degrades to the cheap stats call instead of
stalling; the tx table repopulates when the pool shrinks or the guard is
raised.

**Does poolwatch send anything anywhere?**
No. It only POSTs to the RPC URLs in your config and writes a log file.

**Is the residency/inflow data exact?**
It's derived from poolwatch's own observations, so it starts when
poolwatch starts. Daemon-reported receive times are used where available;
first-seen tracking fills the rest.

## Development

- Layout: `poolwatch-core` (chain-agnostic model + tracking/stats),
  `poolwatch-adapters` (cryptonote + bitcoind RPC), `poolwatch-tui`
  (binary). The adapter trait + the config table are the only
  abstractions; adding a new *family* means one new adapter module.
- `cargo test` runs entirely offline: adapter tests round-trip captured
  RPC fixture files in [tests/fixtures/](tests/fixtures/) (see its README
  for provenance — Oxen/Beldex fixtures are synthetic, constructed from
  daemon source). Testing against live nodes is manual:
  point a chain block at your daemon and run with `--once`.
- Gate everything through `cargo fmt`, `cargo clippy --all-targets -- -D
  warnings`, `cargo test` before a PR.
- Dependency note: reqwest ≥0.13 renamed the TLS feature to `rustls`
  (this repo uses it with `default-features = false` to keep musl builds
  OpenSSL-free).

## License

MIT © Dhiva-Labs
