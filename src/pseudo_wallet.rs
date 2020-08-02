use crate::crypto;
use crate::network::NodeID;
use ed25519_dalek::Keypair;
use futures::io::SeekFrom;
use serde::Deserialize;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io;
use std::io::Seek;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct FileContents {
    keypair: String,
}

pub struct Wallet {
    pub keypair: Keypair,
    pub node_id: NodeID,
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

                Ok(Self { keypair, node_id })
            }
            Err(_) => {
                let keypair = crypto::gen_keys_random();
                let node_id = crypto::digest_sha_256(&keypair.public.to_bytes());

                {
                    let keypair = hex::encode(keypair.to_bytes().as_ref());
                    let file_contents = FileContents { keypair };
                    file.seek(SeekFrom::Start(0))?;
                    file.write_all(&serde_json::to_vec(&file_contents).unwrap())?;
                }

                Ok(Self { keypair, node_id })
            }
        }
    }
}
