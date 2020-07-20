#![allow(dead_code)]

use std::io::Write;

pub fn xor_bytes(a: &mut [u8], b: &[u8]) {
    for (i, a_byte) in a.iter_mut().enumerate() {
        *a_byte ^= b[i];
    }
}

pub fn usize_to_bytes(number: usize) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv.as_mut()
        .write_all(&(number as u32).to_be_bytes())
        .unwrap();
    iv
}
