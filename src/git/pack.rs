use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

use flate2::bufread::ZlibDecoder;
use sha1::{Digest, Sha1};

use crate::git::object::write_object_at;

#[derive(Clone, Copy, Debug)]
pub enum ObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl ObjectType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
            Self::Tag => "tag",
        }
    }
}

pub enum PackEntry {
    Full {
        offset: usize,
        object_type: ObjectType,
        content: Vec<u8>,
    },
    OfsDelta {
        offset: usize,
        base_offset: usize,
        delta: Vec<u8>,
    },
    RefDelta {
        offset: usize,
        base_sha: String,
        delta: Vec<u8>,
    },
}

#[derive(Clone)]
struct ResolvedObject {
    object_type: ObjectType,
    content: Vec<u8>,
}

// Read the packfile header and parse all entries.
pub fn parse_pack_entries(data: &[u8]) -> io::Result<Vec<PackEntry>> {
    if data.len() < 32 || &data[..4] != b"PACK" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid packfile header",
        ));
    }

    // try_into convert &[u8] to [u8;4]
    let version = u32::from_be_bytes(data[4..8].try_into().unwrap());

    let count = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;

    if version != 2 && version != 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsupported packfile version",
        ));
    }

    // Last 20 bytes contain the packfile checksum.
    let checksum_start = data.len() - 20;

    let expected_checksum = &data[checksum_start..];
    let actual_checksum = Sha1::digest(&data[..checksum_start]);

    if actual_checksum[..] != expected_checksum[..] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Packfile checksum mismatch",
        ));
    }

    let mut cursor = 12;
    let mut entries = Vec::with_capacity(count);

    for _ in 0..count {
        let object_offset = cursor;

        // Read object type and uncompressed size.
        let (type_code, expected_size) = read_object_header(&data[..checksum_start], &mut cursor)?;

        let entry_kind = match type_code {
            1 => EntryKind::Full(ObjectType::Commit),
            2 => EntryKind::Full(ObjectType::Tree),
            3 => EntryKind::Full(ObjectType::Blob),
            4 => EntryKind::Full(ObjectType::Tag),

            6 => {
                let base_offset =
                    read_ofs_delta_offset(&data[..checksum_start], &mut cursor, object_offset)?;

                EntryKind::Ofs(base_offset)
            }

            7 => {
                // ref-delta refer base-object with it sha
                // the base object can be anywhere in the pack
                let base_sha = data.get(cursor..cursor + 20).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "Missing REF_DELTA base SHA")
                })?;

                cursor += 20;

                EntryKind::Ref(hex::encode(base_sha))
            }

            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unknown pack object type",
                ));
            }
        };

        // Decompress the zlib stream.
        let (content, compressed_size) = {
            let mut decoder = ZlibDecoder::new(&data[cursor..checksum_start]);

            let mut content = Vec::new();
            decoder.read_to_end(&mut content)?;

            (content, decoder.total_in() as usize)
        };

        cursor += compressed_size;

        if content.len() != expected_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Pack object size mismatch",
            ));
        }

        let entry = match entry_kind {
            EntryKind::Full(object_type) => PackEntry::Full {
                offset: object_offset,
                object_type,
                content,
            },

            EntryKind::Ofs(base_offset) => PackEntry::OfsDelta {
                offset: object_offset,
                base_offset,
                delta: content,
            },

            EntryKind::Ref(base_sha) => PackEntry::RefDelta {
                offset: object_offset,
                base_sha,
                delta: content,
            },
        };

        entries.push(entry);
    }

    if cursor != checksum_start {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unexpected trailing pack data",
        ));
    }

    Ok(entries)
}

enum EntryKind {
    Full(ObjectType),
    Ofs(usize),
    Ref(String),
}

// read object type and the uncompressed length
/*
In a Git .pack file, each object starts with a variable-length header.
    Byte 1 (the first byte) holds both the type and the lowest 4 bits of the size:
        Bit 7 (0x80): Continuation flag (1 = more size bytes follow; 0 = this is the last header byte).
        Bits 6..4 ((byte >> 4) & 0x07): 3-bit object type (1 = commit, 2 = tree,
            3 = blob, 4 = tag, 6 = OFS_DELTA, 7 = REF_DELTA).
        Bits 3..0 (byte & 0x0f): Least significant 4 bits of the uncompressed size.
    Bytes 2+ (optional continuation bytes) each hold 7 more bits of the size in little-endian bit order (least significant bits come first):
        Bit 7 (0x80): Continuation flag (1 = another byte follows; 0 = done).
        Bits 6..0 (byte & 0x7f): Next 7 bits of the size.
*/
fn read_object_header(data: &[u8], cursor: &mut usize) -> io::Result<(u8, usize)> {
    let mut byte = *data
        .get(*cursor)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Missing object header"))?;

    *cursor += 1;

    let object_type = (byte >> 4) & 0x07;

    let mut size = (byte & 0x0f) as usize;
    let mut shift = 4;

    while byte & 0x80 != 0 {
        byte = *data.get(*cursor).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "Incomplete object size")
        })?;

        *cursor += 1;

        let part = ((byte & 0x7f) as usize)
            .checked_shl(shift)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Object size overflow"))?;

        size |= part;

        shift += 7;

        if shift >= usize::BITS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Object size overflow",
            ));
        }
    }

    Ok((object_type, size))
}

/*
*Off-delta object tell how many bytes back in the file the base object is.
    -Suppose this object starts at byte 1000 in the file (object_offset = 1000),
        and its base object starts at byte 872.
            - base is 128 bytes back (1000 - 872 = 128).
            - The number 128 is what gets encoded into those bytes.
            - When your code reads 128, it does 1000 - 128 = 872 to jump back to the base object.

Why use "how many bytes back" instead of "byte 872"?
    - Smaller numbers = fewer bytes: Nearby objects have small distances (like 50 bytes back), which fit in 1 byte.

The left-most bit is the continuation flag.

Git's decode: the first group gets +1 before the shift.
    (aaaaaaa + 1) bbbbbbb
    So the high part can be 8 bits wide, and the largest 2-byte result is:
        10000000 1111111       15 bits
    In case the 1st group is 1000 0000, the value is
        00000001 1111111
*/
// compute offset of the base object
fn read_ofs_delta_offset(
    data: &[u8],
    cursor: &mut usize,
    object_offset: usize,
) -> io::Result<usize> {
    let mut byte = *data
        .get(*cursor)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Missing delta offset"))?;

    *cursor += 1;

    let mut distance = (byte & 0x7f) as usize;

    while byte & 0x80 != 0 {
        byte = *data.get(*cursor).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "Incomplete delta offset")
        })?;

        *cursor += 1;

        distance = distance
            .checked_add(1)
            .and_then(|n| n.checked_shl(7))
            .and_then(|n| n.checked_add((byte & 0x7f) as usize))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Delta offset overflow"))?;
    }

    object_offset
        .checked_sub(distance)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid delta base offset"))
}

// Read a variable-length integer from delta data.
fn read_delta_size(data: &[u8], cursor: &mut usize) -> io::Result<usize> {
    let mut result = 0usize;
    let mut shift = 0;

    loop {
        let byte = *data
            .get(*cursor)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Incomplete delta size"))?;

        *cursor += 1;

        let part = ((byte & 0x7f) as usize)
            .checked_shl(shift)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Delta size overflow"))?;

        result |= part;

        if byte & 0x80 == 0 {
            return Ok(result);
        }

        shift += 7;

        if shift >= usize::BITS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Delta size overflow",
            ));
        }
    }
}

/*
* How Overwriting Works with Only Copy & Insert
    - copy the part of base before the changed region.
    - insert the new replacement bytes (instead of copying the old bytes from base).
    - copy the part of base after the changed region.
*/
/*
Copy: Take a piece of the old base file and paste it onto the end of the new file
Bit position:   7     6     5     4     3     2     1     0
              +-----+-----+-----+-----+-----+-----+-----+-----+
Meaning:      |  1  |  S2 |  S1 |  S0 |  O3 |  O2 |  O1 |  O0 |
              +-----+-----+-----+-----+-----+-----+-----+-----+
Role:         |Copy | <--- Size --->| <------ Offset ------>|
              |flag |   switches    |       switches        |

Example
    Suppose command is 0b1001_0011 (0x93):

        Plaintext
        Bit:      7   6   5   4   3   2   1   0
        Value:    1   0   0   1   0   0   1   1
                │   └───┴───┘   └───────┴───┘
                │       │            │
            Copy flag  S0 is 1     O0 and O1 are 1
                    (1 size byte) (2 offset bytes)
        Right after this 0x93 byte in delta, you will read 3 extra bytes in this exact order:

        Offset Byte 0 (because O0 is 1)

        Offset Byte 1 (because O1 is 1)

        Size Byte 0 (because S0 is 1)
*/
fn apply_delta(base: &[u8], delta: &[u8]) -> io::Result<Vec<u8>> {
    let mut cursor = 0;

    let base_size = read_delta_size(delta, &mut cursor)?;
    let result_size = read_delta_size(delta, &mut cursor)?;

    if base_size != base.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Delta base size mismatch",
        ));
    }

    let mut result = Vec::with_capacity(result_size);

    while cursor < delta.len() {
        let command = delta[cursor];
        cursor += 1;

        if command & 0x80 != 0 {
            // Copy instruction.
            let mut offset = 0usize;
            let mut size = 0usize;

            // Offset bytes are right after the header bytes
            for i in 0..4 {
                if command & (1 << i) != 0 {
                    let byte = *delta.get(cursor).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::UnexpectedEof, "Missing copy offset")
                    })?;

                    cursor += 1;

                    offset |= (byte as usize) << (i * 8);
                }
            }

            // Size bytes are right after offset bytes
            for i in 0..3 {
                if command & (1 << (4 + i)) != 0 {
                    let byte = *delta.get(cursor).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::UnexpectedEof, "Missing copy size")
                    })?;

                    cursor += 1;

                    size |= (byte as usize) << (i * 8);
                }
            }

            // A zero size means 65536 bytes.
            if size == 0 {
                size = 0x10000;
            }

            let end = offset
                .checked_add(size)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Copy range overflow"))?;

            let copied = base.get(offset..end).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "Copy range outside base object")
            })?;

            result.extend_from_slice(copied);
        } else if command != 0 {
            // Insert instruction.
            let length = command as usize;

            let end = cursor.checked_add(length).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "Insert range overflow")
            })?;

            let inserted = delta.get(cursor..end).ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "Incomplete insert data")
            })?;

            result.extend_from_slice(inserted);

            cursor = end;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid zero delta instruction",
            ));
        }

        if result.len() > result_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Delta result exceeds declared size",
            ));
        }
    }

    if result.len() != result_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Delta result size mismatch",
        ));
    }

    Ok(result)
}

/*
 * write full objects inside .git director and add them to look up tables to later resolve deltas
 * the deltas objects are pushed into remaining. at each pass, unresolved deltas are pushed into a new remaining queue
 * for delta objects that have a matching full base object, resolve it and set resolved_any to true
 * at the end of each pass, if no delta is resolved and remaining is not empty, no progress is possible, return error
 */
pub fn resolve_pack(entries: Vec<PackEntry>, git_dir: &Path) -> io::Result<()> {
    // OFS_DELTA needs lookup by packfile offset.
    let mut by_offset: HashMap<usize, ResolvedObject> = HashMap::new();

    // REF_DELTA needs lookup by object SHA.
    let mut by_sha: HashMap<String, ResolvedObject> = HashMap::new();

    let mut pending = Vec::new();

    // First store all ordinary objects.
    for entry in entries {
        match entry {
            PackEntry::Full {
                offset,
                object_type,
                content,
            } => {
                let sha = write_object_at(git_dir, object_type.as_str(), &content)?;

                let object = ResolvedObject {
                    object_type,
                    content,
                };

                by_offset.insert(offset, object.clone());
                by_sha.insert(sha, object);
            }

            other => {
                pending.push(other);
            }
        }
    }

    // Resolve delta objects whose bases are available.
    while !pending.is_empty() {
        let mut remaining = Vec::new();
        let mut resolved_any = false;

        for entry in pending {
            let (offset, base, delta) = match entry {
                PackEntry::OfsDelta {
                    offset,
                    base_offset,
                    delta,
                } => {
                    let Some(base) = by_offset.get(&base_offset) else {
                        remaining.push(PackEntry::OfsDelta {
                            offset,
                            base_offset,
                            delta,
                        });

                        continue;
                    };

                    (offset, base.clone(), delta)
                }

                PackEntry::RefDelta {
                    offset,
                    base_sha,
                    delta,
                } => {
                    let Some(base) = by_sha.get(&base_sha) else {
                        remaining.push(PackEntry::RefDelta {
                            offset,
                            base_sha,
                            delta,
                        });

                        continue;
                    };

                    (offset, base.clone(), delta)
                }

                PackEntry::Full { .. } => unreachable!(),
            };

            // Apply delta using the base object's content.
            let content = apply_delta(&base.content, &delta)?;

            // Delta object has the same Git object type as its base.
            let sha = write_object_at(git_dir, base.object_type.as_str(), &content)?;

            let object = ResolvedObject {
                object_type: base.object_type,
                content,
            };

            by_offset.insert(offset, object.clone());
            by_sha.insert(sha, object);

            resolved_any = true;
        }

        if !resolved_any && !remaining.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unable to resolve packfile deltas",
            ));
        }

        pending = remaining;
    }

    Ok(())
}
