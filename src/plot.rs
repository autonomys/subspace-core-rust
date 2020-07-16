#![allow(dead_code)]

use crate::{Piece, PIECE_SIZE};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::prelude::*;
use std::io::SeekFrom;

/*
    *   Add tests
    *   Init plot as singe file of plot size
    *   Retain plot between runs
    *   Store plot index to disk
    *
*/

pub struct Plot {
    path: String,
    size: usize,
    file: File,
    map: HashMap<usize, u64>,
}

impl Plot {
    pub fn new(path: String, size: usize) -> Plot {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .expect("Unable to open");

        // file.set_len(size as u64).unwrap();

        let map: HashMap<usize, u64> = HashMap::new();

        Plot {
            path: String::from(&path),
            size,
            file,
            map,
        }
    }

    pub fn add(&mut self, encoding: &Piece, index: usize) {
        let position = self.file.seek(SeekFrom::Current(0)).unwrap();
        self.file.write_all(&encoding[0..PIECE_SIZE]).unwrap();
        self.map.insert(index, position);
    }

    pub fn get(&mut self, index: usize) -> Piece {
        let position = self.map.get(&index).unwrap();
        self.file.seek(SeekFrom::Start(*position)).unwrap();
        let mut buffer = [0u8; PIECE_SIZE];
        self.file.read_exact(&mut buffer).unwrap();
        buffer
    }
}