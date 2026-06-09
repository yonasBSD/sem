use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use colored::Colorize;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
pub struct CloudCredentials {
    pub api_key: String,
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
}

fn default_endpoint() -> String {
    "https://api.sem.sh".into()
}

fn credentials_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(PathBuf::from(home).join(".sem").join("credentials.json"))
}

/// Load stored cloud credentials, if any.
pub fn load_credentials() -> Option<CloudCredentials> {
    let path = credentials_path()?;
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn login(api_key: Option<String>, endpoint: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let key = match api_key {
        Some(k) => k,
        None => {
            eprint!("{}", "Enter your API key: ".bold());
            io::stderr().flush()?;
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line)?;
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                return Err("No API key provided".into());
            }
            trimmed
        }
    };

    if !key.starts_with("sk_live_") {
        eprintln!(
            "{} Key doesn't start with sk_live_ — are you sure this is correct?",
            "warning:".yellow().bold()
        );
    }

    let ep = endpoint.unwrap_or_else(default_endpoint);

    let creds = CloudCredentials {
        api_key: key,
        endpoint: ep.clone(),
    };

    let path = credentials_path().ok_or("Could not determine home directory")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(&creds)?;
    fs::write(&path, json)?;

    // Restrict file permissions on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }

    println!("{} Logged in to {}", "✓".green().bold(), ep);
    println!("  Credentials saved to {}", path.display());

    Ok(())
}

pub fn logout() -> Result<(), Box<dyn std::error::Error>> {
    let path = credentials_path().ok_or("Could not determine home directory")?;

    if path.exists() {
        fs::remove_file(&path)?;
        println!("{} Logged out — credentials removed", "✓".green().bold());
    } else {
        println!("{} No credentials found — already logged out", "✓".green().bold());
    }

    Ok(())
}

pub fn whoami() -> Result<(), Box<dyn std::error::Error>> {
    let creds = load_credentials().ok_or("Not logged in. Run: sem login")?;

    // Mask the key: show first 12 chars + last 4
    let masked = if creds.api_key.len() > 16 {
        format!(
            "{}...{}",
            &creds.api_key[..12],
            &creds.api_key[creds.api_key.len() - 4..]
        )
    } else {
        creds.api_key.clone()
    };

    println!("{} {}", "Endpoint:".bold(), creds.endpoint);
    println!("{} {}", "API Key: ".bold(), masked);

    Ok(())
}
