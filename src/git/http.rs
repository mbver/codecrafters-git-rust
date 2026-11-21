use std::error::Error;

use reqwest::blocking::Client;

use crate::git::pkt_line::{encode_pkt_line, parse_pkt_lines};

pub fn get_head(repo_url: &str) -> Result<(String, String), Box<dyn Error>> {
    let url = format!(
        "{}/info/refs?service=git-upload-pack",
        repo_url.trim_end_matches('/')
    );

    let response = Client::new().get(url).send()?.error_for_status()?;

    let data = response.bytes()?;
    let packets = parse_pkt_lines(&data)?;

    let mut head_sha = None;
    let mut branch = None;

    for packet in packets {
        let packet = packet.strip_suffix(b"\n").unwrap_or(&packet);

        if packet.starts_with(b"# service=") {
            continue;
        }

        // Separate reference information from capabilities.
        let (reference_data, capabilities) = match packet.iter().position(|&b| b == 0) {
            Some(index) => (&packet[..index], &packet[index + 1..]),
            None => (packet, &[][..]),
        };

        let reference_data = std::str::from_utf8(reference_data)?;

        let Some((sha, reference)) = reference_data.split_once(' ') else {
            continue;
        };

        if reference == "HEAD" {
            head_sha = Some(sha.to_string());
        }

        // The default branch is advertised as:
        // symref=HEAD:refs/heads/main
        let capabilities = String::from_utf8_lossy(capabilities);

        for capability in capabilities.split_whitespace() {
            if let Some(name) = capability.strip_prefix("symref=HEAD:refs/heads/") {
                branch = Some(name.to_string());
            }
        }
    }

    let head_sha = head_sha.ok_or("HEAD reference not found")?;

    // Fallback: find a branch pointing at the same commit.
    // Some servers may not advertise symref.
    if branch.is_none() {
        for packet in packets_for_fallback(&head_sha, repo_url)? {
            if let Some(name) = packet {
                branch = Some(name);
                break;
            }
        }
    }

    let branch = branch.ok_or("Default branch not found")?;

    Ok((head_sha, branch))
}

// Fallback discovery when symref isn't advertised.
fn packets_for_fallback(
    head_sha: &str,
    repo_url: &str,
) -> Result<Vec<Option<String>>, Box<dyn Error>> {
    let url = format!(
        "{}/info/refs?service=git-upload-pack",
        repo_url.trim_end_matches('/')
    );

    let response = Client::new().get(url).send()?.error_for_status()?;

    let data = response.bytes()?;
    let packets = parse_pkt_lines(&data)?;

    let mut result = Vec::new();

    for packet in packets {
        let packet = packet.strip_suffix(b"\n").unwrap_or(&packet);

        let reference_data = match packet.iter().position(|&b| b == 0) {
            Some(index) => &packet[..index],
            None => packet,
        };

        let Ok(reference_data) = std::str::from_utf8(reference_data) else {
            continue;
        };

        let Some((sha, reference)) = reference_data.split_once(' ') else {
            continue;
        };

        if sha == head_sha {
            if let Some(name) = reference.strip_prefix("refs/heads/") {
                result.push(Some(name.to_string()));
            }
        }
    }

    Ok(result)
}

pub fn fetch_pack(repo_url: &str, commit_sha: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let url = format!("{}/git-upload-pack", repo_url.trim_end_matches('/'));

    let mut request = Vec::new();

    // Ask for the latest commit and its reachable objects.
    let want = format!("want {commit_sha} side-band-64k ofs-delta\n");

    request.extend_from_slice(&encode_pkt_line(want.as_bytes()));

    // End the want section.
    request.extend_from_slice(b"0000");

    // We have no objects to negotiate.
    request.extend_from_slice(&encode_pkt_line(b"done\n"));

    let response = Client::new()
        .post(url)
        .header("Content-Type", "application/x-git-upload-pack-request")
        .header("Accept", "application/x-git-upload-pack-result")
        .body(request)
        .send()?
        .error_for_status()?;

    let data = response.bytes()?;
    let packets = parse_pkt_lines(&data)?;

    let mut packfile = Vec::new();

    for packet in packets {
        if packet.starts_with(b"NAK") {
            continue;
        }

        if packet.is_empty() {
            continue;
        }

        match packet[0] {
            1 => {
                // Channel 1: packfile data.
                packfile.extend_from_slice(&packet[1..]);
            }
            2 => {
                // Channel 2: progress messages.
                let message = String::from_utf8_lossy(&packet[1..]);
                eprint!("{message}");
            }
            3 => {
                // Channel 3: fatal error.
                let message = String::from_utf8_lossy(&packet[1..]);
                return Err(message.into());
            }
            channel => {
                return Err(format!("Unknown sideband channel: {channel}").into());
            }
        }
    }

    if !packfile.starts_with(b"PACK") {
        return Err("Invalid packfile response".into());
    }

    Ok(packfile)
}
