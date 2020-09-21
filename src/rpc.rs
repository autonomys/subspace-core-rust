use crate::network::messages::GossipMessage;
use crate::network::Network;
use crate::{NodeID, DEV_WS_ADDR};
use async_channel::Sender;
use futures::lock::Mutex as AsyncMutex;
use futures::{executor, future, StreamExt};
use jsonrpc_core::{MetaIoHandler, Middleware, Params, Value};
use jsonrpc_pubsub::{PubSubHandler, PubSubMetadata, Session, Subscriber, SubscriptionId};
use jsonrpc_ws_server::{RequestContext, Server, ServerBuilder};
use log::*;
use static_assertions::_core::sync::atomic::AtomicUsize;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn add_subscriptions<T, S>(io: &mut PubSubHandler<T, S>, network: Network)
where
    T: PubSubMetadata,
    S: Middleware<T>,
{
    let next_subscription_id = Arc::new(AtomicUsize::new(1));
    let subscribers = Arc::<AsyncMutex<HashMap<SubscriptionId, Sender<Params>>>>::default();
    io.add_subscription(
        "blocks",
        ("subscribe_blocks", {
            let subscribers = Arc::clone(&subscribers);

            move |_params: Params, _meta, subscriber: Subscriber| {
                debug!("subscribe_blocks");
                let subscribers = Arc::clone(&subscribers);

                let subscription_id = SubscriptionId::Number(
                    next_subscription_id.fetch_add(1, Ordering::SeqCst) as u64,
                );
                let sink = subscriber.assign_id(subscription_id.clone()).unwrap();

                executor::block_on(async move {
                    let (sender, mut receiver) = async_channel::unbounded::<Params>();
                    subscribers.lock().await.insert(subscription_id, sender);
                    async_std::task::spawn(async move {
                        while let Some(params) = receiver.next().await {
                            // TODO: This is probably not very efficient to serialize every time
                            if !sink.notify(params).is_ok() {
                                break;
                            }
                        }
                    });
                });
            }
        }),
        ("unsubscribe_blocks", {
            let subscribers = Arc::clone(&subscribers);

            move |subscription_id: SubscriptionId, _meta| {
                debug!("unsubscribe_blocks");
                let subscribers = Arc::clone(&subscribers);

                executor::block_on(async move {
                    subscribers.lock().await.remove(&subscription_id);
                });

                future::ok(Value::Bool(true))
            }
        }),
    );

    executor::block_on(network.connect_gossip(move |message| {
        let subscribers = Arc::clone(&subscribers);

        match message {
            GossipMessage::BlockProposal { block } => {
                let params = Params::Array(vec![serde_json::to_value(block.clone()).unwrap()]);
                async_std::task::spawn(async move {
                    for subscriber in subscribers.lock().await.values() {
                        drop(subscriber.send(params.clone()).await);
                    }
                });
            }
            GossipMessage::TxProposal { tx: _ } => {
                // TODO
            }
        }
    }));
}

pub fn run(node_id: NodeID, network: Network) -> Server {
    let mut io = PubSubHandler::new(MetaIoHandler::default());
    io.add_sync_method("get_node_id", move |_params: Params| {
        Ok(Value::String(hex::encode(&node_id)))
    });
    add_subscriptions(&mut io, network);

    let server = ServerBuilder::new(io)
        .session_meta_extractor(|context: &RequestContext| {
            Some(Arc::new(Session::new(context.sender())))
        })
        .start(&DEV_WS_ADDR.parse().unwrap())
        .expect("Server must start with no issues");

    server
}
