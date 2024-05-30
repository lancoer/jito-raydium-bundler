# JITO-RAYDIUM-CPMM-BUNDLER

Jito Raydium CPMM Bundler is a new bundler that deposit liquidity pool, snipe base token and disperse to wallets at ease using Jito CLI.

This is backed by Raydium CPMM instructions library.

## Requirements

0. Install Rust
1. Install `protoc`, `gcc`, `openssl`
2. Copy over `.env.example` into `.env`, plug your RPCs (otherwise uses solana default public RPCs)
3. Copy over `auth.json` - JITO authentication keypair and `id.json` - the keypair with some SOL to fund the endeavour

   - Worring about your private key? Just use [sbjc](https://lib.rs/crates/solana-base58-json-converter) library to convert to json at ease!

4. Enable Rust Nightly and `cargo build --release`

## Usage

### Send the bundle

OpenTime can also be adjusted using `--open-time [n]`, e.g. `--open-time 10000000000` for 10000000000 open-time, default is 0

For cross region functionality, add the `--regions REGION1,REGION2,etc` arg. [More details](https://jito-labs.gitbook.io/mev/searcher-services/recommendations#cross-region)

JitoTip can also be adjusted using `--jito-tip [lamport]`, e.g. `--jito-tip 1000000000` for 0.001 sol, default is 1000

```powershell
# Mainnet (EURC / USDC)
# cargo run send-bundle -h or --help to see inputs
cargo run --keypair-path auth.json send-bundle HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v 100000000 100 <USDC token> 1 <EURC token> 10 --disperse-wallets 96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5
# Note: Don't provide --keypair-path argument if not planning to use authentication
```

```powershell
# Testnet (EURC / USDC)
# cargo run send-bundle -h or --help to see inputs
cargo run send-bundle HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr 4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU 100000000 100 5UDo7EdsHU5YaBSJCcmdA7c5idxykdZbR3D4TxrccxNN 1 9mRRczk7jbWG1rXHfnmHkmkwmrf9QoD1TpcNPT6CohVe 10 --disperse-wallets B1mrQSpdeMU9gCvkJ6VsXVVoYjRGkNA7TtjMyqxrhecH
# Note: Don't provide --keypair-path argument if not planning to use authentication
```

## Devnet

In order for test or simulate cases, you need to add `'devnet'` feature to `raydium-cp-swap` library in [Cargo.toml](./cli/Cargo.toml)

And ensure you've set devnet url and testnet url for solana & raydium and jito cli in [.env](.env)
