//! MIX archives — Westwood's flat container format.
//!
//! Two on-disk header variants exist and both are supported:
//!
//! - **Plain** (Tiberian Dawn / older): the file opens directly with the file
//!   header (`u16 count`, `u32 data_size`) followed by the entry index.
//! - **Flagged** (Red Alert): the file opens with a 32-bit flags word. If the
//!   low 16 bits are zero it is the extended format and the next 16 bits are a
//!   bitfield — bit 0 = SHA digest attached, bit 1 = encrypted header. An
//!   encrypted header is preceded by an 80-byte public-key-wrapped Blowfish key
//!   (see [`crate::crypto`]); the header itself is then Blowfish-ECB encrypted
//!   and padded up to an 8-byte boundary so the data section stays aligned.
//!
//! Each index entry is `(id: u32, offset: u32, size: u32)` where `id` is the
//! [`crate::crc`] hash of the upper-cased filename. Archives can be nested
//! (e.g. `redalert.mix` contains `local.mix`, `hires.mix`, …); an entry's bytes
//! are just a sub-slice and can be re-parsed as another [`MixArchive`].
//!
//! Ported from `common/mixfile.cpp`.

use crate::crypto::{self, Blowfish};
use crate::{crc, FormatError, Result};

const FLAG_DIGEST: u16 = 0x0001;
const FLAG_ENCRYPTED: u16 = 0x0002;

/// One entry in a MIX index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixEntry {
    /// Westwood filename hash (see [`crate::crc`]).
    pub id: u32,
    /// Offset of the entry's data from the start of the data section.
    pub offset: u32,
    /// Size of the entry's data in bytes.
    pub size: u32,
}

/// A parsed MIX archive borrowing the underlying file bytes.
pub struct MixArchive<'a> {
    data: &'a [u8],
    data_start: usize,
    entries: Vec<MixEntry>,
    /// Whether this archive's header was encrypted.
    pub encrypted: bool,
    /// Whether this archive declares an attached SHA digest.
    pub has_digest: bool,
}

fn read_u16(data: &[u8], at: usize, ctx: &'static str) -> Result<u16> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(FormatError::UnexpectedEof { context: ctx })
}

fn read_u32(data: &[u8], at: usize, ctx: &'static str) -> Result<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(FormatError::UnexpectedEof { context: ctx })
}

/// Parse `count` index entries (12 bytes each) out of an already-plaintext
/// header slice starting at `at`.
fn parse_index(header: &[u8], at: usize, count: usize) -> Result<Vec<MixEntry>> {
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let base = at + i * 12;
        let id = read_u32(header, base, "mix entry id")?;
        let offset = read_u32(header, base + 4, "mix entry offset")?;
        let size = read_u32(header, base + 8, "mix entry size")?;
        entries.push(MixEntry { id, offset, size });
    }
    Ok(entries)
}

/// Round `n` up to the next multiple of 8.
fn round_up_8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

impl<'a> MixArchive<'a> {
    /// Parse the archive header out of `data` (the whole MIX file bytes).
    pub fn parse(data: &'a [u8]) -> Result<MixArchive<'a>> {
        let first = read_u16(data, 0, "mix header")?;

        if first == 0 {
            // Flagged / extended format.
            let flags = read_u16(data, 2, "mix flags")?;
            let has_digest = flags & FLAG_DIGEST != 0;
            let encrypted = flags & FLAG_ENCRYPTED != 0;

            if encrypted {
                return Self::parse_encrypted(data, has_digest);
            }
            // Extended but not encrypted: header follows the 4-byte flags word.
            let count = read_u16(data, 4, "mix count")? as usize;
            let index_at = 10; // 4 flags + 2 count + 4 size
            let entries = parse_index(data, index_at, count)?;
            let data_start = index_at + count * 12;
            Ok(MixArchive {
                data,
                data_start,
                entries,
                encrypted: false,
                has_digest,
            })
        } else {
            // Plain format: `first` is really the entry count.
            let count = first as usize;
            let index_at = 6; // 2 count + 4 size
            let entries = parse_index(data, index_at, count)?;
            let data_start = index_at + count * 12;
            Ok(MixArchive {
                data,
                data_start,
                entries,
                encrypted: false,
                has_digest: false,
            })
        }
    }

    fn parse_encrypted(data: &'a [u8], has_digest: bool) -> Result<MixArchive<'a>> {
        // 4-byte flags word, then the 80-byte encrypted Blowfish key block.
        let key_block =
            data.get(4..4 + crypto::ENCRYPTED_KEY_LEN)
                .ok_or(FormatError::UnexpectedEof {
                    context: "mix encrypted key block",
                })?;
        let bf_key = crypto::decrypt_blowfish_key(key_block).ok_or(FormatError::Invalid {
            reason: "could not recover Blowfish key",
        })?;
        let bf = Blowfish::new(&bf_key);

        let enc_start = 4 + crypto::ENCRYPTED_KEY_LEN;
        let enc = &data[enc_start..];

        // Decrypt the first 8-byte block to learn the entry count.
        let first_block = enc.get(0..8).ok_or(FormatError::UnexpectedEof {
            context: "mix encrypted header",
        })?;
        let head = bf.decrypt(first_block);
        let count = u16::from_le_bytes([head[0], head[1]]) as usize;

        // Full plaintext header = count(2) + size(4) + count*12 entry bytes,
        // stored padded up to a whole number of 8-byte Blowfish blocks.
        let plain_header_len = 6 + count * 12;
        let enc_header_len = round_up_8(plain_header_len);
        let enc_header = enc
            .get(0..enc_header_len)
            .ok_or(FormatError::UnexpectedEof {
                context: "mix encrypted header body",
            })?;
        let header = bf.decrypt(enc_header);

        let entries = parse_index(&header, 6, count)?;
        let data_start = enc_start + enc_header_len;

        Ok(MixArchive {
            data,
            data_start,
            entries,
            encrypted: true,
            has_digest,
        })
    }

    /// All index entries, in file order.
    pub fn entries(&self) -> &[MixEntry] {
        &self.entries
    }

    /// Offset of the data section within the underlying bytes.
    pub fn data_start(&self) -> usize {
        self.data_start
    }

    /// Look up an entry by its precomputed id.
    pub fn find_id(&self, id: u32) -> Option<&MixEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Look up an entry by filename (hashed via [`crate::crc`]).
    pub fn find(&self, name: &str) -> Option<&MixEntry> {
        self.find_id(crc::id_of(name))
    }

    /// Borrow the raw bytes of an entry, or `None` if they fall outside the
    /// file (a corrupt offset/size).
    pub fn entry_bytes(&self, entry: &MixEntry) -> Option<&'a [u8]> {
        let start = self.data_start.checked_add(entry.offset as usize)?;
        let end = start.checked_add(entry.size as usize)?;
        self.data.get(start..end)
    }

    /// Borrow the bytes of a named entry.
    pub fn get(&self, name: &str) -> Option<&'a [u8]> {
        let entry = *self.find(name)?;
        self.entry_bytes(&entry)
    }

    /// Open a nested MIX archive stored as an entry of this one.
    pub fn open_nested(&self, name: &str) -> Result<MixArchive<'a>> {
        let bytes = self.get(name).ok_or(FormatError::Invalid {
            reason: "nested mix entry not found",
        })?;
        MixArchive::parse(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_plain_mix() -> Vec<u8> {
        // Two tiny entries in a hand-built plain MIX.
        let payload_a = b"HELLO";
        let payload_b = b"WORLD!";
        let mut body = Vec::new();
        let off_a = 0u32;
        body.extend_from_slice(payload_a);
        let off_b = body.len() as u32;
        body.extend_from_slice(payload_b);

        let entries = [
            (crc::id_of("a.txt"), off_a, payload_a.len() as u32),
            (crc::id_of("b.txt"), off_b, payload_b.len() as u32),
        ];

        let mut out = Vec::new();
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        for (id, off, sz) in entries {
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&off.to_le_bytes());
            out.extend_from_slice(&sz.to_le_bytes());
        }
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn plain_roundtrip() {
        let mix_bytes = build_plain_mix();
        let mix = MixArchive::parse(&mix_bytes).unwrap();
        assert_eq!(mix.entries().len(), 2);
        assert_eq!(mix.get("a.txt"), Some(&b"HELLO"[..]));
        assert_eq!(mix.get("b.txt"), Some(&b"WORLD!"[..]));
        assert!(!mix.encrypted);
    }
}
