//! `sem update` — self-update to the latest GitHub release.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use colored::Colorize;

const REPO: &str = "Ataraxy-Labs/sem";
const DOWNLOAD_TIMEOUT_SECS: u64 = 120;
/// Release binaries are ~15MB; refuse anything wildly larger.
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let current = env!("CARGO_PKG_VERSION");

    // Homebrew owns its files; replacing them under brew's feet breaks
    // `brew upgrade` later. Defer to it.
    let exe = std::env::current_exe()?;
    let exe_str = exe.to_string_lossy();
    if exe_str.contains("/Cellar/") || exe_str.contains("/linuxbrew/") {
        println!(
            "sem was installed with Homebrew. Update it with:\n  {}",
            "brew upgrade sem".bold()
        );
        return Ok(());
    }

    eprint!("{}", "Checking for updates...".dimmed());
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .build();

    let release: serde_json::Value = agent
        .get(&format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .set("User-Agent", "sem-cli")
        .set("Accept", "application/vnd.github+json")
        .call()?
        .into_json()?;

    let tag = release["tag_name"]
        .as_str()
        .ok_or("No tag_name in latest release")?;
    let latest = tag.trim_start_matches('v');
    eprintln!(" latest is v{latest}");

    if !is_newer(latest, current) {
        println!(
            "{} sem v{current} is already the latest version",
            "ok".green().bold()
        );
        return Ok(());
    }

    let artifact = artifact_name()?;
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{artifact}");

    println!(
        "Updating sem {} → {}",
        format!("v{current}").dimmed(),
        format!("v{latest}").bold()
    );

    // Download to a temp dir.
    let tmp = std::env::temp_dir().join(format!("sem-update-{}", std::process::id()));
    fs::create_dir_all(&tmp)?;
    let archive = tmp.join(&artifact);
    download(&agent, &url, &archive)?;

    // Best-effort checksum verification when a system sha tool exists.
    verify_checksum(&agent, tag, &artifact, &archive);

    // Extract. tar handles .tar.gz on macOS, Linux, and Windows 10+.
    let status = std::process::Command::new("tar")
        .arg("xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&tmp)
        .status()?;
    if !status.success() {
        cleanup(&tmp);
        return Err("Failed to extract release archive".into());
    }

    let new_binary = find_binary(&tmp).ok_or("No sem binary found in release archive")?;

    // Swap in place: move the running binary aside (allowed on Unix and
    // Windows), then move the new one into its path.
    let old = exe.with_extension("old");
    let _ = fs::remove_file(&old);
    if let Err(e) = fs::rename(&exe, &old) {
        cleanup(&tmp);
        return Err(format!(
            "Cannot replace {} ({e}). Try with elevated permissions, or reinstall:\n  curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | sh",
            exe.display()
        )
        .into());
    }
    if let Err(e) = fs::rename(&new_binary, &exe) {
        // Roll back so the user still has a working sem.
        let _ = fs::rename(&old, &exe);
        cleanup(&tmp);
        return Err(format!("Failed to install new binary: {e}").into());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe, fs::Permissions::from_mode(0o755));
    }

    let _ = fs::remove_file(&old); // fails harmlessly on Windows while running
    cleanup(&tmp);

    println!(
        "{} Updated to v{latest} ({})",
        "ok".green().bold(),
        exe.display()
    );
    Ok(())
}

/// Strict numeric semver comparison; non-numeric parts compare as 0.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> (u64, u64, u64) {
        let mut parts = v.split('.').map(|p| {
            p.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .unwrap_or(0)
        });
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    };
    parse(candidate) > parse(current)
}

fn artifact_name() -> Result<String, Box<dyn std::error::Error>> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        other => return Err(format!("No prebuilt binaries for OS '{other}'").into()),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => return Err(format!("No prebuilt binaries for arch '{other}'").into()),
    };
    Ok(format!("sem-{os}-{arch}.tar.gz"))
}

fn download(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let resp = agent.get(url).set("User-Agent", "sem-cli").call()?;
    let mut reader = resp.into_reader().take(MAX_DOWNLOAD_BYTES);
    let mut file = fs::File::create(dest)?;
    std::io::copy(&mut reader, &mut file)?;
    Ok(())
}

/// Verify the archive against the release's checksums.txt when shasum or
/// sha256sum is available. Hard-fails on mismatch; skips silently when no
/// tool or no checksum entry exists.
fn verify_checksum(agent: &ureq::Agent, tag: &str, artifact: &str, archive: &Path) {
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/checksums.txt");
    let Ok(resp) = agent.get(&url).set("User-Agent", "sem-cli").call() else {
        return;
    };
    let Ok(listing) = resp.into_string() else {
        return;
    };
    let Some(expected) = listing
        .lines()
        .find(|l| l.contains(artifact))
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_lowercase)
    else {
        return;
    };

    let actual = ["sha256sum", "shasum"]
        .iter()
        .find_map(|tool| {
            let mut cmd = std::process::Command::new(tool);
            if *tool == "shasum" {
                cmd.args(["-a", "256"]);
            }
            cmd.arg(archive);
            let out = cmd.output().ok()?;
            if !out.status.success() {
                return None;
            }
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .map(str::to_lowercase)
        });

    if let Some(actual) = actual {
        if actual != expected {
            eprintln!(
                "{} checksum mismatch for {artifact} — aborting",
                "error:".red().bold()
            );
            std::process::exit(1);
        }
    }
}

fn find_binary(dir: &Path) -> Option<PathBuf> {
    let names = ["sem", "sem.exe"];
    // Top level first, then one level deep (archives may nest a directory).
    for name in names {
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct);
        }
    }
    for entry in fs::read_dir(dir).ok()?.flatten() {
        if entry.path().is_dir() {
            for name in names {
                let nested = entry.path().join(name);
                if nested.is_file() {
                    return Some(nested);
                }
            }
        }
    }
    None
}

fn cleanup(tmp: &Path) {
    let _ = fs::remove_dir_all(tmp);
}
