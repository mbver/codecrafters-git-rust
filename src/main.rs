use std::env;
use std::fs;
use std::io::{self, Read, Write};

use flate2::read::ZlibDecoder;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args[1] == "init" {
        fs::create_dir(".git").unwrap();
        fs::create_dir(".git/objects").unwrap();
        fs::create_dir(".git/refs").unwrap();
        fs::write(".git/HEAD", "ref: refs/heads/main\n").unwrap();
        println!("Initialized git directory");
    } else if args[1] == "cat-file" && args[2] == "-p" {
        cat_file(&args[3]).unwrap();
    } else {
        println!("unknown command: {}", args[1]);
    }
}

fn cat_file(hash: &str) -> io::Result<()> {
    let object_path = format!(".git/objects/{}/{}", &hash[..2], &hash[2..]);

    // Read compressed object.
    let compressed = fs::read(object_path)?;

    // Decompress using Zlib.
    let mut decoder = ZlibDecoder::new(&compressed[..]);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;

    // Extract header and content.
    let header_end = decompressed
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing null byte"))?;

    let header = &decompressed[..header_end];
    let content = &decompressed[header_end + 1..];

    // Validate blob header.
    let header =
        std::str::from_utf8(header).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let (object_type, size) = header
        .split_once(' ')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid header"))?;

    if object_type != "blob" || content.len() != size.parse::<usize>().unwrap_or(usize::MAX) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid blob"));
    }

    // Write blob content directly to stdout.
    io::stdout().write_all(content)?;

    Ok(())
}
