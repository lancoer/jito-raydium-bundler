use anyhow::Result;
use solana_client::{rpc_client::RpcClient, rpc_config::RpcSendTransactionConfig};
use solana_sdk::{commitment_config::CommitmentConfig, signature::Signature, transaction::Transaction};

pub fn send_txn(client: &RpcClient, txn: &Transaction, wait_confirm: bool) -> Result<Signature> {
    Ok(client.send_and_confirm_transaction_with_spinner_and_config(
        txn,
        if wait_confirm {
            CommitmentConfig::confirmed()
        } else {
            CommitmentConfig::processed()
        },
        RpcSendTransactionConfig {
            skip_preflight: true,
            ..RpcSendTransactionConfig::default()
        },
    )?)
}
