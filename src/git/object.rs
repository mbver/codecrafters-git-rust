use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use sha1::{Digest, Sha1};

pub fn write_object_at(git_dir: &Path, object_type: &str, content: &[u8]) -> io::Result<String> {
    // Construct the Git object:
    // <type> <size>\0<content>
    let mut object = format!("{object_type} {}\0", content.len(),).into_bytes();

    object.extend_from_slice(content);

    // Calculate SHA-1.
    let hash = hex::encode(Sha1::digest(&object));

    // Compress.
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());

    encoder.write_all(&object)?;

    let compressed = encoder.finish()?;

    // Store in .git/objects/<first two chars>/<remaining chars>.
    let (directory, filename) = hash.split_at(2);

    let object_dir = git_dir.join("objects").join(directory);

    fs::create_dir_all(&object_dir)?;

    fs::write(object_dir.join(filename), compressed)?;

    Ok(hash)
}

pub fn read_object(git_dir: &Path, sha: &str) -> io::Result<(String, Vec<u8>)> {
    if sha.len() != 40 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid SHA-1 length",
        ));
    }

    let (directory, filename) = sha.split_at(2);

    let path = git_dir.join("objects").join(directory).join(filename);

    let compressed = fs::read(path)?;

    let mut decoder = ZlibDecoder::new(&compressed[..]);

    let mut object = Vec::new();
    decoder.read_to_end(&mut object)?;

    // Find the null byte separating header and content.
    let header_end = object
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid Git object header"))?;

    let header = std::str::from_utf8(&object[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid Git object header"))?;

    let (object_type, size) = header
        .split_once(' ')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid Git object header"))?;

    let size: usize = size
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid Git object size"))?;

    let content = object[header_end + 1..].to_vec();

    if content.len() != size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git object size mismatch",
        ));
    }

    Ok((object_type.to_string(), content))
}
