use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::git::object::read_object;

// Read a commit and return its root tree SHA.
pub fn get_commit_tree(git_dir: &Path, commit_sha: &str) -> io::Result<String> {
    let (object_type, content) = read_object(git_dir, commit_sha)?;

    if object_type != "commit" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Expected commit object",
        ));
    }

    let content = std::str::from_utf8(&content)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid commit content"))?;

    for line in content.lines() {
        if let Some(tree_sha) = line.strip_prefix("tree ") {
            return Ok(tree_sha.to_string());
        }

        // Headers end at the first empty line.
        if line.is_empty() {
            break;
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Commit tree not found",
    ))
}

// Recursively reconstruct a tree in the working directory.
/*
* In Git, a tree object is like a snapshot of a directory: it stores a list of entries,
* where each entry has a file mode (permissions/type), a name, and a 20-byte SHA-1 hash
* pointing to either a blob (file contents) or another tree (subdirectory).
*
* Materializes each entry onto disk at directory.join(name):
    - "40000" | "040000" (directory): Recursively calls checkout_tree(git_dir, &sha, &path).
    - "100644" | "100755" (regular file): Reads the blob object for sha, writes its bytes to path,
        and sets 0o755 (executable) permissions if mode == "100755".
    - "120000" (symlink): Reads the target path from the blob and creates a real filesystem symlink
        via std::os::unix::fs::symlink.
    - "160000" (git submodule link): Creates an empty directory at path instead of cloning the submodule.
*/
pub fn checkout_tree(git_dir: &Path, tree_sha: &str, directory: &Path) -> io::Result<()> {
    let (object_type, content) = read_object(git_dir, tree_sha)?;

    if object_type != "tree" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Expected tree object",
        ));
    }

    fs::create_dir_all(directory)?;

    let mut cursor = 0;

    while cursor < content.len() {
        // Read mode.
        let mode_end = content[cursor..]
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid tree mode"))?;

        let mode = std::str::from_utf8(&content[cursor..cursor + mode_end])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid tree mode"))?;

        cursor += mode_end + 1;

        // Read filename.
        let name_end = content[cursor..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid tree filename"))?;

        let name = &content[cursor..cursor + name_end];

        cursor += name_end + 1;

        // Read the raw 20-byte SHA.
        let sha_bytes = content
            .get(cursor..cursor + 20)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Missing tree SHA"))?;

        let sha = hex::encode(sha_bytes);

        cursor += 20;

        // Preserve Unix filename bytes.
        let name = std::ffi::OsStr::from_bytes(name);

        let path = directory.join(name);

        match mode {
            "40000" | "040000" => {
                // Directory: recursively process child tree.
                checkout_tree(git_dir, &sha, &path)?;
            }

            "100644" | "100755" => {
                // Regular file: read blob and write contents.
                let (object_type, file_content) = read_object(git_dir, &sha)?;

                if object_type != "blob" {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Expected blob object",
                    ));
                }

                fs::write(&path, file_content)?;

                if mode == "100755" {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
                }
            }

            "120000" => {
                // Symbolic link: blob content is the link target.
                let (_, target) = read_object(git_dir, &sha)?;

                let target = std::ffi::OsStr::from_bytes(&target);

                std::os::unix::fs::symlink(target, &path)?;
            }

            "160000" => {
                // Submodule gitlink.
                // We don't recursively clone submodules here.
                fs::create_dir_all(&path)?;
            }

            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Unsupported tree mode: {mode}"),
                ));
            }
        }
    }

    Ok(())
}

// Create HEAD, branch reference, and config.
pub fn create_git_metadata(
    git_dir: &Path,
    repo_url: &str,
    branch: &str,
    commit_sha: &str,
) -> io::Result<()> {
    fs::create_dir_all(git_dir.join("objects/info"))?;
    fs::create_dir_all(git_dir.join("objects/pack"))?;
    fs::create_dir_all(git_dir.join("refs/tags"))?;

    // HEAD points to the checked-out branch.
    fs::write(git_dir.join("HEAD"), format!("ref: refs/heads/{branch}\n"))?;

    // Create branch reference.
    let branch_ref = git_dir.join("refs/heads").join(branch);

    if let Some(parent) = branch_ref.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(branch_ref, format!("{commit_sha}\n"))?;

    // Basic repository configuration.
    let config = format!(
        "[core]\n\
         \trepositoryformatversion = 0\n\
         \tfilemode = true\n\
         \tbare = false\n\
         [remote \"origin\"]\n\
         \turl = {repo_url}\n\
         \tfetch = +refs/heads/*:refs/remotes/origin/*\n"
    );

    fs::write(git_dir.join("config"), config)?;

    Ok(())
}
