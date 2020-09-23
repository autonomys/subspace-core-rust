use crate::block::Proof;
use crate::{crypto, ProofId, BLOCK_REWARD};
use ed25519_dalek::{Keypair, PublicKey, Signature};
use log::error;
use serde::{Deserialize, Serialize};

/*
   Generate some random credit txs
   Validate, gossip, and add to the mempool
   Include transaction ids in new block
   As new blocks are staged (local or remote)
   For each tx, switch to pending state in mempool (so as not to include again)
   Order the blocks and txs
   Apply transactions to balances (including coinbase)
   Handle forks in the proof chain
   Handle forks in the content chain
   Store the merkle root of account state in the state block
   Store the merkle root of each tx set in the block header?
*/

/// Hash of tx content
pub type TxId = [u8; 32];
/// The hash of a public key
pub type AccountAddress = [u8; 32];
/// Some amount of subspace credits
pub type AccountBalance = u64;
/// An auto-incrementing nonce for each account
pub type TxNonce = u16;
/// Public key used to verify account ownership
pub type TxPublicKey = [u8; 32];
/// Signature of transaction content used to verify account ownership
pub type TxSignature = Vec<u8>;

/// The current state of an individual account (balance and nonce)
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Copy)]
pub struct AccountState {
    /// the amount of credits present in the account
    pub balance: AccountBalance,
    /// the current nonce (to be used for the next tx)
    pub nonce: TxNonce,
}

/// The state of a non-coinbase tx, as tracked in the memory pool
// pub enum TransactionState {
//     /// Recently published and valid, will soon be published in a new block
//     Staged,
//     /// Has been included in at least one valid content block
//     Applied,
//     /// The content block has achieved a depth of k
//     Confirmed,
// }

/// Dynamic transaction type that allows all types to be stored in the same hash map
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum Transaction {
    /// A special purpose transaction that assigns the block reward
    Coinbase(CoinbaseTx),
    /// A standard transaction that sends credits from account A -> account B
    Credit(SimpleCreditTx),
}

/// The first transaction for each set of tx ids in the content block
/// Assigns the block reward to the farmer who produced the block
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct CoinbaseTx {
    /// the current coinbase reward specified by the protocol
    pub reward: AccountBalance,
    /// hash of the public key of the farmer who created the block
    pub to_address: AccountAddress,
    /// hash of the new full block that has been created
    pub proof_id: ProofId,
}

impl CoinbaseTx {
    pub fn new(reward: AccountBalance, public_key: PublicKey, proof_id: ProofId) -> Self {
        let to_address = crypto::digest_sha_256(&public_key.to_bytes());

        CoinbaseTx {
            reward,
            to_address,
            proof_id,
        }
    }

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

    pub fn is_valid(&self, proof: &Proof) -> bool {
        // is the block reward correct
        if self.reward != BLOCK_REWARD {
            error!("Invalid coinbase transaction, incorrect block reward");
            return false;
        }

        // is the proof id correct
        if self.proof_id != proof.get_id() {
            error!("Invalid coinbase transaction, tx references incorrect proof");
            return false;
        }

        // does the to_address match the farmers public key
        if self.to_address != crypto::digest_sha_256(&proof.public_key) {
            error!(
                "Invalid coinbase transaction, reward address does not match farmers public key"
            );
            return false;
        }

        true
    }
}

/// A simple credit transaction from account A to account B
/// Supports single signer only without any time lock
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct SimpleCreditTx {
    /// number of subspace credits being sent in the tx
    pub amount: AccountBalance,
    /// sender address (hash of public key)
    pub from_address: AccountAddress,
    /// recipient address (hash of public key)
    pub to_address: AccountAddress,
    /// sender nonce for this tx (unique and auto-incrementing)
    pub nonce: TxNonce,
    /// sender public key (to verify account and signature)
    pub public_key: TxPublicKey,
    /// signature of unsigned tx with senders private key
    pub signature: Vec<u8>,
}

// TODO: Add a notion of a tx fee for farmers

impl SimpleCreditTx {
    pub fn new(
        amount: AccountBalance,
        to_address: AccountAddress,
        nonce: TxNonce,
        keys: &Keypair,
    ) -> Self {
        let from_address = crypto::digest_sha_256(&keys.public.to_bytes());

        let mut tx = SimpleCreditTx {
            amount,
            from_address,
            to_address,
            nonce,
            public_key: keys.public.to_bytes(),
            signature: Vec::new(),
        };

        let signature = keys.sign(&tx.get_id()).to_bytes().to_vec();

        tx.signature = signature;

        tx
    }

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
            error!("Invalid transaction, from account does not have sufficient funds");
            return false;
        }

        // is the nonce larger than the last nonce
        if from_account_state.unwrap().nonce >= self.nonce {
            error!("Invalid transaction, from account nonce is too low");
            return false;
        }

        // does the senders public key match account address
        if self.from_address != crypto::digest_sha_256(&self.public_key) {
            error!("Invalid transaction, from account address does not match public key");
            return false;
        }

        // is the signature valid
        let mut unsigned_tx = self.clone();
        unsigned_tx.signature = Vec::new();

        let public_key = PublicKey::from_bytes(&self.public_key).unwrap();
        let signature = Signature::from_bytes(&self.signature).unwrap();

        if public_key
            .verify_strict(&unsigned_tx.get_id(), &signature)
            .is_err()
        {
            error!("Invalid transaction, signature does not match public key");
            return false;
        }

        true
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct AdvancedTx {
    pub amount: AccountBalance,
    pub from_address: AccountAddress,
    pub to_address: AccountAddress,
    pub nonce: TxNonce,
    pub spend_conditions: Vec<u8>,
    pub witness: Vec<u8>,
    pub spend_proof: Vec<u8>,
}

pub struct SpendConditions {
    pub version: u16,
}

// spend conditions: 1 of 1 signatures for key with hash at time
// spend proof: signature of message_id with public key

pub struct SpendProof {}

impl AdvancedTx {
    pub fn is_valid() -> bool {
        // do the spend conditions and witness match the from_address

        // does the spend proof satisfy the spend conditions

        true
    }
}

pub struct DataTx {
    pub data: Vec<u8>,
    pub storage_cost: AccountBalance,
    pub from_address: AccountAddress,
    pub nonce: TxNonce,
    pub public_key: PublicKey,
    pub signature: Signature,
}

impl DataTx {
    pub fn is_valid() -> bool {
        // is the data within size range?

        true
    }
}
