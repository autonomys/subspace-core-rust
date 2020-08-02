#![allow(dead_code)]

use super::*;
use crate::Piece;
use crate::PIECE_SIZE;
use async_std::fs::OpenOptions;
use async_std::io::prelude::*;
use async_std::path::PathBuf;
use async_std::task;
use futures::channel::mpsc;
use futures::channel::mpsc::Sender;
use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use futures::SinkExt;
use futures::StreamExt;
use log::error;
use solver::Solution;
use std::collections::HashMap;
use std::convert::TryInto;
use std::io;
use std::io::SeekFrom;
use std::mem;
use std::ops::Deref;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

const INDEX_LENGTH: usize = mem::size_of::<usize>();
const OFFSET_LENGTH: usize = mem::size_of::<u64>();

#[derive(Debug)]
pub enum PlotCreationError {
    PlotOpen(io::Error),
    PlotMapOpen(io::Error),
    MapRead(io::Error),
}

#[derive(Debug)]
enum ReadRequests {
    IsEmpty {
        result_sender: oneshot::Sender<bool>,
    },
    ReadEncoding {
        index: usize,
        result_sender: oneshot::Sender<io::Result<Piece>>,
    },
}

#[derive(Debug)]
enum WriteRequests {
    WriteEncoding {
        encoding: Piece,
        index: usize,
        result_sender: oneshot::Sender<io::Result<()>>,
    },
    RemoveEncoding {
        index: usize,
        result_sender: oneshot::Sender<io::Result<()>>,
    },
    ForceWriteMap {
        result_sender: oneshot::Sender<io::Result<()>>,
    },
}

pub struct Inner {
    any_requests_sender: Sender<()>,
    read_requests_sender: UnboundedSender<ReadRequests>,
    write_requests_sender: UnboundedSender<WriteRequests>,
    updates: Arc<AtomicUsize>,
    update_interval: usize,
}

/* ToDo
 *
 * Return result for solve()
 * Detect if plot exists on startup and load
 * Delete entire plot (perhaps with script) for testing
 * Extend tests
 * Resize plot by removing the last x indices and adjusting struct params
*/

#[derive(Clone)]
pub struct Plot {
    inner: Arc<Inner>,
}

impl Plot {
    /// Creates a new plot for persisting encoded pieces to disk
    pub async fn open_or_create(path: &PathBuf) -> Result<Plot, PlotCreationError> {
        let mut plot_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.join("plot.bin"))
            .await
            .map_err(PlotCreationError::PlotOpen)?;

        let mut map_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.join("plot-map.bin"))
            .await
            .map_err(PlotCreationError::PlotMapOpen)?;

        let mut map = HashMap::new();

        {
            let map_bytes_len = map_file
                .metadata()
                .await
                .map_err(PlotCreationError::MapRead)?
                .len();
            let mut buffer = [0u8; INDEX_LENGTH + OFFSET_LENGTH];
            // let mut buffer = Vec::with_capacity(index_length + offset_length);
            if map_bytes_len > 0 {
                map_file
                    .seek(SeekFrom::Start(0))
                    .await
                    .map_err(PlotCreationError::MapRead)?;
                for _ in (0..map_bytes_len).step_by(INDEX_LENGTH + OFFSET_LENGTH) {
                    if map_file.read_exact(&mut buffer).await.is_err() {
                        error!("Bad map, ignoring remaining bytes");
                        break;
                    }
                    let index =
                        usize::from_le_bytes(buffer[..INDEX_LENGTH].as_ref().try_into().unwrap());
                    let offset =
                        u64::from_le_bytes(buffer[INDEX_LENGTH..].as_ref().try_into().unwrap());
                    map.insert(index, offset);
                }
            }
        }

        // Channel with at most single element to throttle loop below if there are no updates
        let (any_requests_sender, mut any_requests_receiver) = mpsc::channel::<()>(1);
        let (read_requests_sender, mut read_requests_receiver) = mpsc::unbounded::<ReadRequests>();
        let (write_requests_sender, mut write_requests_receiver) =
            mpsc::unbounded::<WriteRequests>();

        // TODO: Handle drop nicer: when read is dropped, make sure writes still all finish
        task::spawn(async move {
            let mut did_nothing = true;
            loop {
                if did_nothing {
                    // Wait for stuff to come in
                    if any_requests_receiver.next().await.is_none() {
                        return;
                    }
                }

                did_nothing = true;

                // Process as many read requests as there is
                while let Ok(read_request) = read_requests_receiver.try_next() {
                    did_nothing = false;

                    match read_request {
                        Some(ReadRequests::IsEmpty { result_sender }) => {
                            let _ = result_sender.send(map.is_empty());
                        }
                        Some(ReadRequests::ReadEncoding {
                            index,
                            result_sender,
                        }) => {
                            let _ = result_sender.send(match map.get(&index) {
                                Some(&position) => {
                                    try {
                                        plot_file.seek(SeekFrom::Start(position)).await?;
                                        let mut buffer = [0u8; PIECE_SIZE];
                                        plot_file.read_exact(&mut buffer).await?;
                                        buffer
                                    }
                                }
                                None => Err(io::Error::from(io::ErrorKind::NotFound)),
                            });
                        }
                        None => {
                            return;
                        }
                    }
                }

                let write_request = write_requests_receiver.try_next();
                if write_request.is_ok() {
                    did_nothing = false;
                }
                // Process at most write request since reading is higher priority
                match write_request {
                    Ok(Some(WriteRequests::WriteEncoding {
                        index,
                        encoding,
                        result_sender,
                    })) => {
                        map.remove(&index);

                        let _ = result_sender.send(
                            try {
                                let position = plot_file.seek(SeekFrom::Current(0)).await?;
                                plot_file.write_all(&encoding).await?;

                                map.insert(index, position);
                            },
                        );
                    }
                    Ok(Some(WriteRequests::RemoveEncoding {
                        index,
                        result_sender,
                    })) => {
                        map.remove(&index);

                        let _ = result_sender.send(Ok(()));
                    }
                    Ok(Some(WriteRequests::ForceWriteMap { result_sender })) => {
                        let _ = result_sender.send(
                            try {
                                map_file.seek(SeekFrom::Start(0)).await?;
                                map_file
                                    .set_len(((INDEX_LENGTH + OFFSET_LENGTH) * map.len()) as u64)
                                    .await?;
                                for (index, offset) in map.iter() {
                                    map_file.write_all(&index.to_le_bytes()).await?;
                                    map_file.write_all(&offset.to_le_bytes()).await?;
                                }
                            },
                        );
                    }
                    Ok(None) => {
                        return;
                    }
                    Err(_) => {
                        // Ignore
                    }
                }
            }
        });

        let updates = Arc::new(AtomicUsize::new(0));
        let update_interval = crate::PLOT_UPDATE_INTERVAL;

        let inner = Inner {
            any_requests_sender,
            read_requests_sender,
            write_requests_sender,
            updates,
            update_interval,
        };

        Ok(Plot {
            inner: Arc::new(inner),
        })
    }

    pub async fn is_empty(&self) -> bool {
        let (result_sender, result_receiver) = oneshot::channel();

        self.read_requests_sender
            .clone()
            .send(ReadRequests::IsEmpty { result_sender })
            .await
            .expect("Failed sending read request");

        // If fails - it is either full or disconnected, we don't care either way, so ignore result
        let _ = self.any_requests_sender.clone().try_send(());

        result_receiver
            .await
            .expect("Read result sender was dropped")
    }

    /// Reads a piece from plot by index
    pub async fn read(&self, index: usize) -> io::Result<Piece> {
        let (result_sender, result_receiver) = oneshot::channel();

        self.read_requests_sender
            .clone()
            .send(ReadRequests::ReadEncoding {
                index,
                result_sender,
            })
            .await
            .expect("Failed sending read encoding request");

        // If fails - it is either full or disconnected, we don't care either way, so ignore result
        let _ = self.any_requests_sender.clone().try_send(());

        result_receiver
            .await
            .expect("Read encoding result sender was dropped")
    }

    /// Writes a piece to the plot by index, will overwrite if piece exists (updates)
    pub async fn write(&self, encoding: Piece, index: usize) -> io::Result<()> {
        let (result_sender, result_receiver) = oneshot::channel();

        self.write_requests_sender
            .clone()
            .send(WriteRequests::WriteEncoding {
                encoding,
                index,
                result_sender,
            })
            .await
            .expect("Failed sending write encoding request");

        // If fails - it is either full or disconnected, we don't care either way, so ignore result
        let _ = self.any_requests_sender.clone().try_send(());

        result_receiver
            .await
            .expect("Write encoding result sender was dropped")?;

        self.handle_update().await
    }

    /// Removes a piece from the plot by index, by deleting its index from the map
    pub async fn remove(&self, index: usize) -> io::Result<()> {
        let (result_sender, result_receiver) = oneshot::channel();

        self.write_requests_sender
            .clone()
            .send(WriteRequests::RemoveEncoding {
                index,
                result_sender,
            })
            .await
            .expect("Failed sending remove encoding request");

        // If fails - it is either full or disconnected, we don't care either way, so ignore result
        let _ = self.any_requests_sender.clone().try_send(());

        result_receiver
            .await
            .expect("Remove encoding result sender was dropped")?;

        self.handle_update().await
    }

    /// Fetches the encoding for an audit and returns the solution with random delay
    ///
    /// Given the target:
    /// Given an expected replication factor (encoding_count) as u32
    /// Compute the target value as 2^ (32 - log(2) encoding count)
    ///
    /// Given a sample:
    /// Given a 256 bit tag
    /// Reduce it to a 32 bit number by taking the first four bytes
    /// Convert to an u32 -> f64 -> take log(2)
    /// Compute exponent as log2(tag) - log2(tgt)
    ///
    /// Compute delay as base_delay * 2^exponent
    ///
    pub async fn solve(
        &self,
        challenge: [u8; 32],
        timestamp: u128,
        piece_count: usize,
        replication_factor: u32,
        target: u32,
    ) -> Vec<Solution> {
        // choose the correct "virtual" piece
        let base_index = utils::modulo(&challenge, piece_count);
        let mut solutions: Vec<Solution> = Vec::new();
        // read each "virtual" encoding of that piece
        for i in 0..replication_factor {
            let index = base_index + (i * replication_factor) as usize;
            let encoding = self.read(index).await.unwrap();
            let tag = crypto::create_hmac(&encoding[..], &challenge);
            let sample = utils::bytes_le_to_u32(&tag[0..4]);
            let distance = (sample as f64).log2() - (target as f64).log2();
            let delay = (TARGET_BLOCK_DELAY * 2f64.powf(distance)) as u32;

            solutions.push(Solution {
                challenge,
                base_time: timestamp,
                index: index as u64,
                tag,
                delay,
                encoding,
            })
        }

        // sort the solutions so that smallest delay is first
        solutions.sort_by_key(|s| s.delay);
        solutions[0..DEGREE_OF_SIMULATION].to_vec()
    }

    /// Writes the map to disk to persist between sessions (does not load on startup yet)
    pub async fn force_write_map(&self) -> io::Result<()> {
        let (result_sender, result_receiver) = oneshot::channel();

        self.write_requests_sender
            .clone()
            .send(WriteRequests::ForceWriteMap { result_sender })
            .await
            .expect("Failed sending force write map request");

        // If fails - it is either full or disconnected, we don't care either way, so ignore result
        let _ = self.any_requests_sender.clone().try_send(());

        result_receiver
            .await
            .expect("Force write map result sender was dropped")?;

        self.updates.store(0, Ordering::Relaxed);

        Ok(())
    }

    /// Increment a counter to persist the map based on some interval
    async fn handle_update(&self) -> io::Result<()> {
        let updates = self.updates.fetch_add(1, Ordering::Relaxed);

        if updates % self.update_interval == 0 {
            self.force_write_map().await?;
        }

        Ok(())
    }
}

impl Deref for Plot {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;
    use async_std::path::PathBuf;

    #[async_std::test]
    async fn test_basic() {
        let path = PathBuf::from("target").join("test");

        let piece = crypto::generate_random_piece();

        let mut plot = Plot::open_or_create(&path).await.unwrap();
        plot.write(piece, 0).await.unwrap();
        let extracted_piece = plot.read(0).await.unwrap();

        assert_eq!(extracted_piece[..], piece[..]);

        plot.force_write_map().await.unwrap();
    }
}
