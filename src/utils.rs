#![allow(dead_code)]

pub fn xor_bytes(a: &mut [u8], b: &[u8]) {
    for (i, a_byte) in a.iter_mut().enumerate() {
        *a_byte ^= b[i];
    }
}
