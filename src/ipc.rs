use async_oneshot::Sender;
use async_std::os::unix::net::UnixListener;
use async_std::path::Path;
use async_std::task::JoinHandle;
use bytes::BytesMut;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::convert::TryInto;
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Serialize, Deserialize)]
pub enum IpcRequestMessage {
    // TODO
}

#[derive(Serialize, Deserialize)]
pub enum IpcResponseMessage {
    // TODO
}

pub struct IpcServer {
    task_handle: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let task_handle = self.task_handle.lock().unwrap().take().unwrap();
        async_std::task::spawn(async move {
            let _ = task_handle.cancel().await;
        });
    }
}

impl IpcServer {
    pub async fn new<P, OM>(path: P, on_message: OM) -> io::Result<IpcServer>
    where
        P: AsRef<Path>,
        OM: Fn((IpcRequestMessage, Sender<IpcResponseMessage>)) + Send + Sync + 'static,
    {
        let listener = UnixListener::bind(path).await?;
        let on_message = Arc::new(on_message);

        let task_handle = async_std::task::spawn(async move {
            let mut connections = listener.incoming();

            while let Some(Ok(mut stream)) = connections.next().await {
                let on_message = Arc::clone(&on_message);

                async_std::task::spawn(async move {
                    let mut length_buffer = BytesMut::with_capacity(4);
                    length_buffer.resize(length_buffer.capacity(), 0);
                    let mut payload = BytesMut::new();

                    while stream.read_exact(length_buffer.as_mut()).await.is_ok() {
                        let length = u32::from_le_bytes(length_buffer.as_ref().try_into().unwrap());
                        payload.resize(length as usize, 0);
                        if stream.read_exact(payload.as_mut()).await.is_err() {
                            // EOF, exit
                            break;
                        }

                        match bincode::deserialize::<IpcRequestMessage>(&payload) {
                            Ok(request_message) => {
                                let (response_sender, response_receiver) = async_oneshot::oneshot();
                                on_message((request_message, response_sender));
                                match response_receiver.await {
                                    Ok(response) => {
                                        let data =
                                            bincode::serialize(&Ok::<_, ()>(response)).unwrap();
                                        let result: io::Result<()> = try {
                                            stream
                                                .write_all(&(data.len() as u32).to_le_bytes())
                                                .await?;
                                            stream.write_all(&data).await?;
                                        };

                                        if let Err(error) = result {
                                            debug!("Failed writing IPC response: {}", error);
                                            break;
                                        }
                                    }
                                    Err(_) => {
                                        warn!("IPC response not handled, response channel closed");
                                    }
                                }
                            }
                            Err(error) => {
                                warn!("Failed to deserialize IPC request: {}", error);

                                let data =
                                    bincode::serialize(&Err::<IpcResponseMessage, _>(())).unwrap();
                                let result: io::Result<()> = try {
                                    stream.write_all(&(data.len() as u32).to_le_bytes()).await?;
                                    stream.write_all(&data).await?;
                                };

                                if let Err(error) = result {
                                    debug!("Failed writing IPC error response: {}", error);
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });

        Ok(IpcServer {
            task_handle: Mutex::new(Some(task_handle)),
        })
    }
}
