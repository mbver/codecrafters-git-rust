pub mod checkout;
pub mod http;
pub mod object;
pub mod pack;
pub mod pkt_line;

use std::error::Error;
use std::fs;
use std::path::Path;

pub fn clone_repo(repo_url: &str, target_dir: &Path) -> Result<(), Box<dyn Error>> {
    // Create target directory and .git.
    fs::create_dir_all(target_dir)?;

    let git_dir = target_dir.join(".git");
    fs::create_dir_all(&git_dir)?;

    // 1. Discover default branch and latest commit.
    let (commit_sha, branch) = http::get_head(repo_url)?;

    println!("Cloning {repo_url}");
    println!("Branch: {branch}");
    println!("Commit: {commit_sha}");

    // 2. Request packfile.
    let packfile = http::fetch_pack(repo_url, &commit_sha)?;

    println!("Received packfile: {} bytes", packfile.len());

    // 3. Parse packfile.
    let entries = pack::parse_pack_entries(&packfile)?;

    println!("Packfile contains {} objects", entries.len());

    // 4. Resolve deltas and store all objects.
    pack::resolve_pack(entries, &git_dir)?;

    // 5. Read root tree SHA from latest commit.
    let tree_sha = checkout::get_commit_tree(&git_dir, &commit_sha)?;

    // 6. Reconstruct working directory.
    checkout::checkout_tree(&git_dir, &tree_sha, target_dir)?;

    // 7. Create Git metadata.
    checkout::create_git_metadata(&git_dir, repo_url, &branch, &commit_sha)?;

    println!("Clone completed.");

    Ok(())
}
