use bitintr::Lzcnt;
use num_bigint::{BigUint, ToBigUint};
use num_traits::cast::ToPrimitive;
use std::io::Write;

/// XOR two bytes slices
pub fn xor_bytes(a: &mut [u8], b: &[u8]) {
    for (i, a_byte) in a.iter_mut().enumerate() {
        *a_byte ^= b[i];
    }
}

/// Convert a usize integer to a byte array
pub fn usize_to_bytes(number: usize) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv.as_mut()
        .write_all(&(number as u32).to_be_bytes())
        .unwrap();
    iv
}

/// Compute the remainder of an arbitry sized byte slice for a given modulus
pub fn modulo(a: &[u8], n: usize) -> usize {
    let big_int_a = bytes_to_bigint(&a);
    let big_int_n = usize_to_bigint(n);
    let big_int_modulus = big_int_a % big_int_n;
    bigint_to_usize(big_int_modulus)
}

/// Convert arbitrary byte slice to a big unsigned integer
pub fn bytes_to_bigint(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_be(bytes)
}

/// Convert four bytes into u32 representation
pub fn bytes_le_to_u32(array: &[u8]) -> u32 {
    (array[0] as u32)
        + ((array[1] as u32) << 8)
        + ((array[2] as u32) << 16)
        + ((array[3] as u32) << 24)
}

pub fn u16_to_bytes_le(number: u16) -> [u8; 2] {
    let mut bytes = [0u8; 2];
    bytes.as_mut().write_all(&number.to_le_bytes()).unwrap();
    bytes
}

pub fn u64_to_bytes_le(number: u16) -> [u8; 2] {
    let mut bytes = [0u8; 2];
    bytes.as_mut().write_all(&number.to_le_bytes()).unwrap();
    bytes
}

pub fn bytes_to_le_16(array: &[u8]) -> u16 {
    (array[0] as u16) + ((array[1] as u16) << 8)
}

/// Convert usize to a big unsigned integer
pub fn usize_to_bigint(number: usize) -> BigUint {
    ToBigUint::to_biguint(&number).unwrap()
}

/// Convert big unsigned integer to usize, assumes it will fit
pub fn bigint_to_usize(bigint: BigUint) -> usize {
    bigint.to_usize().unwrap()
}

// count the number of leading zeros in a slice of bytes
pub fn measure_quality(tag: &[u8]) -> u8 {
    let mut quality: u8 = 0;
    for byte in tag.iter() {
        let zero_bits = byte.lzcnt();
        quality += zero_bits;
        if zero_bits < 8 {
            break;
        }
    }
    quality
}
