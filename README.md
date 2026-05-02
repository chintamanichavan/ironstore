# ironstore (vektalite)

An embedded LSM-tree storage server written in Rust. WAL-backed durability, sorted-string-table compaction, optional TLS, token auth.

The binary is named `vektalite` (original codename); the repo is `ironstore`.

## Stack

- **Runtime** — Tokio async
- **Storage** — Custom LSM tree (memtable → WAL → SSTables → background compaction)
- **Network** — Async TCP server
- **TLS** — optional, via cert+key env vars
- **Logging** — tracing + tracing-subscriber

## Build

```sh
cargo build --release
```

## Run

```sh
VEKTALITE_DATA=./data \
VEKTALITE_ADDR=127.0.0.1:6380 \
VEKTALITE_AUTH=<token> \
cargo run --release --bin vektalite
```

For TLS:
```sh
VEKTALITE_TLS_CERT=path/to/cert.pem \
VEKTALITE_TLS_KEY=path/to/key.pem \
cargo run --release --bin vektalite
```

CLI tool:
```sh
cargo run --bin vektalite-cli -- <command>
```

## Status

Personal storage-engine experiment. Not battle-tested; do not use for production data.
