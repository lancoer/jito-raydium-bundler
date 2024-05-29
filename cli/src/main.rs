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
use instructions::rpc::send_txn;
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
    InitializePool {
        mint0: Pubkey,
        mint1: Pubkey,
        init_amount_0: u64,
        init_amount_1: u64,
        #[arg(short, long, default_value_t = 0)]
        open_time: u64,
    },
    SendBundle {
        pool_id: Pubkey,
        user_token_0: Pubkey,
        user_token_1: Pubkey,
        lp_token_amount: u64,
        user_input_amount: u64,
        #[clap(short, long, value_parser, num_args = 1.., value_delimiter = ' ')]
        disperse_wallets: Vec<Pubkey>,
        disperse_amount: u64,
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
    let payer = read_keypair_file(&client_config.payer_path)?;
    // solana rpc client
    let rpc_client = solana_client::rpc_client::RpcClient::new(client_config.http_url.to_string());
    // solana non-blocking rpc clinet
    let nonblocking_rpc_client =
        solana_client::nonblocking::rpc_client::RpcClient::new_with_commitment(client_config.http_url.to_string(), CommitmentConfig::confirmed());

    // anchor client.
    let url = Cluster::Custom(client_config.http_url.to_string(), client_config.ws_url.to_string());
    let wallet = read_keypair_file(&client_config.payer_path)?;
    let anchor_client = Client::new(url, Rc::new(wallet));
    let program = anchor_client.program(client_config.raydium_cp_program)?;

    match app.command {
        Command::InitializePool {
            mint0,
            mint1,
            init_amount_0,
            init_amount_1,
            open_time,
        } => {
            let (mint0, mint1, init_amount_0, init_amount_1) = if mint0 > mint1 {
                (mint1, mint0, init_amount_1, init_amount_0)
            } else {
                (mint0, mint1, init_amount_0, init_amount_1)
            };
            let load_pubkeys = vec![mint0, mint1];
            let rsps = rpc_client.get_multiple_accounts(&load_pubkeys).unwrap();
            let token_0_program = rsps[0].clone().unwrap().owner;
            let token_1_program = rsps[1].clone().unwrap().owner;

            let config = client_config.clone();
            let payer_address = payer.pubkey();
            let initialize_pool_ixs = tokio::task::spawn_blocking(move || {
                initialize_pool_instr(
                    &config,
                    mint0,
                    mint1,
                    token_0_program,
                    token_1_program,
                    spl_associated_token_account::get_associated_token_address(&payer_address, &mint0),
                    spl_associated_token_account::get_associated_token_address(&payer_address, &mint1),
                    raydium_cp_swap::create_pool_fee_reveiver::id(),
                    init_amount_0,
                    init_amount_1,
                    open_time,
                )
            })
            .await??;

            let signers = vec![&payer];
            let recent_hash = rpc_client.get_latest_blockhash()?;
            let txn = Transaction::new_signed_with_payer(&initialize_pool_ixs, Some(&payer.pubkey()), &signers, recent_hash);
            let signature = tokio::task::spawn_blocking(move || send_txn(&rpc_client, &txn, true)).await??;
            println!("Signature: {signature}");
        }
        Command::SendBundle {
            pool_id,
            user_token_0,
            user_token_1,
            lp_token_amount,
            user_input_amount,
            disperse_wallets,
            disperse_amount,
            jito_tip,
        } => {
            let user_input_token = user_token_1; // Swap with quote token

            /*** prepare seed lp instructions ***/
            let pool_state = program.account::<raydium_cp_swap::states::PoolState>(pool_id).await?;
            // load account
            let load_pubkeys = vec![
                pool_state.amm_config,
                pool_state.token_0_vault,
                pool_state.token_1_vault,
                pool_state.token_0_mint,
                pool_state.token_1_mint,
                user_input_token,
            ];
            let rsps = rpc_client.get_multiple_accounts(&load_pubkeys)?;
            let amm_config_account: &Option<solana_sdk::account::Account> = &rsps[0];
            let token_0_vault_account: &Option<solana_sdk::account::Account> = &rsps[1];
            let token_1_vault_account: &Option<solana_sdk::account::Account> = &rsps[2];
            let token_0_mint_account: &Option<solana_sdk::account::Account> = &rsps[3];
            let token_1_mint_account: &Option<solana_sdk::account::Account> = &rsps[4];
            let user_input_token_account: &Option<solana_sdk::account::Account> = &rsps[5];
            // docode account
            let mut token_0_vault_data = token_0_vault_account.clone().unwrap().data;
            let mut token_1_vault_data = token_1_vault_account.clone().unwrap().data;
            let mut token_0_mint_data = token_0_mint_account.clone().unwrap().data;
            let mut token_1_mint_data = token_1_mint_account.clone().unwrap().data;
            let mut user_input_token_data = user_input_token_account.clone().unwrap().data;
            let amm_config_state = deserialize_anchor_account::<raydium_cp_swap::states::AmmConfig>(amm_config_account.as_ref().unwrap())?;
            let token_0_vault_info = StateWithExtensionsMut::<Account>::unpack(&mut token_0_vault_data)?;
            let token_1_vault_info = StateWithExtensionsMut::<Account>::unpack(&mut token_1_vault_data)?;
            let token_0_mint_info = StateWithExtensionsMut::<Mint>::unpack(&mut token_0_mint_data)?;
            let token_1_mint_info = StateWithExtensionsMut::<Mint>::unpack(&mut token_1_mint_data)?;
            let user_input_token_info = StateWithExtensionsMut::<Account>::unpack(&mut user_input_token_data)?;

            let (total_token_0_amount, total_token_1_amount) =
                pool_state.vault_amount_without_fee(token_0_vault_info.base.amount, token_1_vault_info.base.amount);

            // calculate amount
            let results = raydium_cp_swap::curve::CurveCalculator::lp_tokens_to_trading_tokens(
                u128::from(lp_token_amount),
                u128::from(pool_state.lp_supply),
                u128::from(total_token_0_amount),
                u128::from(total_token_1_amount),
                raydium_cp_swap::curve::RoundDirection::Ceiling,
            )
            .ok_or(raydium_cp_swap::error::ErrorCode::ZeroTradingTokens)
            .unwrap();
            println!(
                "amount_0:{}, amount_1:{}, lp_token_amount:{}",
                results.token_0_amount, results.token_1_amount, lp_token_amount
            );

            // calc with slippage
            let amount_0_with_slippage = amount_with_slippage(results.token_0_amount as u64, client_config.slippage, true);
            let amount_1_with_slippage = amount_with_slippage(results.token_1_amount as u64, client_config.slippage, true);
            // calc with transfer_fee
            let transfer_fee = get_pool_mints_inverse_fee(
                &rpc_client,
                pool_state.token_0_mint,
                pool_state.token_1_mint,
                amount_0_with_slippage,
                amount_1_with_slippage,
            );
            println!("transfer_fee_0:{}, transfer_fee_1:{}", transfer_fee.0.transfer_fee, transfer_fee.1.transfer_fee);

            let amount_0_max = (amount_0_with_slippage as u64).checked_add(transfer_fee.0.transfer_fee).unwrap();
            let amount_1_max = (amount_1_with_slippage as u64).checked_add(transfer_fee.1.transfer_fee).unwrap();
            println!("amount_0_max:{}, amount_1_max:{}", amount_0_max, amount_1_max);

            let mut seed_lp_ixs = vec![build_memo(format!("jito bundle 0: seed lp").as_bytes(), &[])];
            let create_user_lp_token_instr = create_ata_token_account_instr(&client_config, spl_token::id(), &pool_state.lp_mint, &payer.pubkey())?;
            seed_lp_ixs.extend(create_user_lp_token_instr);
            let deposit_instr = deposit_instr(
                &client_config,
                pool_id,
                pool_state.token_0_mint,
                pool_state.token_1_mint,
                pool_state.lp_mint,
                pool_state.token_0_vault,
                pool_state.token_1_vault,
                user_token_0,
                user_token_1,
                spl_associated_token_account::get_associated_token_address(&payer.pubkey(), &pool_state.lp_mint),
                lp_token_amount,
                amount_0_max,
                amount_1_max,
            )?;
            seed_lp_ixs.extend(deposit_instr);

            /*** prepare snipe token instructions ***/
            let epoch = rpc_client.get_epoch_info().unwrap().epoch;

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
            ) = if user_input_token_info.base.mint == token_0_vault_info.base.mint {
                (
                    raydium_cp_swap::curve::TradeDirection::ZeroForOne,
                    total_token_0_amount,
                    total_token_1_amount,
                    user_input_token,
                    spl_associated_token_account::get_associated_token_address(&payer.pubkey(), &pool_state.token_1_mint),
                    pool_state.token_0_vault,
                    pool_state.token_1_vault,
                    pool_state.token_0_mint,
                    pool_state.token_1_mint,
                    pool_state.token_0_program,
                    pool_state.token_1_program,
                    get_transfer_fee(&token_0_mint_info, epoch, user_input_amount),
                )
            } else {
                (
                    raydium_cp_swap::curve::TradeDirection::OneForZero,
                    total_token_1_amount,
                    total_token_0_amount,
                    user_input_token,
                    spl_associated_token_account::get_associated_token_address(&payer.pubkey(), &pool_state.token_0_mint),
                    pool_state.token_1_vault,
                    pool_state.token_0_vault,
                    pool_state.token_1_mint,
                    pool_state.token_0_mint,
                    pool_state.token_1_program,
                    pool_state.token_0_program,
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

            let mut snipe_token_ixs = vec![build_memo(format!("jito bundle 1: snipe token").as_bytes(), &[])];
            let create_user_output_token_instr = create_ata_token_account_instr(&client_config, spl_token::id(), &output_token_mint, &payer.pubkey())?;
            snipe_token_ixs.extend(create_user_output_token_instr);
            let swap_base_in_instr = swap_base_input_instr(
                &client_config,
                pool_id,
                pool_state.amm_config,
                pool_state.observation_key,
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
                    &user_token_0,
                    &disperse_wallet,
                    disperse_each_amount,
                    &payer,
                )?);
            }

            /*** prepare jito ***/
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
            let signers = vec![&payer];
            let recent_hash = nonblocking_rpc_client.get_latest_blockhash().await?;
            let tip_accounts: GetTipAccountsResponse = searcher_client
                .get_tip_accounts(GetTipAccountsRequest {})
                .await
                .expect("gets connected leaders")
                .into_inner();
            let tip_accounts = tip_accounts.accounts;
            let tip_account = Pubkey::from_str(tip_accounts[0].as_str()).unwrap();

            msg!("Chosen 0# of Tip Accounts: {:?}", tip_accounts);

            send_bundle_with_confirmation(
                &[
                    VersionedTransaction::from(Transaction::new_signed_with_payer(&seed_lp_ixs, Some(&payer.pubkey()), &signers, recent_hash)),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &snipe_token_ixs,
                        Some(&payer.pubkey()),
                        &signers,
                        recent_hash,
                    )),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &disperse_wallet_ixs,
                        Some(&payer.pubkey()),
                        &signers,
                        recent_hash,
                    )),
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &vec![
                            build_memo(format!("jito bundle 3: jito tip").as_bytes(), &[]),
                            transfer(&payer.pubkey(), &tip_account, jito_tip),
                        ],
                        Some(&payer.pubkey()),
                        &signers,
                        recent_hash,
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
