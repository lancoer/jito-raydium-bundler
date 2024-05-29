// use anyhow::{format_err, Result};
// use solana_sdk::pubkey::Pubkey;
// use solana_sdk::signature::Keypair;

// pub mod constants;
// pub mod instructions;

// #[derive(Clone, Debug, PartialEq)]
// pub struct ClientConfig {
//     http_url: String,
//     ws_url: String,
//     payer_path: String,
//     raydium_cp_program: Pubkey,
//     slippage: f64,
// }

// pub fn read_keypair_file(s: &str) -> Result<Keypair> {
//     solana_sdk::signature::read_keypair_file(s).map_err(|_| format_err!("failed to read keypair from {}", s))
// }
