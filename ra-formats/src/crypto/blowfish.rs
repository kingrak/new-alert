//! Blowfish (Bruce Schneier's public-domain cipher) as used by Westwood to
//! encrypt MIX headers. This is standard big-endian Blowfish with a 16-round
//! Feistel network; the P-array and S-boxes are the pi-derived constants in
//! [`super::tables`]. Ported from `common/blowfish.cpp`.
//!
//! MIX headers are encrypted in ECB mode over 8-byte blocks. Only decryption is
//! needed to read archives, but key scheduling builds both permutation tables
//! exactly as the reference does so behaviour is bit-identical.

use super::tables::{P_INIT, S_INIT};

const ROUNDS: usize = 16;

pub struct Blowfish {
    p_encrypt: [u32; ROUNDS + 2],
    p_decrypt: [u32; ROUNDS + 2],
    s: [[u32; 256]; 4],
}

impl Blowfish {
    /// Build the cipher state from a key (max 56 bytes, per Westwood).
    pub fn new(key: &[u8]) -> Blowfish {
        let mut bf = Blowfish {
            p_encrypt: P_INIT,
            p_decrypt: P_INIT,
            s: S_INIT,
        };
        bf.schedule_key(key);
        bf
    }

    fn schedule_key(&mut self, key: &[u8]) {
        if key.is_empty() {
            return;
        }
        // Fold the key (wrapping as needed) into the encryption P-array.
        let mut j = 0usize;
        for p in self.p_encrypt.iter_mut() {
            let mut data: u32 = 0;
            for _ in 0..4 {
                data = (data << 8) | key[j % key.len()] as u32;
                j += 1;
            }
            *p ^= data;
        }

        // Scramble the P-arrays with the (evolving) cipher itself.
        let mut left = 0u32;
        let mut right = 0u32;
        let mut de = ROUNDS + 1;
        let mut en = 0usize;
        while en < ROUNDS + 2 {
            let (l, r) = self.sub_key_encrypt(left, right);
            left = l;
            right = r;
            self.p_encrypt[en] = left;
            self.p_encrypt[en + 1] = right;
            self.p_decrypt[de] = left;
            self.p_decrypt[de - 1] = right;
            en += 2;
            de = de.wrapping_sub(2);
        }

        // Scramble the S-boxes, carrying the (left,right) state onward.
        for sbox in 0..4 {
            let mut idx = 0;
            while idx < 256 {
                let (l, r) = self.sub_key_encrypt(left, right);
                left = l;
                right = r;
                self.s[sbox][idx] = left;
                self.s[sbox][idx + 1] = right;
                idx += 2;
            }
        }
    }

    #[inline]
    fn f(&self, x: u32) -> u32 {
        let a = (x >> 24) as usize & 0xff;
        let b = (x >> 16) as usize & 0xff;
        let c = (x >> 8) as usize & 0xff;
        let d = x as usize & 0xff;
        ((self.s[0][a].wrapping_add(self.s[1][b])) ^ self.s[2][c]).wrapping_add(self.s[3][d])
    }

    /// The key-scheduling encryption step (`Sub_Key_Encrypt` in the reference),
    /// which always uses the encryption P-array.
    fn sub_key_encrypt(&self, left: u32, right: u32) -> (u32, u32) {
        let mut l = left;
        let mut r = right;
        let mut i = 0;
        while i < ROUNDS {
            l ^= self.p_encrypt[i];
            r ^= self.f(l);
            r ^= self.p_encrypt[i + 1];
            l ^= self.f(r);
            i += 2;
        }
        // Note the swap: outputs are (r ^ P17, l ^ P16).
        (r ^ self.p_encrypt[ROUNDS + 1], l ^ self.p_encrypt[ROUNDS])
    }

    fn process_block(&self, block: [u8; 8], p: &[u32; ROUNDS + 2]) -> [u8; 8] {
        // Load big-endian halves.
        let mut left = u32::from_be_bytes([block[0], block[1], block[2], block[3]]);
        let mut right = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);

        let mut i = 0;
        while i < ROUNDS {
            left ^= p[i];
            right ^= self.f(left);
            right ^= p[i + 1];
            left ^= self.f(right);
            i += 2;
        }
        left ^= p[ROUNDS];
        right ^= p[ROUNDS + 1];

        // Output right then left (undoing the final Feistel swap), big-endian.
        let mut out = [0u8; 8];
        out[0..4].copy_from_slice(&right.to_be_bytes());
        out[4..8].copy_from_slice(&left.to_be_bytes());
        out
    }

    /// Decrypt as many whole 8-byte ECB blocks as fit in `data`; any trailing
    /// partial block is copied through unchanged (matching the reference).
    pub fn decrypt(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut chunks = data.chunks_exact(8);
        for chunk in &mut chunks {
            let mut block = [0u8; 8];
            block.copy_from_slice(chunk);
            out.extend_from_slice(&self.process_block(block, &self.p_decrypt));
        }
        out.extend_from_slice(chunks.remainder());
        out
    }

    /// Encrypt whole 8-byte ECB blocks (present for symmetry / testing).
    #[cfg(test)]
    pub fn encrypt(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut chunks = data.chunks_exact(8);
        for chunk in &mut chunks {
            let mut block = [0u8; 8];
            block.copy_from_slice(chunk);
            out.extend_from_slice(&self.process_block(block, &self.p_encrypt));
        }
        out.extend_from_slice(chunks.remainder());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let bf = Blowfish::new(b"a Westwood MIX key");
        let plain = *b"12345678ABCDEFGH";
        let ct = bf.encrypt(&plain);
        let pt = bf.decrypt(&ct);
        assert_eq!(pt, plain);
    }

    #[test]
    fn known_answer() {
        // Standard Blowfish ECB test vector (Eric Young / Schneier set):
        // key = 0000000000000000, plaintext = 0000000000000000,
        // ciphertext = 4EF99745 6198DD78.
        let bf = Blowfish::new(&[0u8; 8]);
        let ct = bf.encrypt(&[0u8; 8]);
        assert_eq!(ct, vec![0x4E, 0xF9, 0x97, 0x45, 0x61, 0x98, 0xDD, 0x78]);
    }
}
