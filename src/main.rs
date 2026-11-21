use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use sha1::Digest;
use sha1::Sha1;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

fn main() {
    let args: Vec<String> = env::args().collect();

    match args[1].as_str() {
        "init" => {
            fs::create_dir(".git").unwrap();
            fs::create_dir(".git/objects").unwrap();
            fs::create_dir(".git/refs").unwrap();
            fs::write(".git/HEAD", "ref: refs/heads/main\n").unwrap();
            println!("Initialized git directory");
        }
        "cat-file" if args[2] == "-p" => {
            cat_file(&args[3]).unwrap();
        }
        "hash-object" if args[2] == "-w" => {
            hash_object(&args[3]).unwrap();
        }
        "ls-tree" if args[2] == "--name-only" => {
            ls_tree(&args[3]).unwrap();
        }
        "write-tree" => {
            write_tree().unwrap();
        }
        "commit-tree" => {
            let tree_sha = &args[2];
            let parent_sha = &args[4];
            let message = &args[6];

            commit_tree(tree_sha, parent_sha, message).unwrap();
        }
        _ => {
            println!("unknown command: {}", args[1]);
        }
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

fn hash_object(file_path: &str) -> io::Result<()> {
    // Read file contents.
    let content = fs::read(file_path)?;
    let hash = write_object("blob", &content)?;

    println!("{hash}");

    Ok(())
}

fn write_object(object_type: &str, content: &[u8]) -> io::Result<String> {
    // Build Git object: <type> <size>\0<content>
    let mut object = format!("{object_type} {}\0", content.len()).into_bytes();
    object.extend_from_slice(content);

    // Calculate SHA-1.
    let hash = Sha1::digest(&object);
    let hash_hex = hex::encode(hash);

    // Compress using Zlib.
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&object)?;
    let compressed = encoder.finish()?;

    // Store in .git/objects.
    let (directory, filename) = hash_hex.split_at(2);
    let object_dir = format!(".git/objects/{directory}");

    fs::create_dir_all(&object_dir)?;
    fs::write(format!("{object_dir}/{filename}"), compressed)?;

    Ok(hash_hex)
}

struct TreeEntry {
    mode: String,
    name: String,
    sha: [u8; 20],
}
fn ls_tree(tree_sha: &str) -> io::Result<()> {
    let object_path = format!(".git/objects/{}/{}", &tree_sha[..2], &tree_sha[2..]);

    let compressed = fs::read(object_path)?;
    let mut decoder = ZlibDecoder::new(compressed.as_slice());

    let mut object = Vec::new();
    decoder.read_to_end(&mut object)?;

    let header_end = object
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid tree header"))?;

    if !object[..header_end].starts_with(b"tree ") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Not a tree object",
        ));
    }

    let mut entries = Vec::new();
    let mut cursor = header_end + 1;

    while cursor < object.len() {
        // Read mode.
        let mode_end = object[cursor..]
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid tree mode"))?
            + cursor;

        let mode = String::from_utf8(object[cursor..mode_end].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        cursor = mode_end + 1;

        // Read name.
        let name_end = object[cursor..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid tree name"))?
            + cursor;

        let name = String::from_utf8(object[cursor..name_end].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        cursor = name_end + 1;

        // Read 20 raw SHA-1 bytes.
        let sha: [u8; 20] = object
            .get(cursor..cursor + 20)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid tree SHA"))?
            .try_into() // convert slice to array
            .unwrap();

        cursor += 20;

        entries.push(TreeEntry { mode, name, sha });
    }

    for entry in entries {
        println!("{}", entry.name);
    }

    Ok(())
}

fn serialize_tree_entry(entry: &TreeEntry) -> Vec<u8> {
    let mut data = Vec::new();

    data.extend_from_slice(entry.mode.as_bytes());
    data.push(b' ');
    data.extend_from_slice(entry.name.as_bytes());
    data.push(0);
    data.extend_from_slice(&entry.sha);

    data
}

fn write_tree_object(entries: &[TreeEntry]) -> io::Result<String> {
    let mut content = Vec::new();

    for entry in entries {
        content.extend_from_slice(&serialize_tree_entry(entry));
    }

    write_object("tree", &content)
}

fn write_tree_recursive(dir: &Path) -> io::Result<String> {
    let mut entries = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        if name == ".git" {
            continue;
        }

        let metadata = fs::metadata(&path)?;

        let (mode, sha) = if metadata.is_dir() {
            let sha_hex = write_tree_recursive(&path)?;
            let sha = hex::decode(sha_hex).unwrap().try_into().unwrap();

            ("40000", sha)
        } else {
            let content = fs::read(&path)?;
            let sha_hex = write_object("blob", &content)?;
            let sha = hex::decode(sha_hex).unwrap().try_into().unwrap();

            ("100644", sha)
        };

        entries.push(TreeEntry {
            mode: mode.to_string(),
            name,
            sha,
        });
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));

    write_tree_object(&entries)
}

fn write_tree() -> io::Result<()> {
    let sha = write_tree_recursive(Path::new("."))?;
    println!("{sha}");

    Ok(())
}

struct Commit {
    tree_sha: String,
    parent_sha: String,
    author: String,
    committer: String,
    message: String,
}
impl Commit {
    fn serialize(&self) -> Vec<u8> {
        format!(
            "tree {}\nparent {}\nauthor {}\ncommitter {}\n\n{}\n",
            self.tree_sha, self.parent_sha, self.author, self.committer, self.message
        )
        .into_bytes()
    }
}

fn commit_tree(tree_sha: &str, parent_sha: &str, message: &str) -> io::Result<()> {
    let commit = Commit {
        tree_sha: tree_sha.to_string(),
        parent_sha: parent_sha.to_string(),
        author: "John Doe <john@example.com> 1234567890 +0000".to_string(),
        committer: "John Doe <john@example.com> 1234567890 +0000".to_string(),
        message: message.to_string(),
    };

    let content = commit.serialize();
    let hash = write_object("commit", &content)?;

    println!("{hash}");

    Ok(())
}
