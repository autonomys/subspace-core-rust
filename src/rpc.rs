use crate::DEV_WS_ADDR;
use jsonrpc_core::{IoHandler, Params, Result};
use jsonrpc_derive::rpc;
use jsonrpc_ws_server::{Server, ServerBuilder};
use serde_json::Value;

#[rpc]
pub trait Rpc {
    #[rpc(name = "say_hello")]
    fn say_hello(&self) -> Result<String>;
}

pub struct RpcImpl;
impl Rpc for RpcImpl {
    fn say_hello(&self) -> Result<String> {
        Ok("hello".into())
    }
}

pub fn run() -> Server {
    let mut io = IoHandler::new();
    io.extend_with(RpcImpl.to_delegate());

    let server = ServerBuilder::new(io)
        .start(&DEV_WS_ADDR.parse().unwrap())
        .expect("Server must start with no issues");

    server
}
