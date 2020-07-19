#![allow(dead_code)]

use crate::{Piece, PIECE_SIZE};
use async_std::fs::File;
use async_std::fs::OpenOptions;
use async_std::io::prelude::*;
use async_std::path::Path;
use std::collections::HashMap;
use std::io::SeekFrom;

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
    pub async fn new(path: &Path, size: usize) -> Plot {
        if !path.exists().await {
            async_std::fs::create_dir_all(path)
                .await
                .expect("Failed to create directory for plot");
        }

        let plot_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.join("plot.bin"))
            .await
            .expect("Unable to open plot file");

        let map_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path.join("plot-map.bin"))
            .await
            .expect("Unable to open plot  file");

        let map = HashMap::new();
        let updates = 0;

        Plot {
            map,
            map_file,
            plot_file,
            size,
            updates,
        }
    }

    pub async fn read(&mut self, index: usize) -> Piece {
        let position = self.map.get(&index).unwrap();
        self.plot_file
            .seek(SeekFrom::Start(*position))
            .await
            .unwrap();
        let mut buffer = [0u8; PIECE_SIZE];
        self.plot_file.read_exact(&mut buffer).await.unwrap();
        buffer
    }

    pub async fn write(&mut self, encoding: &Piece, index: usize) {
        let position = self.plot_file.seek(SeekFrom::Current(0)).await.unwrap();
        self.plot_file
            .write_all(&encoding[0..PIECE_SIZE])
            .await
            .unwrap();
        self.map.insert(index, position);
        self.handle_update().await;
    }

    pub async fn remove(&mut self, index: usize) {
        self.map.remove(&index);
        self.handle_update().await;
    }

    pub async fn force_write_map(&mut self) {
        self.handle_update().await;
    }

    async fn handle_update(&mut self) {
        self.updates = self.updates.wrapping_add(1);

        if self.updates % 10000 == 0 {
            // TODO: Writing everything every time is probably not the smartest idea
            self.map_file.seek(SeekFrom::Start(0)).await.unwrap();
            for (index, offset) in self.map.iter() {
                self.map_file.write_all(&index.to_le_bytes()).await.unwrap();
                self.map_file
                    .write_all(&offset.to_le_bytes())
                    .await
                    .unwrap();
            }
        }
    }
}
