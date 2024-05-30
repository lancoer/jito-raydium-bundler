mod instructions;

use std::env;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{format_err, Ok, Result};
use clap::{Parser, Subcommand};
use dotenv_codegen::dotenv;

use anchor_client::{Client, Cluster};
use jito_protos::searcher::searcher_service_client::SearcherServiceClient;
use jito_protos::searcher::{GetTipAccountsRequest, SubscribeBundleResultsRequest};
use jito_protos::searcher::{GetTipAccountsResponse, NextScheduledLeaderRequest};
use jito_searcher_client::send_bundle_with_confirmation;
use jito_searcher_client::{get_searcher_client_auth, get_searcher_client_no_auth};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::system_instruction::transfer;
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction};
use spl_memo::{build_memo, solana_program::msg};
use spl_token_2022::{extension::StateWithExtensionsMut, state::*};

use tokio::time::sleep;
use tonic::codegen::{Body, Bytes, StdError};

use env_logger::TimestampPrecision;

use instructions::amm_instructions::*;
use instructions::token_instructions::*;
use instructions::utils::*;

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
        /// Token 0 mint address for creating pool
        token_0_mint: Pubkey,
        /// Token 1 mint address for creating pool
        token_1_mint: Pubkey,
        /// Token 0 mint amount for creating pool
        init_amount_0: u64,
        /// Token 1 mint amount for creating pool
        init_amount_1: u64,
        /// Pool open time
        #[arg(short, long, default_value_t = 0)]
        open_time: u64,
        /// User token vault address to swap
        user_input_token: Pubkey,
        /// User token vault amount to swap
        user_input_amount: u64,
        /// User token vault address to disperse
        user_disperse_token: Pubkey,
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
    http_url: String,
    ws_url: String,
    payer_path: String,
    raydium_cp_program: Pubkey,
    slippage: f64,
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
            http_url: dotenv!("HTTP_URL").to_string(),
            ws_url: dotenv!("WS_URL").to_string(),
            payer_path: dotenv!("PAYER_PATH").to_string(),
            raydium_cp_program: Pubkey::from_str(dotenv!("RAYDIUM_CP_PROGRAM")).unwrap(),
            slippage: f64::from_str(dotenv!("SLIPPAGE")).unwrap(),
        },
        JitoConfig {
            block_engine_url: dotenv!("BLOCK_ENGINE_URL").to_string(),
        },
    ))
}

// #[tokio::main(flavor = "multi_thread", worker_threads = 10)]
#[tokio::main]
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
    let payer = read_keypair_file(&client_config.payer_path)?;
    // solana non-blocking rpc clinet
    let rpc_client = solana_client::nonblocking::rpc_client::RpcClient::new_with_commitment(client_config.http_url.to_string(), CommitmentConfig::confirmed());

    // anchor client.
    let url = Cluster::Custom(client_config.http_url.to_string(), client_config.ws_url.to_string());
    let wallet = read_keypair_file(&client_config.payer_path)?;
    let anchor_client = Client::new(url, Rc::new(wallet));
    let program = anchor_client.program(client_config.raydium_cp_program)?;

    match app.command {
        Command::SendBundle {
            token_0_mint,
            token_1_mint,
            init_amount_0,
            init_amount_1,
            open_time,
            user_input_token,
            user_input_amount,
            user_disperse_token,
            disperse_wallets,
            disperse_amount,
            jito_tip,
        } => {
            /*** prepare create pool instructions ***/
            // ensure pool uniqueness
            let (token_0_mint, token_1_mint, init_amount_0, init_amount_1) = if token_0_mint > token_1_mint {
                (token_1_mint, token_0_mint, init_amount_1, init_amount_0)
            } else {
                (token_0_mint, token_1_mint, init_amount_0, init_amount_1)
            };
            // load account
            let load_pubkeys = vec![token_0_mint, token_1_mint];
            let rsps: Vec<Option<solana_sdk::account::Account>> = rpc_client.get_multiple_accounts(&load_pubkeys).await?;
            let token_0_program = rsps[0].clone().unwrap().owner;
            let token_1_program = rsps[1].clone().unwrap().owner;

            let mut initialize_pool_ixs = vec![build_memo(format!("jito bundle 0: initialize pool").as_bytes(), &[])];
            initialize_pool_ixs.extend(initialize_pool_instr(
                &client_config,
                token_0_mint,
                token_1_mint,
                token_0_program,
                token_1_program,
                spl_associated_token_account::get_associated_token_address(&payer.pubkey(), &token_0_mint),
                spl_associated_token_account::get_associated_token_address(&payer.pubkey(), &token_1_mint),
                raydium_cp_swap::create_pool_fee_reveiver::id(),
                init_amount_0,
                init_amount_1,
                open_time,
            )?);

            /*** Calculate arguments for a new pool ***/
            // Calculate Pool id
            let amm_config_index = 0u16;
            let (amm_config_key, __bump) = Pubkey::find_program_address(
                &[raydium_cp_swap::states::config::AMM_CONFIG_SEED.as_bytes(), &amm_config_index.to_be_bytes()],
                &program.id(),
            );

            let (pool_account_key, __bump) = Pubkey::find_program_address(
                &[
                    raydium_cp_swap::states::pool::POOL_SEED.as_bytes(),
                    amm_config_key.to_bytes().as_ref(),
                    token_0_mint.to_bytes().as_ref(),
                    token_1_mint.to_bytes().as_ref(),
                ],
                &program.id(),
            );
            let (token_0_vault, __bump) = Pubkey::find_program_address(
                &[
                    raydium_cp_swap::states::pool::POOL_VAULT_SEED.as_bytes(),
                    pool_account_key.to_bytes().as_ref(),
                    token_0_mint.to_bytes().as_ref(),
                ],
                &program.id(),
            );
            let (token_1_vault, __bump) = Pubkey::find_program_address(
                &[
                    raydium_cp_swap::states::pool::POOL_VAULT_SEED.as_bytes(),
                    pool_account_key.to_bytes().as_ref(),
                    token_1_mint.to_bytes().as_ref(),
                ],
                &program.id(),
            );
            let (observation_key, __bump) = Pubkey::find_program_address(
                &[
                    raydium_cp_swap::states::oracle::OBSERVATION_SEED.as_bytes(),
                    pool_account_key.to_bytes().as_ref(),
                ],
                &program.id(),
            );
            println!(
                "\npool_id: {pool_account_key}\ntoken_0_vault: {token_0_vault} , token_1_vault: {token_1_vault}\nobservation_account: {observation_key}\n"
            );

            // load account
            let load_pubkeys = vec![amm_config_key, token_0_mint, token_1_mint, user_input_token];
            let rsps: Vec<Option<solana_sdk::account::Account>> = rpc_client.get_multiple_accounts(&load_pubkeys).await?;
            let amm_config_account: &Option<solana_sdk::account::Account> = &rsps[0];
            let token_0_mint_account: &Option<solana_sdk::account::Account> = &rsps[1];
            let token_1_mint_account: &Option<solana_sdk::account::Account> = &rsps[2];
            let user_input_token_account: &Option<solana_sdk::account::Account> = &rsps[3];
            // docode account
            let mut token_0_mint_data = token_0_mint_account.clone().unwrap().data;
            let mut token_1_mint_data = token_1_mint_account.clone().unwrap().data;
            let mut user_input_token_data = user_input_token_account.clone().unwrap().data;
            let amm_config_state = deserialize_anchor_account::<raydium_cp_swap::states::AmmConfig>(amm_config_account.as_ref().unwrap())?;
            let token_0_mint_info = StateWithExtensionsMut::<Mint>::unpack(&mut token_0_mint_data)?;
            let token_1_mint_info = StateWithExtensionsMut::<Mint>::unpack(&mut token_1_mint_data)?;
            let user_input_token_info = StateWithExtensionsMut::<Account>::unpack(&mut user_input_token_data)?;

            /*** prepare snipe token instructions ***/
            let epoch = rpc_client.get_epoch_info().await?.epoch;

            let (
                trade_direction,
                total_input_token_amount,
                total_output_token_amount,
                user_input_token,
                user_output_token,
                input_vault,
                output_vault,
                input_token_mint,
                output_token_mint,
                input_token_program,
                output_token_program,
                transfer_fee,
            ) = if user_input_token_info.base.mint == token_0_mint {
                (
                    raydium_cp_swap::curve::TradeDirection::ZeroForOne,
                    init_amount_0,
                    init_amount_1,
                    user_input_token,
                    spl_associated_token_account::get_associated_token_address(&payer.pubkey(), &token_1_mint),
                    token_0_vault,
                    token_1_vault,
                    token_0_mint,
                    token_1_mint,
                    token_0_program,
                    token_1_program,
                    get_transfer_fee(&token_0_mint_info, epoch, user_input_amount),
                )
            } else {
                (
                    raydium_cp_swap::curve::TradeDirection::OneForZero,
                    init_amount_1,
                    init_amount_0,
                    user_input_token,
                    spl_associated_token_account::get_associated_token_address(&payer.pubkey(), &token_0_mint),
                    token_1_vault,
                    token_0_vault,
                    token_1_mint,
                    token_0_mint,
                    token_1_program,
                    token_0_program,
                    get_transfer_fee(&token_1_mint_info, epoch, user_input_amount),
                )
            };
            // Take transfer fees into account for actual amount transferred in
            let actual_amount_in = user_input_amount.saturating_sub(transfer_fee);
            let result = raydium_cp_swap::curve::CurveCalculator::swap_base_input(
                u128::from(actual_amount_in),
                u128::from(total_input_token_amount),
                u128::from(total_output_token_amount),
                amm_config_state.trade_fee_rate,
                amm_config_state.protocol_fee_rate,
                amm_config_state.fund_fee_rate,
            )
            .ok_or(raydium_cp_swap::error::ErrorCode::ZeroTradingTokens)
            .unwrap();
            let amount_out = u64::try_from(result.destination_amount_swapped).unwrap();
            let transfer_fee = match trade_direction {
                raydium_cp_swap::curve::TradeDirection::ZeroForOne => get_transfer_fee(&token_1_mint_info, epoch, amount_out),
                raydium_cp_swap::curve::TradeDirection::OneForZero => get_transfer_fee(&token_0_mint_info, epoch, amount_out),
            };
            let amount_received = amount_out.checked_sub(transfer_fee).unwrap();
            // calc mint out amount with slippage
            let minimum_amount_out = amount_with_slippage(amount_received, client_config.slippage, false);
            println!("minimum_amount_out: {minimum_amount_out}");

            let mut snipe_token_ixs = vec![build_memo(format!("jito bundle 1: snipe token").as_bytes(), &[])];
            let create_user_output_token_instr = create_ata_token_account_instr(&client_config, spl_token::id(), &output_token_mint, &payer.pubkey())?;
            snipe_token_ixs.extend(create_user_output_token_instr);
            let swap_base_in_instr = swap_base_input_instr(
                &client_config,
                pool_account_key,
                amm_config_key,
                observation_key,
                user_input_token,
                user_output_token,
                input_vault,
                output_vault,
                input_token_mint,
                output_token_mint,
                input_token_program,
                output_token_program,
                user_input_amount,
                minimum_amount_out,
            )?;
            snipe_token_ixs.extend(swap_base_in_instr);

            /*** prepare disperse token to wallets instructions ***/
            // calculate the amount to disperse to each wallet
            let disperse_each_amount = disperse_amount / u64::try_from(disperse_wallets.len())?;

            let mut disperse_wallet_ixs = vec![build_memo(format!("jito bundle 2: disperse token to wallets").as_bytes(), &[])];
            for disperse_wallet in disperse_wallets {
                disperse_wallet_ixs.extend(spl_token_transfer_instr(
                    &client_config,
                    &user_disperse_token,
                    &disperse_wallet,
                    disperse_each_amount,
                    &payer,
                )?);
            }

            /*** prepare jito ***/
            // Get tip accounts
            let tip_accounts: GetTipAccountsResponse = searcher_client
                .get_tip_accounts(GetTipAccountsRequest {})
                .await
                .expect("gets connected leaders")
                .into_inner();
            let tip_accounts = tip_accounts.accounts;
            let tip_account = Pubkey::from_str(tip_accounts[0].as_str()).unwrap();

            msg!("Chosen 0# of Tip Accounts: {:?}", tip_accounts);

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
                msg!("next jito leader slot in {num_slots} slots in {}", next_leader.next_leader_region);
                sleep(Duration::from_millis(500)).await;
            }

            // build + sign the transactions
            let blockhash = rpc_client.get_latest_blockhash().await.expect("get blockhash");
            let signers = [&payer];

            send_bundle_with_confirmation(
                &vec![
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &initialize_pool_ixs,
                        Some(&payer.pubkey()),
                        &signers,
                        blockhash,
                    )),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(&snipe_token_ixs, Some(&payer.pubkey()), &signers, blockhash)),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &disperse_wallet_ixs,
                        Some(&payer.pubkey()),
                        &signers,
                        blockhash,
                    )),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &[
                            build_memo(format!("jito bundle 3: jito tip").as_bytes(), &[]),
                            transfer(&payer.pubkey(), &tip_account, jito_tip),
                        ],
                        Some(&payer.pubkey()),
                        &signers,
                        blockhash,
                    )),
                ],
                &rpc_client,
                &mut searcher_client,
                &mut bundle_results_subscription,
            )
            .await
            .expect("Sending bundle failed");
        }
    }

    Ok(())
}
