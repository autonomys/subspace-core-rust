#![allow(dead_code)]

use crate::{Piece, PIECE_SIZE};
use async_std::fs::File;
use async_std::fs::OpenOptions;
use async_std::io::prelude::*;
use async_std::path::Path;
use std::collections::HashMap;
use std::io;
use std::io::SeekFrom;

#[derive(Debug)]
pub enum PlotCreationError {
    DirectoryCreation(io::Error),
    PlotOpen(io::Error),
    PlotMapOpen(io::Error),
}

/*
 *   Add tests
 *   Init plot as singe file of plot size
 *   Retain plot between runs
 *   Store plot index to disk
 *
*/

pub struct Plot {
    map: HashMap<usize, u64>,
    map_file: File,
    plot_file: File,
    size: usize,
    updates: usize,
}

impl Plot {
    pub async fn new(path: &Path, size: usize) -> Result<Plot, PlotCreationError> {
        if !path.exists().await {
            async_std::fs::create_dir_all(path)
                .await
                .map_err(|error| PlotCreationError::DirectoryCreation(error))?;
        }

        let plot_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.join("plot.bin"))
            .await
            .map_err(|error| PlotCreationError::PlotOpen(error))?;

        let map_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.join("plot-map.bin"))
            .await
            .map_err(|error| PlotCreationError::PlotMapOpen(error))?;

        let map = HashMap::new();
        let updates = 0;

        Ok(Plot {
            map,
            map_file,
            plot_file,
            size,
            updates,
        })
    }

    pub async fn read(&mut self, index: usize) -> io::Result<Piece> {
        let position = self.map.get(&index).unwrap();
        self.plot_file.seek(SeekFrom::Start(*position)).await?;
        let mut buffer = [0u8; PIECE_SIZE];
        self.plot_file.read_exact(&mut buffer).await?;
        Ok(buffer)
    }

    pub async fn write(&mut self, encoding: &Piece, index: usize) -> io::Result<()> {
        let position = self.plot_file.seek(SeekFrom::Current(0)).await?;
        self.plot_file.write_all(&encoding[0..PIECE_SIZE]).await?;
        self.map.insert(index, position);
        self.handle_update().await
    }

    pub async fn remove(&mut self, index: usize) -> io::Result<()> {
        self.map.remove(&index);
        self.handle_update().await
    }

    pub async fn force_write_map(&mut self) -> io::Result<()> {
        // TODO: Writing everything every time is probably not the smartest idea
        self.map_file.seek(SeekFrom::Start(0)).await?;
        for (index, offset) in self.map.iter() {
            self.map_file.write_all(&index.to_le_bytes()).await?;
            self.map_file.write_all(&offset.to_le_bytes()).await?;
        }

        Ok(())
    }

    async fn handle_update(&mut self) -> io::Result<()> {
        self.updates = self.updates.wrapping_add(1);

        if self.updates % 10000 == 0 {
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
