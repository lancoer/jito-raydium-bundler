use std::env;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{format_err, Ok, Result};
use clap::{Parser, Subcommand};
use dotenv_codegen::dotenv;

use jito_protos::searcher::searcher_service_client::SearcherServiceClient;
use jito_protos::searcher::{GetTipAccountsRequest, SubscribeBundleResultsRequest};
use jito_protos::searcher::{GetTipAccountsResponse, NextScheduledLeaderRequest};
use jito_searcher_client::send_bundle_with_confirmation;
use jito_searcher_client::{get_searcher_client_auth, get_searcher_client_no_auth};
use raydium_library::amm;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::system_instruction::transfer;
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction};

use tokio::time::sleep;
use tonic::codegen::{Body, Bytes, StdError};

use env_logger::TimestampPrecision;

// Define App args and commands struct
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct App {
    /// Path to keypair file used to authenticate with the Jito Block Engine
    /// See: https://jito-labs.gitbook.io/mev/searcher-resources/getting-started#block-engine-api-key
    #[arg(long)]
    keypair_path: Option<String>,

    /// Comma-separated list of regions to request cross-region data from.
    /// If no region specified, then default to the currently connected block engine's region.
    /// Details: https://jito-labs.gitbook.io/mev/searcher-services/recommendations#cross-region
    /// Available regions: https://jito-labs.gitbook.io/mev/searcher-resources/block-engine#connection-details
    #[arg(long, value_delimiter = ',')]
    regions: Vec<String>,

    /// Subcommand to run
    #[clap(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    SendBundle {
        /// Openbook market address
        market: Pubkey,
        /// Token base mint address for creating pool (e.g <WSOL mint>)
        token_base_mint: Pubkey,
        /// Token quote mint address for creating pool (e.g <USDC mint>)
        token_quote_mint: Pubkey,
        /// Base token mint amount for creating pool
        init_base_amount: u64,
        /// Quote token mint amount for creating pool
        init_quote_amount: u64,
        /// Pool open time
        #[arg(short, long, default_value_t = 0)]
        open_time: u64,
        /// User token mint address to swap
        input_token_mint: Pubkey,
        /// Token amount user want to get from swap
        amount_specified: u64,
        /// User token mint address to disperse
        disperse_token_mint: Pubkey,
        /// Wallet addresses user to disperse
        #[clap(short, long, value_parser, num_args = 1.., value_delimiter = ' ')]
        disperse_wallets: Vec<Pubkey>,
        /// Total user token amount to disperse
        disperse_amount: u64,
        /// Jito tip
        #[arg(short, long, default_value_t = 1000)]
        jito_tip: u64,
    },
}

// Dotenv config structs
#[derive(Clone, Debug, PartialEq)]
pub struct ClientConfig {
    cluster_url: String,
    wallet_path: String,
    openbook_market_program: Pubkey,
    raydium_amm_program: Pubkey,
    create_fee_destination: Pubkey,
    slippage_bps: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JitoConfig {
    block_engine_url: String,
}

fn read_keypair_file(s: &str) -> Result<Keypair> {
    solana_sdk::signature::read_keypair_file(s).map_err(|_| format_err!("failed to read keypair from {}", s))
}

fn load_cfg() -> Result<(ClientConfig, JitoConfig)> {
    Ok((
        ClientConfig {
            cluster_url: dotenv!("CLUSTER_URL").to_string(),
            wallet_path: dotenv!("WALLET_PATH").to_string(),
            openbook_market_program: Pubkey::from_str(dotenv!("OPENBOOK_MARKET_PROGRAM")).unwrap(),
            raydium_amm_program: Pubkey::from_str(dotenv!("RAYDIUM_AMM_PROGRAM")).unwrap(),
            create_fee_destination: Pubkey::from_str(dotenv!("CREATE_FEE_DESTINATION")).unwrap(),
            slippage_bps: u64::from_str(dotenv!("SLIPPAGE_BPS")).unwrap(),
        },
        JitoConfig {
            block_engine_url: dotenv!("BLOCK_ENGINE_URL").to_string(),
        },
    ))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    let (client_config, jito_config) = load_cfg().unwrap();
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info")
    }
    env_logger::builder().format_timestamp(Some(TimestampPrecision::Micros)).init();

    let app = App::parse();
    let keypair = app
        .keypair_path
        .as_ref()
        .map(|path| Arc::new(read_keypair_file(path.as_str()).expect("parse kp file")));

    match keypair {
        Some(auth_keypair) => {
            let searcher_client_auth = get_searcher_client_auth(jito_config.block_engine_url.as_str(), &auth_keypair).await.expect(
                "Failed to get searcher client with auth. Note: If you don't pass in the auth keypair, we can attempt to connect to the no auth endpoint",
            );

            if let Err(e) = process_commands(app, &client_config, searcher_client_auth).await {
                eprintln!("Error: {:?}", e);
            }
        }
        None => {
            let searcher_client_no_auth = get_searcher_client_no_auth(jito_config.block_engine_url.as_str()).await.expect(
                "Failed to get searcher client with auth. Note: If you don't pass in the auth keypair, we can attempt to connect to the no auth endpoint",
            );
            if let Err(e) = process_commands(app, &client_config, searcher_client_no_auth).await {
                eprintln!("Error: {:?}", e);
            }
        }
    }
}

async fn process_commands<T>(app: App, client_config: &ClientConfig, mut searcher_client: SearcherServiceClient<T>) -> Result<()>
where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static + Clone,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::Future: std::marker::Send,
{
    // cluster params
    let wallet = read_keypair_file(&client_config.wallet_path).map_err(|_| format_err!("failed to read keypair from {}", client_config.wallet_path))?;
    // solana rpc client
    let rpc_client = solana_client::rpc_client::RpcClient::new(client_config.cluster_url.to_string());
    // solana non-blocking rpc clinet
    let nonblocking_rpc_client =
        solana_client::nonblocking::rpc_client::RpcClient::new_with_commitment(client_config.cluster_url.to_string(), CommitmentConfig::confirmed());

    println!();

    match app.command {
        Command::SendBundle {
            market,
            token_base_mint,
            token_quote_mint,
            init_base_amount,
            init_quote_amount,
            open_time,
            input_token_mint,
            amount_specified,
            disperse_token_mint,
            disperse_wallets,
            disperse_amount,
            jito_tip,
        } => {
            /*** parepare initialize pool instructions ***/
            // generate amm keys
            let amm_keys = amm::utils::get_amm_pda_keys(
                &client_config.raydium_amm_program,
                &client_config.openbook_market_program,
                &market,
                &token_base_mint,
                &token_quote_mint,
            )?;

            // build initialize instruction
            let build_init_instruction = amm::instructions::initialize_amm_pool(
                &client_config.raydium_amm_program,
                &amm_keys,
                &client_config.create_fee_destination,
                &wallet.pubkey(),
                &spl_associated_token_account::get_associated_token_address(&wallet.pubkey(), &amm_keys.amm_coin_mint),
                &spl_associated_token_account::get_associated_token_address(&wallet.pubkey(), &amm_keys.amm_pc_mint),
                &spl_associated_token_account::get_associated_token_address(&wallet.pubkey(), &amm_keys.amm_lp_mint),
                open_time,
                init_quote_amount,
                init_base_amount,
            )?;

            /*** parepare snipe token instructions ***/
            // load market keys
            let market_keys = amm::openbook::get_keys_for_market(&rpc_client, &amm_keys.market_program, &amm_keys.market)?;

            // prepare swap instruction
            let (direction, output_token_mint) = if input_token_mint == amm_keys.amm_coin_mint {
                (amm::utils::SwapDirection::Coin2PC, amm_keys.amm_pc_mint)
            } else {
                (amm::utils::SwapDirection::PC2Coin, amm_keys.amm_coin_mint)
            };
            // swap_fee_numberator and swap_fee_denominator default values are specified at
            // https://github.com/raydium-io/raydium-amm/blob/ae039d21cd49ef670d76b3a1cf5485ae0213dc5e/program/src/state.rs#L503
            //
            let other_amount_threshold = amm::swap_with_slippage(
                init_quote_amount,
                init_base_amount,
                25,
                10_1000,
                direction,
                amount_specified,
                false,
                client_config.slippage_bps,
            )?;
            println!("other_amount_threshold: {other_amount_threshold}");

            // build swap instruction
            let build_swap_instruction = amm::instructions::swap(
                &client_config.raydium_amm_program,
                &amm_keys,
                &market_keys,
                &wallet.pubkey(),
                &spl_associated_token_account::get_associated_token_address(&wallet.pubkey(), &input_token_mint),
                &spl_associated_token_account::get_associated_token_address(&wallet.pubkey(), &output_token_mint),
                amount_specified,
                other_amount_threshold,
                false,
            )?;

            /*** prepare disperse token to wallets instructions ***/
            // load account
            let user_disperse_token_account = spl_associated_token_account::get_associated_token_address(&wallet.pubkey(), &disperse_token_mint);
            let load_pubkeys = [user_disperse_token_account];
            let rsps: Vec<Option<solana_sdk::account::Account>> = nonblocking_rpc_client.get_multiple_accounts(&load_pubkeys).await?;
            let disperse_token_program = rsps[0].clone().unwrap().owner;

            // calculate the amount to disperse to each wallet
            let disperse_each_amount = disperse_amount / u64::try_from(disperse_wallets.len())?;
            println!("disperse_each_amount: {disperse_each_amount}\n");

            let mut disperse_wallet_ixs = vec![];
            for disperse_wallet in disperse_wallets {
                let dispserse_token_pubkey = spl_associated_token_account::get_associated_token_address(&disperse_wallet, &disperse_token_mint);
                if !nonblocking_rpc_client.get_account(&dispserse_token_pubkey).await.is_ok() {
                    let create_disperse_token_instr = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                        &wallet.pubkey(),
                        &disperse_wallet,
                        &disperse_token_mint,
                        &disperse_token_program,
                    );
                    disperse_wallet_ixs.extend([create_disperse_token_instr]);
                }
                let transfer_token_instr = spl_token::instruction::transfer(
                    &disperse_token_program,
                    &user_disperse_token_account,
                    &dispserse_token_pubkey,
                    &wallet.pubkey(),
                    &[],
                    disperse_each_amount,
                )?;
                disperse_wallet_ixs.extend([transfer_token_instr]);
            }

            // // build + sign the transactions
            // let (blockhash, _) = nonblocking_rpc_client
            //     .get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
            //     .await
            //     .expect("get blockhash");
            // let signers = [&wallet];

            // let txn = Transaction::new_signed_with_payer(&ixs, Some(&wallet.pubkey()), &signers, blockhash);
            // let signature = nonblocking_rpc_client
            //     .send_and_confirm_transaction_with_spinner_and_config(
            //         &txn,
            //         CommitmentConfig::confirmed(),
            //         solana_client::rpc_config::RpcSendTransactionConfig {
            //             skip_preflight: true,
            //             ..solana_client::rpc_config::RpcSendTransactionConfig::default()
            //         },
            //     )
            //     .await?;
            // println!("{}", signature);

            /*** prepare jito ***/
            // Get tip accounts
            let tip_accounts: GetTipAccountsResponse = searcher_client
                .get_tip_accounts(GetTipAccountsRequest {})
                .await
                .expect("gets connected leaders")
                .into_inner();
            let tip_accounts = tip_accounts.accounts;
            let tip_account = Pubkey::from_str(tip_accounts[0].as_str()).unwrap();

            println!("Chosen #0 of Tip Accounts: {:?}\n", tip_accounts);

            // prepare Jito results output subscription
            let mut bundle_results_subscription = searcher_client
                .subscribe_bundle_results(SubscribeBundleResultsRequest {})
                .await
                .expect("subscribe to bundle results")
                .into_inner();

            // wait for jito-solana leader slot
            let mut is_leader_slot = false;
            while !is_leader_slot {
                let next_leader = searcher_client
                    .get_next_scheduled_leader(NextScheduledLeaderRequest { regions: app.regions.clone() })
                    .await
                    .expect("gets next scheduled leader")
                    .into_inner();
                let num_slots = next_leader.next_leader_slot - next_leader.current_slot;
                is_leader_slot = num_slots <= 2;
                println!("next jito leader slot in {num_slots} slots in {}", next_leader.next_leader_region);
                sleep(Duration::from_millis(500)).await;
            }

            // build + sign the transactions
            let (blockhash, _) = nonblocking_rpc_client
                .get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
                .await
                .expect("get blockhash");
            let signers = [&wallet];

            send_bundle_with_confirmation(
                &[
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &[build_init_instruction],
                        Some(&wallet.pubkey()),
                        &signers,
                        blockhash,
                    )),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &[build_swap_instruction],
                        Some(&wallet.pubkey()),
                        &signers,
                        blockhash,
                    )),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &disperse_wallet_ixs,
                        Some(&wallet.pubkey()),
                        &signers,
                        blockhash,
                    )),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &[transfer(&wallet.pubkey(), &tip_account, jito_tip)],
                        Some(&wallet.pubkey()),
                        &signers,
                        blockhash,
                    )),
                ],
                &nonblocking_rpc_client,
                &mut searcher_client,
                &mut bundle_results_subscription,
            )
            .await
            .expect("Sending bundle failed");
        }
    }

    Ok(())
}
