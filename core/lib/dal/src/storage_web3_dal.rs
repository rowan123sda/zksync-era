use std::time::Instant;

use zksync_types::{
    get_code_key, get_nonce_key,
    utils::{decompose_full_nonce, storage_key_for_standard_token_balance},
    AccountTreeId, Address, L1BatchNumber, MiniblockNumber, StorageKey,
    FAILED_CONTRACT_DEPLOYMENT_BYTECODE_HASH, H256, U256,
};
use zksync_utils::h256_to_u256;

use crate::{SqlxError, StorageProcessor};

#[derive(Debug)]
pub struct StorageWeb3Dal<'a, 'c> {
    pub(crate) storage: &'a mut StorageProcessor<'c>,
}

impl StorageWeb3Dal<'_, '_> {
    pub async fn get_address_historical_nonce(
        &mut self,
        address: Address,
        block_number: MiniblockNumber,
    ) -> Result<U256, SqlxError> {
        let nonce_key = get_nonce_key(&address);
        let nonce_value = self
            .get_historical_value_unchecked(&nonce_key, block_number)
            .await?;
        let full_nonce = h256_to_u256(nonce_value);
        Ok(decompose_full_nonce(full_nonce).0)
    }

    pub async fn standard_token_historical_balance(
        &mut self,
        token_id: AccountTreeId,
        account_id: AccountTreeId,
        block_number: MiniblockNumber,
    ) -> Result<U256, SqlxError> {
        let key = storage_key_for_standard_token_balance(token_id, account_id.address());
        let balance = self
            .get_historical_value_unchecked(&key, block_number)
            .await?;
        Ok(h256_to_u256(balance))
    }

    /// This method does not check if a block with this number exists in the database.
    /// It will return the current value if the block is in the future.
    pub async fn get_historical_value_unchecked(
        &mut self,
        key: &StorageKey,
        block_number: MiniblockNumber,
    ) -> Result<H256, SqlxError> {
        {
            let started_at = Instant::now();
            // We need to proper distinguish if the value is zero or None
            // for the VM to correctly determine initial writes.
            // So, we accept that the value is None if it's zero and it wasn't initially written at the moment.
            let result = sqlx::query!(
                r#"
                SELECT value
                FROM storage_logs
                WHERE storage_logs.hashed_key = $1 AND storage_logs.miniblock_number <= $2
                ORDER BY storage_logs.miniblock_number DESC, storage_logs.operation_number DESC
                LIMIT 1
                "#,
                key.hashed_key().0.to_vec(),
                block_number.0 as i64
            )
            .fetch_optional(self.storage.conn())
            .await
            .map(|option_row| {
                option_row
                    .map(|row| H256::from_slice(&row.value))
                    .unwrap_or_else(H256::zero)
            });
            metrics::histogram!("dal.request", started_at.elapsed(), "method" => "get_historical_value_unchecked");

            result
        }
    }

    /// Gets the L1 batch number that the miniblock has now or will have in the future (provided
    /// that the node will operate correctly). Assumes that the miniblock is present in the DB;
    /// this is not checked, and if this is false, the returned value will be meaningless.
    pub async fn get_provisional_l1_batch_number_of_miniblock_unchecked(
        &mut self,
        miniblock_number: MiniblockNumber,
    ) -> Result<L1BatchNumber, SqlxError> {
        let row = sqlx::query!(
            "SELECT \
                (SELECT l1_batch_number FROM miniblocks WHERE number = $1) as \"block_batch?\", \
                (SELECT MAX(number) + 1 FROM l1_batches) as \"max_batch?\"",
            miniblock_number.0 as i64
        )
        .fetch_one(self.storage.conn())
        .await?;

        let batch_number = row.block_batch.or(row.max_batch).unwrap_or(0);
        Ok(L1BatchNumber(batch_number as u32))
    }

    pub async fn get_l1_batch_number_for_initial_write(
        &mut self,
        key: &StorageKey,
    ) -> Result<Option<L1BatchNumber>, SqlxError> {
        let started_at = Instant::now();
        let hashed_key = key.hashed_key();
        let row = sqlx::query!(
            "SELECT l1_batch_number FROM initial_writes WHERE hashed_key = $1",
            hashed_key.as_bytes(),
        )
        .fetch_optional(self.storage.conn())
        .await?;

        let l1_batch_number = row.map(|record| L1BatchNumber(record.l1_batch_number as u32));
        metrics::histogram!(
            "dal.request",
            started_at.elapsed(),
            "method" => "get_l1_batch_number_for_initial_write"
        );
        Ok(l1_batch_number)
    }

    /// This method doesn't check if block with number equals to `block_number`
    /// is present in the database. For such blocks `None` will be returned.
    pub async fn get_contract_code_unchecked(
        &mut self,
        address: Address,
        block_number: MiniblockNumber,
    ) -> Result<Option<Vec<u8>>, SqlxError> {
        let hashed_key = get_code_key(&address).hashed_key();
        {
            sqlx::query!(
                "
                    SELECT bytecode FROM (
                        SELECT * FROM storage_logs
                        WHERE
                            storage_logs.hashed_key = $1 AND
                            storage_logs.miniblock_number <= $2
                        ORDER BY
                            storage_logs.miniblock_number DESC, storage_logs.operation_number DESC
                        LIMIT 1
                    ) t
                    JOIN factory_deps ON value = factory_deps.bytecode_hash
                    WHERE value != $3
                ",
                hashed_key.as_bytes(),
                block_number.0 as i64,
                FAILED_CONTRACT_DEPLOYMENT_BYTECODE_HASH.as_bytes(),
            )
            .fetch_optional(self.storage.conn())
            .await
            .map(|option_row| option_row.map(|row| row.bytecode))
        }
    }

    /// This method doesn't check if block with number equals to `block_number`
    /// is present in the database. For such blocks `None` will be returned.
    pub async fn get_factory_dep_unchecked(
        &mut self,
        hash: H256,
        block_number: MiniblockNumber,
    ) -> Result<Option<Vec<u8>>, SqlxError> {
        {
            sqlx::query!(
                "SELECT bytecode FROM factory_deps WHERE bytecode_hash = $1 AND miniblock_number <= $2",
                &hash.0.to_vec(),
                block_number.0 as i64
            )
            .fetch_optional(self.storage.conn())
            .await
            .map(|option_row| option_row.map(|row| row.bytecode))
        }
    }
}
