#![allow(dead_code)]

use super::*;
use crate::{Piece, PIECE_SIZE};
use async_std::fs::File;
use async_std::fs::OpenOptions;
use async_std::io::prelude::*;
use async_std::path::PathBuf;
use log::error;
use solver::Solution;
use std::collections::HashMap;
use std::convert::TryInto;
use std::io::SeekFrom;
use std::{io, mem};

const INDEX_LENGTH: usize = mem::size_of::<usize>();
const OFFSET_LENGTH: usize = mem::size_of::<u64>();

#[derive(Debug)]
pub enum PlotCreationError {
    PlotOpen(io::Error),
    PlotMapOpen(io::Error),
    MapRead(io::Error),
}

/* ToDo
 *
 * Return result for solve()
 * Detect if plot exists on startup and load
 * Delete entire plot (perhaps with script) for testing
 * Extend tests
 * Resize plot by removing the last x indices and adjusting struct params
*/

pub struct Plot {
    map: HashMap<usize, u64>,
    map_file: File,
    plot_file: File,
    size: usize,
    updates: usize,
    update_interval: usize,
}

impl Plot {
    /// Creates a new plot for persisting encoded pieces to disk
    pub async fn open_or_create(path: &PathBuf, size: usize) -> Result<Plot, PlotCreationError> {
        let plot_file = OpenOptions::new()
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

        let updates = 0;
        let update_interval = crate::PLOT_UPDATE_INTERVAL;

        Ok(Plot {
            map,
            map_file,
            plot_file,
            size,
            updates,
            update_interval,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Reads a piece from plot by index
    pub async fn read(&mut self, index: usize) -> io::Result<Piece> {
        let position = match self.map.get(&index) {
            Some(position) => *position,
            None => {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
        };
        self.plot_file.seek(SeekFrom::Start(position)).await?;
        let mut buffer = [0u8; PIECE_SIZE];
        self.plot_file.read_exact(&mut buffer).await?;
        Ok(buffer)
    }

    /// Writes a piece to the plot by index, will overwrite if piece exists (updates)
    pub async fn write(&mut self, encoding: &Piece, index: usize) -> io::Result<()> {
        let position = self.plot_file.seek(SeekFrom::Current(0)).await?;
        self.plot_file.write_all(&encoding[0..PIECE_SIZE]).await?;
        self.map.insert(index, position);
        self.handle_update().await
    }

    /// Removes a piece from the plot by index, by deleting its index from the map
    pub async fn remove(&mut self, index: usize) -> io::Result<()> {
        self.map.remove(&index);
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
        &mut self,
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
    pub async fn force_write_map(&mut self) -> io::Result<()> {
        // TODO: Writing everything every time is probably not the smartest idea
        self.map_file.seek(SeekFrom::Start(0)).await?;
        self.map_file
            .set_len(((INDEX_LENGTH + OFFSET_LENGTH) * self.map.len()) as u64)
            .await?;
        for (index, offset) in self.map.iter() {
            self.map_file.write_all(&index.to_le_bytes()).await?;
            self.map_file.write_all(&offset.to_le_bytes()).await?;
        }

        Ok(())
    }

    /// Increment a counter to persist the map based on some interval
    async fn handle_update(&mut self) -> io::Result<()> {
        self.updates = self.updates.wrapping_add(1);

        if self.updates % self.update_interval == 0 {
            self.force_write_map().await?;
        }

        Ok(())
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

        let mut plot = Plot::new(&path, 10).await.unwrap();
        plot.write(&piece, 0).await.unwrap();
        let extracted_piece = plot.read(0).await.unwrap();

        assert_eq!(extracted_piece[..], piece[..]);

        plot.force_write_map().await.unwrap();
    }
}
