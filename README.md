# poolwatch

[![ARM64 builds](https://github.com/Dhiva-Labs/poolwatch/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Dhiva-Labs/poolwatch/actions/workflows/ci.yml)
![Linux x64 | arm64](https://img.shields.io/badge/Linux-x64%20%7C%20arm64-informational?logo=linux&logoColor=white)
![Windows x64 | arm64](https://img.shields.io/badge/Windows-x64%20%7C%20arm64-informational?logo=windows&logoColor=white)
![License: MIT](https://img.shields.io/badge/license-MIT-blue)

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
- **Zero runtime dependencies** — static musl build for x86_64 and arm64
  Linux; `scp` it to a bare server and run.

## Install

### Release binary (recommended)

Download the archive for your platform from the
[latest release](https://github.com/Dhiva-Labs/poolwatch/releases). Every
archive has a matching `.sha256` file.

| Platform | Archive | Notes |
|---|---|---|
| Linux x86_64 | `poolwatch-<version>-x86_64-linux-musl.tar.gz` | static musl, no libraries needed |
| Linux arm64 (aarch64) | `poolwatch-<version>-aarch64-linux-musl.tar.gz` | static musl, no libraries needed (Raspberry Pi 4/5, Ampere, Graviton, ...) |
| Windows x64 | `poolwatch-<version>-x86_64-windows-msvc.zip` | `poolwatch.exe` |
| Windows arm64 | `poolwatch-<version>-aarch64-windows-msvc.zip` | `poolwatch.exe`, native ARM64 (Snapdragon X, Windows on ARM) |

The arm64 and Windows archives are attached to every release after v0.1.0
(v0.1.0 shipped the Linux x86_64 archive only).

```sh
tar xzf poolwatch-<version>-<arch>-linux-musl.tar.gz
cd poolwatch-<version>-<arch>-linux-musl
./poolwatch --config poolwatch.toml
```

Verify a download with `sha256sum -c poolwatch-<version>-<arch>-linux-musl.tar.gz.sha256`
(Linux) or `Get-FileHash <archive> -Algorithm SHA256` (Windows).

#### Windows notes

Run poolwatch in Windows Terminal (or any console with VT support):

```powershell
.\poolwatch.exe --config poolwatch.toml
```

- `~` in the config expands to your user profile (`%USERPROFILE%`), so the
  example config's `log_file` lands in `C:\Users\<you>\.local\state\poolwatch\`.
  Change it if you prefer another place.
- bitcoind keeps its auth cookie in `%APPDATA%\Bitcoin\.cookie`, not in
  `~/.bitcoin`. Give the full path with a TOML *literal* string (single
  quotes, no escaping): `rpc_cookie_file = 'C:\Users\you\AppData\Roaming\Bitcoin\.cookie'`,
  or use `rpc_user` / `rpc_password`.
- CI builds both Windows targets natively (x64 and ARM64 runners), runs the
  full test suite on them, and runs `--once` against a mock node. The
  interactive TUI uses crossterm's Windows console backend, but it is not
  driven interactively in CI.

### From source

```sh
git clone https://github.com/Dhiva-Labs/poolwatch
cd poolwatch
cargo install --path crates/poolwatch-tui
```

For the fully static binary (use `aarch64-unknown-linux-musl` on arm64;
needs `musl-tools` and `rustup target add <target>`):

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
