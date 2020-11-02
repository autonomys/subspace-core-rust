use crate::network::messages::GossipMessage;
use crate::network::Network;
use crate::{NodeID, DEV_WS_ADDR};
use futures::future;
use jsonrpc_core::{MetaIoHandler, Middleware, Params, Value};
use jsonrpc_pubsub::{PubSubHandler, PubSubMetadata, Session, Sink, Subscriber, SubscriptionId};
use jsonrpc_ws_server::{RequestContext, Server, ServerBuilder};
use log::*;
use static_assertions::_core::sync::atomic::AtomicUsize;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

fn add_subscriptions<T, S>(io: &mut PubSubHandler<T, S>, network: Network)
where
    T: PubSubMetadata,
    S: Middleware<T>,
{
    let next_subscription_id = Arc::new(AtomicUsize::new(1));
    let sinks = Arc::<Mutex<HashMap<SubscriptionId, Sink>>>::default();
    io.add_subscription(
        "blocks",
        ("subscribe_blocks", {
            let sinks = Arc::clone(&sinks);

            move |_params: Params, _meta, subscriber: Subscriber| {
                debug!("subscribe_blocks");
                let sinks = Arc::clone(&sinks);

                let subscription_id = SubscriptionId::Number(
                    next_subscription_id.fetch_add(1, Ordering::SeqCst) as u64,
                );
                let sink = subscriber.assign_id(subscription_id.clone()).unwrap();

                sinks.lock().unwrap().insert(subscription_id, sink);
            }
        }),
        ("unsubscribe_blocks", {
            let sinks = Arc::clone(&sinks);

            move |subscription_id: SubscriptionId, _meta| {
                debug!("unsubscribe_blocks");
                let sinks = Arc::clone(&sinks);

                sinks.lock().unwrap().remove(&subscription_id);

                future::ok(Value::Bool(true))
            }
        }),
    );

    network
        .on_gossip(move |message| {
            let sinks = Arc::clone(&sinks);

            match message {
                GossipMessage::BlockProposal { block } => {
                    // TODO: Serialization for numbers may lose u64 precision, also bytes are ugly, we
                    //  probably want to have them as hex.
                    //  https://openethereum.github.io/wiki/JSONRPC
                    let params = Params::Array(vec![serde_json::to_value(block.clone()).unwrap()]);
                    for sink in sinks.lock().unwrap().values() {
                        drop(sink.notify(params.clone()));
                    }
                }
                GossipMessage::TxProposal { tx: _ } => {
                    // TODO
                }
            }
        })
        .detach();
}

pub fn run(node_id: NodeID, network: Network) -> Server {
    let mut io = PubSubHandler::new(MetaIoHandler::default());
    io.add_sync_method("get_node_id", move |_params: Params| {
        Ok(Value::String(hex::encode(&node_id)))
    });
    add_subscriptions(&mut io, network);

    // TODO: CORS origins
    let server = ServerBuilder::new(io)
        .session_meta_extractor(|context: &RequestContext| {
            Some(Arc::new(Session::new(context.sender())))
        })
        .start(&DEV_WS_ADDR.parse().unwrap())
        .expect("Server must start with no issues");

    server
}
