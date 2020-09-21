use crate::{NodeID, DEV_WS_ADDR};
use jsonrpc_core::IoHandler;
use jsonrpc_ws_server::{Server, ServerBuilder};

mod rpc {
    use crate::NodeID;
    use jsonrpc_core::Result;
    use jsonrpc_derive::rpc;

    #[rpc]
    pub trait Rpc {
        #[rpc(name = "get_node_id")]
        fn get_node_id(&self) -> Result<String>;
    }

    pub struct RpcImpl {
        node_id: NodeID,
    }

    impl Rpc for RpcImpl {
        fn get_node_id(&self) -> Result<String> {
            Ok(hex::encode(&self.node_id))
        }
    }

    impl RpcImpl {
        pub fn new(node_id: NodeID) -> Self {
            Self { node_id }
        }
    }
}

// mod sub {
//     use jsonrpc_core::{Error, ErrorCode, Result};
//     use jsonrpc_derive::rpc;
//     use jsonrpc_pubsub::{
//         typed::{Sink, Subscriber},
//         PubSubHandler, Session, SubscriptionId,
//     };
//     use std::collections::HashMap;
//     use std::sync::atomic::{AtomicUsize, Ordering};
//     use std::sync::{Arc, RwLock};
//
//     #[rpc]
//     pub trait Rpc {
//         type Metadata;
//
//         /// Hello subscription
//         #[pubsub(subscription = "hello", subscribe, name = "hello_subscribe")]
//         fn subscribe(&self, _: Self::Metadata, _: Subscriber<String>, param: u64);
//
//         /// Unsubscribe from hello subscription.
//         #[pubsub(subscription = "hello", unsubscribe, name = "hello_unsubscribe")]
//         fn unsubscribe(&self, _: Option<Self::Metadata>, _: SubscriptionId) -> Result<bool>;
//     }
//
//     #[derive(Default)]
//     pub struct RpcImpl {
//         uid: AtomicUsize,
//         active: Arc<RwLock<HashMap<SubscriptionId, Sink<String>>>>,
//     }
//     impl Rpc for RpcImpl {
//         type Metadata = Arc<Session>;
//
//         fn subscribe(&self, _meta: Self::Metadata, subscriber: Subscriber<String>, param: u64) {
//             if param != 10 {
//                 subscriber
//                     .reject(Error {
//                         code: ErrorCode::InvalidParams,
//                         message: "Rejecting subscription - invalid parameters provided.".into(),
//                         data: None,
//                     })
//                     .unwrap();
//                 return;
//             }
//
//             let id = self.uid.fetch_add(1, Ordering::SeqCst);
//             let sub_id = SubscriptionId::Number(id as u64);
//             let sink = subscriber.assign_id(sub_id.clone()).unwrap();
//             self.active.write().unwrap().insert(sub_id, sink);
//         }
//
//         fn unsubscribe(&self, _meta: Option<Self::Metadata>, id: SubscriptionId) -> Result<bool> {
//             let removed = self.active.write().unwrap().remove(&id);
//             if removed.is_some() {
//                 Ok(true)
//             } else {
//                 Err(Error {
//                     code: ErrorCode::InvalidParams,
//                     message: "Invalid subscription.".into(),
//                     data: None,
//                 })
//             }
//         }
//     }
// }

use rpc::Rpc as HelloRpc;
// use sub::Rpc as SubRpc;

pub fn run(node_id: NodeID) -> Server {
    let mut io = IoHandler::new();
    let rpc = rpc::RpcImpl::new(node_id);
    io.extend_with(rpc.to_delegate());
    // io.extend_with(sub::RpcImpl::default().to_delegate());

    let server = ServerBuilder::new(io)
        .start(&DEV_WS_ADDR.parse().unwrap())
        .expect("Server must start with no issues");

    server
}
