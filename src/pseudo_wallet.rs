use crate::crypto;
use crate::transaction::{AccountAddress, AccountBalance, SimpleCreditTx, TxNonce};
use crate::NodeID;
use ed25519_dalek::Keypair;
use futures::io::SeekFrom;
use serde::Deserialize;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::Seek;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct FileContents {
    keypair: String,
    #[serde(default)]
    nonce: u16,
}

pub struct Wallet {
    pub keypair: Keypair,
    pub node_id: NodeID,
    nonce: TxNonce,
    file: File,
}

impl Wallet {
    pub fn open_or_create(path: &PathBuf) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.join("config.json"))?;

        let mut file_contents = String::new();
        file.read_to_string(&mut file_contents)?;
        match serde_json::from_str::<FileContents>(&file_contents) {
            Ok(file_contents) => {
                let keypair = Keypair::from_bytes(
                    &hex::decode(file_contents.keypair).expect("Keypair is not a hex string"),
                )
                .expect("Incorrect keypair string");

                let node_id = crypto::digest_sha_256(&keypair.public.to_bytes());

                let nonce = file_contents.nonce;

                Ok(Self {
                    keypair,
                    node_id,
                    nonce,
                    file,
                })
            }
            Err(_) => {
                let keypair = crypto::gen_keys_random();
                let node_id = crypto::digest_sha_256(&keypair.public.to_bytes());
                let nonce = 0;

                let mut wallet = Self {
                    keypair,
                    node_id,
                    nonce,
                    file,
                };

                wallet.save()?;

                Ok(wallet)
            }
        }
    }

    fn save(&mut self) -> io::Result<()> {
        let keypair = hex::encode(self.keypair.to_bytes().as_ref());
        let nonce = self.nonce;
        let file_contents = FileContents { keypair, nonce };
        self.file.seek(SeekFrom::Start(0))?;
        self.file
            .write_all(&serde_json::to_vec(&file_contents).unwrap())?;
        Ok(())
    }

    /// Create a new tx using keys and nonce from wallet
    pub fn create_tx(
        &mut self,
        to: AccountAddress,
        amount: AccountBalance,
    ) -> io::Result<SimpleCreditTx> {
        self.nonce += 1;
        let tx = SimpleCreditTx::new(amount, to, self.nonce, &self.keypair);
        self.save()?;
        Ok(tx)
    }
}
