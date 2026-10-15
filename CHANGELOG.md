# Changelog

## 0.2.0 - 2026-10-16

- Added: Windows support. `poolwatch-<version>-x86_64-windows-msvc.zip` and
  `poolwatch-<version>-aarch64-windows-msvc.zip` contain a native `poolwatch.exe`
  for Windows x64 and Windows on ARM64. Each archive is unpacked and smoke-tested
  on a runner of its own architecture in CI.
- Added: Linux arm64. `poolwatch-<version>-aarch64-linux-musl.tar.gz` is a static
  musl binary (Raspberry Pi 4/5, Ampere, Graviton and similar), built and
  smoke-tested on a native arm64 runner.
- Every archive has a matching `.sha256` file.

## 0.1.0 - 2026-08-10

First release: the Linux x86_64 static musl binary.
