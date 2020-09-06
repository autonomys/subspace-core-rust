use crate::crypto;
use log::error;
use serde::{Deserialize, Serialize};

pub type TxId = [u8; 32];
pub type AccountAddress = [u8; 32];
pub type AccountBalance = u64;
pub type TxNonce = u16;

pub struct AccountState {
    pub balance: AccountBalance,
    pub nonce: TxNonce,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Tx {
    pub amount: AccountBalance,
    pub from_address: AccountAddress,
    pub to_address: AccountAddress,
    pub nonce: TxNonce,
    pub spend_conditions: Vec<u8>,
    pub witness: Vec<u8>,
    pub spend_proof: Vec<u8>,
    pub data: Vec<u8>,
}

pub struct SpendConditions {
    pub version: u16,
}

// spend conditions: 1 of 1 signatures for key with hash at time
// spend proof: signature of message_id with public key

pub struct SpendProof {}

impl Tx {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        bincode::deserialize(bytes).map_err(|error| {
            error!("Failed to deserialize transaction: {}", error);
        })
    }

    pub fn get_id(&self) -> TxId {
        crypto::digest_sha_256(&self.to_bytes())
    }

    pub fn is_valid(&self, from_account_state: Option<&AccountState>) -> bool {
        // does the account exist
        if from_account_state.is_none() {
            error!("Invalid transaction, from account does not exist");
            return false;
        }

        // does the balance have the required funds
        if from_account_state.unwrap().balance < self.amount {
            error!("Invalid transaction, from account doees not have sufficient funds");
            return false;
        }

        // is the nonce larger than the last nonce
        if from_account_state.unwrap().nonce >= self.nonce {
            error!("Invalid transaction, from account nonce is too low");
            return false;
        }

        // is the data within size range?

        // do the spend conditions and witness match the from_address

        // does the spend proof satisfy the spend conditions

        true
    }
}
