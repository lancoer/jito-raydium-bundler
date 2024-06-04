# JITO-RAYDIUM-AMM-BUNDLER

Jito Raydium AMM Bundler is a new bundler that deposit liquidity pool, snipe base token and disperse to wallets at ease using Jito CLI.

This is backed by Raydium AMM instructions library.

## Requirements

0. Install Rust
1. Install `protoc`, `gcc`, `openssl`
2. Copy over `.env.example` into `.env`, plug your RPCs (otherwise uses solana default public RPCs)

   - You can set `JITO_BUNDLE_RESULT_WAIT_SECONDS=10` for extending bundle result waiting time, default is 5

3. Copy over `auth.json` - JITO authentication keypair and `id.json` - the keypair with some SOL to fund the endeavour

   - Worring about your private key? Just use [sbjc](https://lib.rs/crates/solana-base58-json-converter) library to convert to json at ease!

4. Enable Rust Nightly and `cargo build --release`

## Usage

### Send the bundle

OpenTime can also be adjusted using `--open-time [n]`, e.g. `--open-time 10000000000` for 10000000000 open-time, default is 0

For cross region functionality, add the `--regions REGION1,REGION2,etc` arg. [More details](https://jito-labs.gitbook.io/mev/searcher-services/recommendations#cross-region)

JitoTip can also be adjusted using `--jito-tip [lamport]`, e.g. `--jito-tip 1000000000` for 0.001 sol, default is 1000

```powershell
# Mainnet (EURC / WSOL)
# cargo run send-bundle -h or --help to see inputs
cargo run -- --keypair-path auth.json send-bundle HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr So11111111111111111111111111111111111111112 1000000000000000000 100000000 <WSOL mint> 800000000000000000 <EURC mint> 300000000 --disperse-wallets 96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5
# Note: Don't provide --keypair-path argument if not planning to use authentication
```

```powershell
# Testnet (USDC-Dev / WSOL)
# cargo run send-bundle -h or --help to see inputs
cargo run send-bundle 4i79W4hgqjdtV9a3dX7DCNdeBaxRRGtKCj67E7aWBRWz Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr So11111111111111111111111111111111111111112 1000000000000000 100000000 So11111111111111111111111111111111111111112 800000000000000 Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr 1000000000000000000 --disperse-wallets B1mrQSpdeMU9gCvkJ6VsXVVoYjRGkNA7TtjMyqxrhecH
# Note: Don't provide --keypair-path argument if not planning to use authentication
```

## Devnet

And ensure you've set devnet url and testnet url for solana, openbook, raydium and jito cli in [.env](.env)
