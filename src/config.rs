use anyhow::{Result, ensure};
use bech32::FromBase32;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    pub phone_npub: String,
    pub herdr_socket: PathBuf,
    pub wn_agent_socket: PathBuf,
    pub account_id_hex: String,
    pub state_path: PathBuf,
    /// Isolated in-process fixture storage, never accepted from a config file.
    /// Production callers leave this unset to use the UID's fixed home directory.
    #[serde(skip)]
    #[doc(hidden)]
    pub native_claims_fixture_root: Option<PathBuf>,
    /// Owner-confirmed workspace launches are restricted to this directory.
    #[serde(default = "default_launch_root")]
    pub launch_root: PathBuf,
    /// Optional same-user desktop endpoint for portr; does not configure SSH.
    #[serde(default)]
    pub portr_enabled: bool,
    /// Create a standalone owner-only control group for /new and general commands.
    #[serde(default)]
    pub control_chat: bool,
    #[serde(default = "default_opencode")]
    pub opencode_bin: PathBuf,
    /// Read-only V2 HTTP connection; never starts or recovers the shared service.
    #[serde(default)]
    pub opencode_service_file: Option<PathBuf>,
    /// Explicit database/profile selection. None keeps the CLI export reader.
    #[serde(default)]
    pub opencode_db: Option<PathBuf>,
    #[serde(default = "default_codex_home")]
    pub codex_home: PathBuf,
    #[serde(default = "default_claude_home")]
    pub claude_home: PathBuf,
    /// Requires an MDK connector with edit_message support.
    #[serde(default)]
    pub edit_responses: bool,
    /// Local native user input mutes all groups; authorized phone input wakes them.
    #[serde(default = "automatic_default")]
    pub auto_forwarding: bool,
    /// Executable and fixed arguments. The downloaded audio path is appended.
    #[serde(default)]
    pub transcribe_command: Vec<String>,
}

fn automatic_default() -> bool {
    true
}

fn default_opencode() -> PathBuf {
    "opencode".into()
}

fn default_launch_root() -> PathBuf {
    native_home("HOME", ".").join("code")
}

fn native_home(variable: &str, directory: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(directory)
        })
}
fn default_codex_home() -> PathBuf {
    native_home("CODEX_HOME", ".codex")
}
fn default_claude_home() -> PathBuf {
    native_home("CLAUDE_CONFIG_DIR", ".claude")
}

pub fn phone_hex(npub: &str) -> Result<String> {
    let (prefix, data, variant) = bech32::decode(npub)?;
    let bytes = Vec::<u8>::from_base32(&data)?;
    ensure!(
        prefix == "npub" && variant == bech32::Variant::Bech32 && bytes.len() == 32,
        "Invalid phone npub"
    );
    Ok(hex::encode(bytes))
}

impl BridgeConfig {
    pub fn from_toml(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        phone_hex(&config.phone_npub)?;
        ensure!(
            phone_hex(&config.phone_npub)? != config.account_id_hex,
            "The bridge requires its own account, separate from the phone"
        );
        ensure!(
            config.account_id_hex.len() == 64
                && config
                    .account_id_hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Invalid bridge account ID"
        );
        for path in [
            &config.herdr_socket,
            &config.wn_agent_socket,
            &config.state_path,
            &config.codex_home,
            &config.claude_home,
            &config.launch_root,
        ] {
            ensure!(path.is_absolute(), "Configuration paths must be absolute");
        }
        ensure!(
            config.launch_root.parent().is_some(),
            "Launch root must not be the filesystem root"
        );
        if let Some(path) = &config.opencode_db {
            ensure!(
                path.is_absolute(),
                "OpenCode database path must be absolute"
            );
        }
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self> {
        Self::from_toml(&std::fs::read_to_string(path)?)
    }
}
