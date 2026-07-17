//! The Westwood public-key step that unlocks an encrypted MIX header.
//!
//! An encrypted RA MIX header begins with an 80-byte block: the 56-byte
//! Blowfish key encrypted with Westwood's RSA-style public key. That key is
//! shipped in the original game (`redalert/const.cpp`, the `[PublicKey]` INI
//! block); the exponent is the fixed "fast key" value 65537 and the modulus is
//! the 320-bit value below. The scheme processes the 80 bytes as two 40-byte
//! ciphertext blocks, each decrypting via `m = c^e mod n` to 39 plaintext bytes
//! (little-endian), yielding 78 bytes of which the first 56 are the Blowfish
//! key. See `common/pk.cpp` (`PKey::Decrypt`) and `common/pkstraw.cpp`.

use super::bignum::Big;

/// Public exponent (Westwood's fixed "fast key", `PKey::Fast_Exponent`).
const EXPONENT: u32 = 65537;

/// The public modulus `n`, big-endian.
///
/// Provenance: base64 `AihRvNoIbTn85FZRYNZRcT+i6KpU+maCsEqr3Q5q+LDB5tH7Tz2qQ38V`
/// from `redalert/const.cpp`, DER-decoded (ASN.1 INTEGER, tag 0x02, length 0x28)
/// to these 40 bytes. 319-bit value ⇒ 39-byte plaintext / 40-byte cipher blocks.
const MODULUS_BE: [u8; 40] = [
    0x51, 0xbc, 0xda, 0x08, 0x6d, 0x39, 0xfc, 0xe4, 0x56, 0x51, 0x60, 0xd6, 0x51, 0x71, 0x3f, 0xa2,
    0xe8, 0xaa, 0x54, 0xfa, 0x66, 0x82, 0xb0, 0x4a, 0xab, 0xdd, 0x0e, 0x6a, 0xf8, 0xb0, 0xc1, 0xe6,
    0xd1, 0xfb, 0x4f, 0x3d, 0xaa, 0x43, 0x7f, 0x15,
];

/// Bytes of ciphertext consumed per RSA block (`Crypt_Block_Size`).
const CRYPT_BLOCK: usize = 40;
/// Bytes of plaintext produced per RSA block (`Plain_Block_Size`).
const PLAIN_BLOCK: usize = 39;

/// Length of the encrypted key block that precedes an encrypted MIX header.
pub const ENCRYPTED_KEY_LEN: usize = 80; // 2 blocks × CRYPT_BLOCK
/// Length of the Blowfish key recovered from that block.
pub const BLOWFISH_KEY_LEN: usize = 56;

/// Decrypt the 80-byte encrypted key block into the 56-byte Blowfish key.
///
/// Returns `None` if fewer than [`ENCRYPTED_KEY_LEN`] bytes are supplied.
pub fn decrypt_blowfish_key(key_block: &[u8]) -> Option<[u8; BLOWFISH_KEY_LEN]> {
    if key_block.len() < ENCRYPTED_KEY_LEN {
        return None;
    }
    let modulus = Big::from_be_bytes(&MODULUS_BE);

    let mut plain = Vec::with_capacity(2 * PLAIN_BLOCK);
    for block in 0..(ENCRYPTED_KEY_LEN / CRYPT_BLOCK) {
        let start = block * CRYPT_BLOCK;
        // Ciphertext block is interpreted as a little-endian integer.
        let c = Big::from_le_bytes(&key_block[start..start + CRYPT_BLOCK]);
        let m = c.pow_mod(EXPONENT, &modulus);
        plain.extend_from_slice(&m.to_le_bytes(PLAIN_BLOCK));
    }

    let mut key = [0u8; BLOWFISH_KEY_LEN];
    key.copy_from_slice(&plain[..BLOWFISH_KEY_LEN]);
    Some(key)
}
