use crate::state::{NetworkPieceBundleByIndex, PieceBundle};
use crate::{
    crypto, sloth, NodeID, Piece, PieceId, PieceIndex, Tag, ENCODING_LAYERS_TEST, PIECE_SIZE,
    PRIME_SIZE_BITS,
};
use async_std::fs::OpenOptions;
use async_std::path::PathBuf;
use async_std::task;
use event_listener_primitives::{Bag, HandlerId};
use futures::channel::mpsc as async_mpsc;
use futures::channel::oneshot;
use futures::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SinkExt, StreamExt};
use log::*;
use rocksdb::IteratorMode;
use rocksdb::DB;
use rug::integer::Order;
use rug::Integer;
use std::convert::TryInto;
use std::io;
use std::io::SeekFrom;
use std::ops::Deref;
use std::sync::Arc;

/*
   Plot File -> all encodings
   Map DB -> (K: index, V: (position, merkle_proof))
   Tags DB -> (K: tag_prefix, V: index)

   FindByRange(target, range) -> Vec<Tag, index>
   Read(index) -> Encoding
   Write(encoding, nonce, index) -> Result()
   Remove(index) -> Result()
*/

#[derive(Debug)]
pub enum PlotCreationError {
    PlotOpen(io::Error),
    PlotMapOpen(rocksdb::Error),
    PlotTagsOpen(rocksdb::Error),
    MapRead(io::Error),
}

#[derive(Debug)]
enum ReadRequests {
    IsEmpty {
        result_sender: oneshot::Sender<bool>,
    },
    ReadEncoding {
        index: u64,
        result_sender: oneshot::Sender<io::Result<(Piece, Vec<u8>)>>,
    },
    FindByRange {
        target: Tag,
        range: u64,
        result_sender: oneshot::Sender<io::Result<Vec<(Tag, u64)>>>,
    },
}

#[derive(Debug)]
enum WriteRequests {
    WriteEncoding {
        encoding: Piece,
        nonce: u64,
        index: u64,
        merkle_proof: Vec<u8>,
        result_sender: oneshot::Sender<io::Result<()>>,
    },
    RemoveEncoding {
        index: u64,
        result_sender: oneshot::Sender<io::Result<()>>,
    },
}

#[derive(Default)]
struct Handlers {
    close: Bag<'static, dyn FnOnce() + Send>,
}

pub struct Inner {
    handlers: Arc<Handlers>,
    any_requests_sender: async_mpsc::Sender<()>,
    read_requests_sender: async_mpsc::UnboundedSender<ReadRequests>,
    write_requests_sender: async_mpsc::UnboundedSender<WriteRequests>,
}

/* ToDo
 *
 * Return result for solve()
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

        let map_db = Arc::new(
            // DB::open_default(path.join("plot-map.rocksdb").to_str().unwrap())
            DB::open_default(path.join("plot-map")).map_err(PlotCreationError::PlotMapOpen)?,
        );

        let tags_db = Arc::new(
            // DB::open_default(path.join("plot-map.rocksdb").to_str().unwrap())
            DB::open_default(path.join("plot-tags")).map_err(PlotCreationError::PlotTagsOpen)?,
        );

        // Channel with at most single element to throttle loop below if there are no updates
        let (any_requests_sender, mut any_requests_receiver) = async_mpsc::channel::<()>(1);
        let (read_requests_sender, mut read_requests_receiver) =
            async_mpsc::unbounded::<ReadRequests>();
        let (write_requests_sender, mut write_requests_receiver) =
            async_mpsc::unbounded::<WriteRequests>();

        let handlers = Arc::new(Handlers::default());

        // TODO: Handle drop nicer: when read is dropped, make sure writes still all finish
        task::spawn({
            let handlers = Arc::clone(&handlers);

            async move {
                let mut did_nothing = true;
                'outer: loop {
                    if did_nothing {
                        // Wait for stuff to come in
                        if any_requests_receiver.next().await.is_none() {
                            break;
                        }
                    }

                    did_nothing = true;

                    // Process as many read requests as there is
                    while let Ok(read_request) = read_requests_receiver.try_next() {
                        did_nothing = false;

                        match read_request {
                            Some(ReadRequests::IsEmpty { result_sender }) => {
                                let _ = result_sender.send(
                                    task::spawn_blocking({
                                        let map_db = Arc::clone(&map_db);
                                        move || {
                                            map_db.iterator(IteratorMode::Start).next().is_none()
                                        }
                                    })
                                    .await,
                                );
                            }
                            Some(ReadRequests::ReadEncoding {
                                index,
                                result_sender,
                            }) => {
                                // TODO: Remove unwrap
                                let piece_data: Option<(u64, Vec<u8>)> = task::spawn_blocking({
                                    let map_db = Arc::clone(&map_db);
                                    move || map_db.get(index.to_le_bytes())
                                })
                                .await
                                .unwrap()
                                .map(|raw_piece_data| {
                                    let position = u64::from_le_bytes(
                                        raw_piece_data[0..8].try_into().unwrap(),
                                    );
                                    let merkle_proof = raw_piece_data[8..].to_vec();
                                    (position, merkle_proof)
                                });

                                let _ = result_sender.send(match piece_data {
                                    Some((position, merkle_proof)) => {
                                        try {
                                            plot_file.seek(SeekFrom::Start(position)).await?;
                                            let mut buffer = [0u8; PIECE_SIZE];
                                            plot_file.read_exact(&mut buffer).await?;
                                            (buffer, merkle_proof)
                                        }
                                    }
                                    None => Err(io::Error::from(io::ErrorKind::NotFound)),
                                });
                            }
                            None => {
                                break 'outer;
                            }
                            Some(ReadRequests::FindByRange {
                                target,
                                range,
                                result_sender,
                            }) => {
                                // TODO: Remove unwrap
                                let solutions = task::spawn_blocking({
                                    let tags_db = Arc::clone(&tags_db);
                                    move || {
                                        let mut iter = tags_db.raw_iterator();

                                        let mut solutions: Vec<(Tag, u64)> = Vec::new();

                                        let (lower, is_lower_overflowed) =
                                            u64::from_be_bytes(target).overflowing_sub(range / 2);
                                        let (upper, is_upper_overflowed) =
                                            u64::from_be_bytes(target).overflowing_add(range / 2);

                                        trace!(
                                            "{} Lower overflow: {} -- Upper overflow: {}",
                                            hex::encode(&target),
                                            is_lower_overflowed,
                                            is_upper_overflowed
                                        );

                                        if is_lower_overflowed || is_upper_overflowed {
                                            iter.seek_to_first();
                                            while let Some(tag) = iter.key() {
                                                let tag = tag.try_into().unwrap();
                                                let index = iter.value().unwrap();
                                                if u64::from_be_bytes(tag) <= upper {
                                                    solutions.push((
                                                        tag,
                                                        u64::from_le_bytes(
                                                            index.try_into().unwrap(),
                                                        ),
                                                    ));
                                                    iter.next();
                                                } else {
                                                    break;
                                                }
                                            }
                                            iter.seek(lower.to_be_bytes());
                                            while let Some(tag) = iter.key() {
                                                let tag = tag.try_into().unwrap();
                                                let index = iter.value().unwrap();

                                                solutions.push((
                                                    tag,
                                                    u64::from_le_bytes(index.try_into().unwrap()),
                                                ));
                                                iter.next();
                                            }
                                        } else {
                                            iter.seek(lower.to_be_bytes());
                                            while let Some(tag) = iter.key() {
                                                let tag = tag.try_into().unwrap();
                                                let index = iter.value().unwrap();
                                                if u64::from_be_bytes(tag) <= upper {
                                                    solutions.push((
                                                        tag,
                                                        u64::from_le_bytes(
                                                            index.try_into().unwrap(),
                                                        ),
                                                    ));
                                                    iter.next();
                                                } else {
                                                    break;
                                                }
                                            }
                                        }

                                        solutions
                                    }
                                })
                                .await;

                                let _ = result_sender.send(Ok(solutions));
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
                            nonce,
                            encoding,
                            merkle_proof,
                            result_sender,
                        })) => {
                            // TODO: remove unwrap
                            task::spawn_blocking({
                                let map_db = Arc::clone(&map_db);
                                move || map_db.delete(index.to_le_bytes())
                            })
                            .await
                            .unwrap();

                            let _ = result_sender.send(
                                try {
                                    let position = plot_file.seek(SeekFrom::End(0)).await?;
                                    plot_file.write_all(&encoding).await?;

                                    // TODO: remove unwrap
                                    task::spawn_blocking({
                                        let map_db = Arc::clone(&map_db);
                                        let tags_db = Arc::clone(&tags_db);
                                        let tag =
                                            crypto::create_hmac(&encoding, &nonce.to_le_bytes());
                                        move || {
                                            tags_db.put(&tag[0..8], index.to_le_bytes()).and_then(
                                                |_| {
                                                    // TODO: may want to put version field of the plotting software here
                                                    let value = &[
                                                        &position.to_le_bytes()[..],
                                                        &merkle_proof[..],
                                                    ]
                                                    .concat();
                                                    map_db.put(index.to_le_bytes(), value)
                                                },
                                            )
                                        }
                                    })
                                    .await
                                    .unwrap();
                                },
                            );
                        }
                        Ok(Some(WriteRequests::RemoveEncoding {
                            index,
                            result_sender,
                        })) => {
                            // TODO: remove unwrap
                            task::spawn_blocking({
                                let map_db = Arc::clone(&map_db);
                                move || map_db.delete(index.to_le_bytes())
                            })
                            .await
                            .unwrap();

                            let _ = result_sender.send(Ok(()));
                        }
                        Ok(None) => {
                            break 'outer;
                        }
                        Err(_) => {
                            // Ignore
                        }
                    }
                }

                std::thread::spawn({
                    let handlers = Arc::clone(&handlers);

                    move || {
                        drop(map_db);
                        drop(tags_db);

                        handlers.close.call_once_simple();
                    }
                });
            }
        });

        let inner = Inner {
            handlers,
            any_requests_sender,
            read_requests_sender,
            write_requests_sender,
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
    pub async fn read(&self, index: u64) -> io::Result<(Piece, Vec<u8>)> {
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

    pub async fn get_piece_bundle_by_index(
        &self,
        index: PieceIndex,
        node_id: NodeID,
    ) -> Option<NetworkPieceBundleByIndex> {
        match self.read(index).await {
            Ok((encoding, merkle_proof)) => Some(NetworkPieceBundleByIndex {
                encoding: encoding.to_vec(),
                piece_proof: merkle_proof,
                node_id,
            }),
            Err(_) => None,
        }
    }

    pub async fn _get_piece_bundle_by_id(&self, _id: PieceId) {
        // get index from id -- needs a new table
        // then call get piece_bundle_from_index
        // needs to include the index
    }

    pub async fn find_by_range(&self, target: [u8; 8], range: u64) -> io::Result<Vec<(Tag, u64)>> {
        let (result_sender, result_receiver) = oneshot::channel();

        self.read_requests_sender
            .clone()
            .send(ReadRequests::FindByRange {
                target,
                range,
                result_sender,
            })
            .await
            .expect("Failed sending get by range request");

        // If fails - it is either full or disconnected, we don't care either way, so ignore result
        let _ = self.any_requests_sender.clone().try_send(());

        result_receiver
            .await
            .expect("Get by range result sender was dropped")
    }

    /// Writes a piece to the plot by index, will overwrite if piece exists (updates)
    pub async fn write(
        &self,
        encoding: Piece,
        nonce: u64,
        index: u64,
        merkle_proof: Vec<u8>,
    ) -> io::Result<()> {
        let (result_sender, result_receiver) = oneshot::channel();

        self.write_requests_sender
            .clone()
            .send(WriteRequests::WriteEncoding {
                encoding,
                nonce,
                index,
                result_sender,
                merkle_proof,
            })
            .await
            .expect("Failed sending write encoding request");

        // If fails - it is either full or disconnected, we don't care either way, so ignore result
        let _ = self.any_requests_sender.clone().try_send(());

        result_receiver
            .await
            .expect("Write encoding result sender was dropped")
    }

    pub async fn plot_pieces(&self, node_id: NodeID, piece_bundles: Vec<PieceBundle>) {
        let expanded_iv = crypto::expand_iv(node_id);
        let integer_expanded_iv = Arc::new(Integer::from_digits(&expanded_iv, Order::Lsf));
        let sloth = Arc::new(sloth::Sloth::init(PRIME_SIZE_BITS));

        for piece_bundle in piece_bundles.into_iter() {
            let mut piece = piece_bundle.piece;

            // TODO: encode pieces first, then add to plot

            let piece = task::spawn_blocking({
                let sloth = Arc::clone(&sloth);
                let integer_expanded_iv = Arc::clone(&integer_expanded_iv);
                move || {
                    sloth
                        .encode(&mut piece, &integer_expanded_iv, ENCODING_LAYERS_TEST)
                        .unwrap();
                    piece
                }
            })
            .await;

            // TODO: Replace challenge here and in other places
            let nonce = u64::from_le_bytes(
                crypto::create_hmac(&piece, b"subspace")[0..8]
                    .try_into()
                    .unwrap(),
            );

            let result = self
                .write(
                    piece,
                    nonce,
                    piece_bundle.piece_index,
                    piece_bundle.piece_proof,
                )
                .await;

            if let Err(error) = result {
                warn!("{}", error);
            }
        }
    }

    /// Removes a piece from the plot by index, by deleting its index from the map
    pub async fn remove(&self, index: u64) -> io::Result<()> {
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
            .expect("Remove encoding result sender was dropped")
    }

    pub fn on_close<F: FnOnce() + Send + 'static>(&self, callback: F) -> HandlerId {
        self.inner.handlers.close.add(Box::new(callback))
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
    use rand::prelude::*;
    use std::fs;
    use std::time::Duration;

    struct TargetDirectory {
        path: PathBuf,
    }

    impl Drop for TargetDirectory {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.path));
        }
    }

    impl Deref for TargetDirectory {
        type Target = PathBuf;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl TargetDirectory {
        fn new(test_name: &str) -> Self {
            let path = PathBuf::from("target").join(test_name);

            fs::create_dir_all(&path).unwrap();

            Self { path }
        }
    }

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[async_std::test]
    async fn test_read_write() {
        init();
        let path = TargetDirectory::new("read_write");

        let piece = crypto::generate_random_piece();
        let nonce = rand::thread_rng().gen::<u64>();
        let index = 0;
        let merkle_proof = vec![0u8; 256];

        let plot = Plot::open_or_create(&path).await.unwrap();
        assert_eq!(true, plot.is_empty().await);
        plot.write(piece, nonce, index, merkle_proof.clone())
            .await
            .unwrap();
        assert_eq!(false, plot.is_empty().await);
        let (extracted_piece, extracted_merkle_proof) = plot.read(index).await.unwrap();

        assert_eq!(piece[..], extracted_piece[..]);
        assert_eq!(merkle_proof, extracted_merkle_proof);

        drop(plot);

        async_std::task::sleep(Duration::from_millis(100)).await;

        // Make sure it is still not empty on reopen
        let plot = Plot::open_or_create(&path).await.unwrap();
        assert_eq!(false, plot.is_empty().await);
        drop(plot);

        // Let plot to destroy gracefully, otherwise may get "pure virtual method called
        // terminate called without an active exception" message
        async_std::task::sleep(Duration::from_millis(100)).await;
    }

    #[async_std::test]
    async fn test_find_by_tag() {
        init();
        let path = TargetDirectory::new("find_by_tag");

        let plot = Plot::open_or_create(&path).await.unwrap();
        for index in 0..1024 {
            let piece = crypto::generate_random_piece();
            let merkle_proof = vec![0u8; 256];
            let nonce = rand::thread_rng().gen::<u64>();
            plot.write(piece, nonce, index, merkle_proof).await.unwrap();
        }

        {
            let target = [0u8, 0, 0, 0, 0, 0, 0, 1];
            let solution_range =
                u64::from_be_bytes([0u8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
            let solutions = plot.find_by_range(target, solution_range).await.unwrap();
            // This is probabilistic, but should be fine most of the time
            assert!(!solutions.is_empty());
            // Wraps around
            let lower = u64::from_be_bytes(target).wrapping_sub(solution_range / 2);
            let upper = u64::from_be_bytes(target) + solution_range / 2;
            for (solution, _) in solutions {
                let solution = u64::from_be_bytes(solution);
                assert!(
                    solution >= lower || solution <= upper,
                    "Solution {:?} must be over wrapped lower edge {:?} or under upper edge {:?}",
                    solution.to_be_bytes(),
                    lower.to_be_bytes(),
                    upper.to_be_bytes(),
                );
            }
        }

        {
            let target = [0xff_u8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe];
            let solution_range =
                u64::from_be_bytes([0u8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
            let solutions = plot.find_by_range(target, solution_range).await.unwrap();
            // This is probabilistic, but should be fine most of the time
            assert!(!solutions.is_empty());
            // Wraps around
            let lower = u64::from_be_bytes(target) - solution_range / 2;
            let upper = u64::from_be_bytes(target).wrapping_add(solution_range / 2);
            for (solution, _) in solutions {
                let solution = u64::from_be_bytes(solution);
                assert!(
                    solution >= lower || solution <= upper,
                    "Solution {:?} must be over lower edge {:?} or under wrapped upper edge {:?}",
                    solution.to_be_bytes(),
                    lower.to_be_bytes(),
                    upper.to_be_bytes(),
                );
            }
        }

        {
            let target = [0xef_u8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
            let solution_range =
                u64::from_be_bytes([0u8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
            let solutions = plot.find_by_range(target, solution_range).await.unwrap();
            // This is probabilistic, but should be fine most of the time
            assert!(!solutions.is_empty());
            let lower = u64::from_be_bytes(target) - solution_range / 2;
            let upper = u64::from_be_bytes(target) + solution_range / 2;
            for (solution, _) in solutions {
                let solution = u64::from_be_bytes(solution);
                assert!(
                    solution >= lower && solution <= upper,
                    "Solution {:?} must be over lower edge {:?} and under upper edge {:?}",
                    solution.to_be_bytes(),
                    lower.to_be_bytes(),
                    upper.to_be_bytes(),
                );
            }
        }

        drop(plot);

        // Let plot to destroy gracefully, otherwise may get "pure virtual method called
        // terminate called without an active exception" message
        async_std::task::sleep(Duration::from_millis(100)).await;
    }
}
