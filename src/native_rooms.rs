//! Local native-room ownership shared by every transport configuration.
//! Locks serialize attachment and POST; durable claims never expire or transfer.
use crate::{BridgeConfig, config::phone_hex, herdr::Agent};
use anyhow::{Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

const MAX_CLAIM: u64 = 16 * 1024;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Purpose {
    Room,
    Private,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claim {
    version: u8,
    purpose: Purpose,
    native: [String; 3],
    state: PathBuf,
    owner: String,
    transport: String,
    group: String,
    key: String,
    terminal: String,
    pane: String,
}

pub(crate) fn identity(agent: &Agent) -> Result<[String; 3]> {
    let session = agent
        .agent_session
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Native session required"))?;
    Ok([
        session.agent.clone(),
        session.kind.clone(),
        session.value.clone(),
    ])
}

impl Claim {
    pub(crate) fn new(config: &BridgeConfig, agent: &Agent, group: &str) -> Result<Self> {
        Ok(Self {
            version: 1,
            purpose: Purpose::Room,
            native: identity(agent)?,
            state: fs::canonicalize(&config.state_path)?,
            owner: phone_hex(&config.phone_npub)?,
            transport: config.account_id_hex.clone(),
            group: group.into(),
            key: agent.key(),
            terminal: agent.terminal_id.clone(),
            pane: agent.pane_id.clone(),
        })
    }
    pub(crate) fn private(config: &BridgeConfig, agent: &Agent, group: &str) -> Result<Self> {
        let mut claim = Self::new(config, agent, group)?;
        claim.purpose = Purpose::Private;
        Ok(claim)
    }
    fn belongs_to(&self, config: &BridgeConfig) -> Result<bool> {
        Ok(self.version == 1
            && self.state == fs::canonicalize(&config.state_path)?
            && self.owner == phone_hex(&config.phone_npub)?
            && self.transport == config.account_id_hex)
    }
    fn matches(&self, expected: &Self) -> bool {
        if self.purpose == Purpose::Private && expected.purpose == Purpose::Private {
            // Private sessions retain their existing terminal-move recovery.
            // Ownership still binds the native ID, source/key, owner, transport,
            // state and group; only the current display location may change.
            self.version == expected.version
                && self.native == expected.native
                && self.state == expected.state
                && self.owner == expected.owner
                && self.transport == expected.transport
                && self.group == expected.group
                && self.key == expected.key
        } else {
            self == expected
        }
    }
}

fn uid() -> u32 {
    // geteuid has no pointer or allocation preconditions.
    unsafe { libc::geteuid() }
}

fn root(config: &BridgeConfig) -> Result<PathBuf> {
    if let Some(path) = &config.native_claims_fixture_root {
        ensure!(path.is_absolute(), "Fixture claim root must be absolute");
        return Ok(path.clone());
    }
    // HOME/XDG values can differ between transport processes. The passwd home
    // is fixed for this UID, so changing a transport config cannot split claims.
    let mut buffer = vec![0u8; 64 * 1024];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // getpwuid_r writes only within the supplied entry/buffer; result remains
    // valid while this buffer lives. Missing/oversized account records fail closed.
    let status = unsafe {
        libc::getpwuid_r(
            uid(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    ensure!(
        status == 0 && !result.is_null(),
        "Cannot resolve native claim owner home"
    );
    // A successful getpwuid_r returned a fully initialized record and C string.
    let home_pointer = unsafe { (*result).pw_dir };
    ensure!(
        !home_pointer.is_null(),
        "Native claim owner home is missing"
    );
    let home = unsafe { CStr::from_ptr(home_pointer) };
    let home = fs::canonicalize(Path::new(std::ffi::OsStr::from_bytes(home.to_bytes())))?;
    let metadata = fs::metadata(&home)?;
    ensure!(
        metadata.uid() == uid() && metadata.is_dir() && metadata.permissions().mode() & 0o022 == 0,
        "Native claim home is not owner-controlled"
    );
    Ok(home.join(".codr-native-rooms"))
}

fn private_directory(path: &Path, create: bool) -> Result<bool> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == uid()
            && metadata.permissions().mode() & 0o077 == 0,
        "Native claim directory is not private"
    );
    Ok(true)
}

fn paths(config: &BridgeConfig, native: &[String; 3]) -> Result<(PathBuf, PathBuf)> {
    let root = root(config)?;
    let name = hex::encode(Sha256::digest(serde_json::to_vec(native)?));
    Ok((root.clone(), root.join(name)))
}

fn check_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == uid()
            && metadata.nlink() == 1
            && metadata.permissions().mode() & 0o077 == 0
            && metadata.len() <= MAX_CLAIM,
        "Invalid native ownership file"
    );
    Ok(())
}

fn read_claim(path: &Path) -> Result<Option<Claim>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    check_file(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_CLAIM + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CLAIM,
        "Native claim exceeds limit"
    );
    // Empty, partial, old-version or otherwise ambiguous claims are retained.
    // They are not evidence that the native conversation has become available.
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub(crate) struct Guard {
    _lock: File,
    path: PathBuf,
    root: PathBuf,
    native: [String; 3],
}

pub(crate) fn guard(config: &BridgeConfig, native: [String; 3]) -> Result<Guard> {
    let (root, path) = paths(config, &native)?;
    private_directory(&root, true)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path.with_extension("lock"))?;
    check_file(&lock)?;
    lock.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("Native conversation admission is already in progress"))?;
    Ok(Guard {
        _lock: lock,
        path: path.with_extension("json"),
        root,
        native,
    })
}

impl Guard {
    /// Private native admission also needs durable provenance before POST: a
    /// lost HTTP acknowledgement must not leave the session apparently free for
    /// a room attachment while the original input can still arrive later.
    pub(crate) fn bind_private(&self, config: &BridgeConfig, claim: &Claim) -> Result<()> {
        if read_claim(&self.path)?.is_some() {
            ensure!(
                claim.purpose == Purpose::Private && claim.belongs_to(config)?,
                "Private claim provenance differs"
            );
            self.verify(claim)
        } else {
            self.reserve(claim)
        }
    }

    pub(crate) fn reserve(&self, claim: &Claim) -> Result<()> {
        ensure!(claim.native == self.native, "Native claim identity differs");
        let bytes = serde_json::to_vec(claim)?;
        ensure!(
            bytes.len() as u64 <= MAX_CLAIM,
            "Native claim exceeds limit"
        );
        // create_new is atomic across processes. Never overwrite or remove a
        // claim, including after state-save failure or an uncertain native POST.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&self.path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    pub(crate) fn verify(&self, claim: &Claim) -> Result<()> {
        ensure!(
            read_claim(&self.path)?.is_some_and(|saved| saved.matches(claim)),
            "Native room ownership is missing, changed or belongs to another transport"
        );
        Ok(())
    }
    pub(crate) fn allow(&self, config: &BridgeConfig) -> Result<()> {
        if let Some(claim) = read_claim(&self.path)? {
            ensure!(
                claim.purpose == Purpose::Private
                    && claim.native == self.native
                    && claim.belongs_to(config)?,
                "Native conversation belongs to another room transport"
            );
        }
        Ok(())
    }
}

fn existing(config: &BridgeConfig, native: &[String; 3]) -> Result<Option<Claim>> {
    let (root, path) = paths(config, native)?;
    if !private_directory(&root, false)? {
        return Ok(None);
    }
    let Some(claim) = read_claim(&path.with_extension("json"))? else {
        return Ok(None);
    };
    ensure!(
        claim.native == *native && claim.version == 1,
        "Ambiguous native ownership record"
    );
    Ok(Some(claim))
}

/// New attachment never adopts an existing reservation, even from the same
/// config after a failed state save. Admission additionally takes the OS lock.
pub(crate) fn claimed(config: &BridgeConfig, native: &[String; 3]) -> Result<bool> {
    Ok(existing(config, native)?.is_some())
}

#[cfg(test)]
fn foreign(config: &BridgeConfig, native: &[String; 3]) -> Result<bool> {
    existing(config, native)?.map_or(Ok(false), |claim| Ok(!claim.belongs_to(config)?))
}

/// Matching the UID/config alone is not enough: a room claim left behind by a
/// failed state save must never be adopted by ordinary private discovery.
pub(crate) fn binding_conflict(
    config: &BridgeConfig,
    agent: &Agent,
    group: Option<&str>,
    room: bool,
) -> Result<bool> {
    let native = identity(agent)?;
    let Some(saved) = existing(config, &native)? else {
        return Ok(room);
    };
    let Some(group) = group else { return Ok(true) };
    let expected = if room {
        Claim::new(config, agent, group)?
    } else {
        Claim::private(config, agent, group)?
    };
    Ok(!saved.matches(&expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bech32::ToBase32;
    use serde_json::json;

    fn fixture(root: &Path, state: &str) -> (BridgeConfig, Agent) {
        let mut config: BridgeConfig = serde_json::from_value(json!({
            "phone_npub":bech32::encode("npub",[0x11u8;32].to_base32(),bech32::Variant::Bech32).unwrap(),
            "herdr_socket":root.join("herdr.sock"),"wn_agent_socket":root.join("mdk.sock"),
            "account_id_hex":"aa".repeat(32),"state_path":root.join(state)
        })).unwrap();
        config.native_claims_fixture_root = Some(root.join("claims"));
        fs::write(&config.state_path, "{}").unwrap();
        let agent = serde_json::from_value(json!({"pane_id":"w1:p1","terminal_id":"term1","agent":"opencode","agent_status":"idle",
            "agent_session":{"agent":"opencode","kind":"id","source":"herdr:opencode","value":"ses_fixture"}})).unwrap();
        (config, agent)
    }

    #[test]
    fn native_claims_survive_restart_and_reject_config_owner_and_metadata_drift() {
        let temp = tempfile::tempdir().unwrap();
        let (config, agent) = fixture(temp.path(), "first.json");
        let (mut other, _) = fixture(temp.path(), "second.json");
        let native = identity(&agent).unwrap();
        let claim = Claim::new(&config, &agent, "group").unwrap();
        let held = guard(&config, native.clone()).unwrap();
        held.reserve(&claim).unwrap();
        assert!(
            guard(&other, native.clone()).is_err(),
            "Even another config must take the same OS lock"
        );
        drop(held);
        let restarted = guard(&config, native.clone()).unwrap();
        restarted.verify(&claim).unwrap();
        assert!(restarted.allow(&other).is_err());
        assert!(
            restarted.reserve(&claim).is_err(),
            "Existing records never become takeover opportunities"
        );
        drop(restarted);
        assert!(foreign(&other, &native).unwrap());
        other.state_path = config.state_path.clone();
        other.account_id_hex = "bb".repeat(32);
        assert!(
            foreign(&other, &native).unwrap(),
            "Transport identity is part of ownership"
        );
        other.account_id_hex = config.account_id_hex.clone();
        other.phone_npub =
            bech32::encode("npub", [0x22u8; 32].to_base32(), bech32::Variant::Bech32).unwrap();
        assert!(
            foreign(&other, &native).unwrap(),
            "Owner identity is part of ownership"
        );
        let mut changed = agent.clone();
        changed.terminal_id = "changed".into();
        let held = guard(&config, identity(&changed).unwrap()).unwrap();
        assert!(
            held.verify(&Claim::new(&config, &changed, "group").unwrap())
                .is_err()
        );
        drop(held);
        changed = agent.clone();
        changed.agent_session.as_mut().unwrap().source = "different-source".into();
        assert_eq!(
            identity(&changed).unwrap(),
            native,
            "Source is not a separate reservation namespace"
        );
        let held = guard(&config, identity(&changed).unwrap()).unwrap();
        assert!(
            held.verify(&Claim::new(&config, &changed, "group").unwrap())
                .is_err()
        );
    }

    #[test]
    fn native_claims_hold_ambiguous_files_and_allow_unrelated_conversations() {
        let temp = tempfile::tempdir().unwrap();
        let (config, agent) = fixture(temp.path(), "state.json");
        let native = identity(&agent).unwrap();
        let held = guard(&config, native.clone()).unwrap();
        fs::write(&held.path, b"").unwrap();
        fs::set_permissions(&held.path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(held.allow(&config).is_err());
        assert!(
            held.reserve(&Claim::new(&config, &agent, "group").unwrap())
                .is_err()
        );
        assert!(
            held.path.exists(),
            "Ambiguous provenance is retained, not cleaned up"
        );
        let mut other = agent.clone();
        other.agent_session.as_mut().unwrap().value = "ses_unrelated".into();
        let unrelated = guard(&config, identity(&other).unwrap()).unwrap();
        unrelated
            .reserve(&Claim::new(&config, &other, "other-group").unwrap())
            .unwrap();
        unrelated
            .verify(&Claim::new(&config, &other, "other-group").unwrap())
            .unwrap();
    }

    #[test]
    fn native_claims_reject_unsafe_directory_and_fixture_root_is_not_configurable() {
        let temp = tempfile::tempdir().unwrap();
        let (config, agent) = fixture(temp.path(), "state.json");
        let path = config.native_claims_fixture_root.as_ref().unwrap();
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(guard(&config, identity(&agent).unwrap()).is_err());
        let mut value = json!({
            "phone_npub":config.phone_npub,"herdr_socket":config.herdr_socket,"wn_agent_socket":config.wn_agent_socket,
            "account_id_hex":config.account_id_hex,"state_path":config.state_path
        });
        value["native_claims_fixture_root"] = json!(temp.path().join("bypass"));
        assert!(serde_json::from_value::<BridgeConfig>(value).is_err());
    }

    #[test]
    fn private_admission_claim_is_not_released_by_an_uncertain_send_or_restart() {
        let temp = tempfile::tempdir().unwrap();
        let (config, agent) = fixture(temp.path(), "private.json");
        let (room_config, _) = fixture(temp.path(), "room.json");
        let native = identity(&agent).unwrap();
        let private_claim = Claim::private(&config, &agent, "private-group").unwrap();
        let held = guard(&config, native.clone()).unwrap();
        held.bind_private(&config, &private_claim).unwrap();
        // Losing the guard/process releases only the OS lock, never the claim.
        drop(held);
        let room = guard(&room_config, native.clone()).unwrap();
        assert!(
            room.reserve(&Claim::new(&room_config, &agent, "room-group").unwrap())
                .is_err()
        );
        assert!(room.allow(&room_config).is_err());
        drop(room);
        let restarted = guard(&config, native).unwrap();
        restarted.bind_private(&config, &private_claim).unwrap();
        restarted.verify(&private_claim).unwrap();
    }

    #[test]
    fn private_binding_can_move_terminal_but_not_group_or_source() {
        let temp = tempfile::tempdir().unwrap();
        let (config, agent) = fixture(temp.path(), "private.json");
        let held = guard(&config, identity(&agent).unwrap()).unwrap();
        held.bind_private(
            &config,
            &Claim::private(&config, &agent, "private-group").unwrap(),
        )
        .unwrap();
        let mut moved = agent.clone();
        moved.terminal_id = "moved-terminal".into();
        moved.pane_id = "moved-pane".into();
        held.bind_private(
            &config,
            &Claim::private(&config, &moved, "private-group").unwrap(),
        )
        .unwrap();
        assert!(!binding_conflict(&config, &moved, Some("private-group"), false).unwrap());
        assert!(binding_conflict(&config, &moved, Some("other-group"), false).unwrap());
        assert!(
            held.bind_private(
                &config,
                &Claim::private(&config, &moved, "other-group").unwrap()
            )
            .is_err()
        );
        moved.agent_session.as_mut().unwrap().source = "different-source".into();
        assert!(binding_conflict(&config, &moved, Some("private-group"), false).unwrap());
    }

    #[test]
    fn a_room_claim_without_matching_state_cannot_be_adopted_by_private_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let (config, agent) = fixture(temp.path(), "state.json");
        let held = guard(&config, identity(&agent).unwrap()).unwrap();
        held.reserve(&Claim::new(&config, &agent, "room-group").unwrap())
            .unwrap();
        drop(held);
        assert!(binding_conflict(&config, &agent, None, false).unwrap());
        assert!(binding_conflict(&config, &agent, Some("private-group"), false).unwrap());
        let held = guard(&config, identity(&agent).unwrap()).unwrap();
        assert!(
            held.bind_private(
                &config,
                &Claim::private(&config, &agent, "private-group").unwrap()
            )
            .is_err()
        );
        assert!(
            held.allow(&config).is_err(),
            "Unmanaged prompts cannot use a room reservation"
        );
        assert!(!binding_conflict(&config, &agent, Some("room-group"), true).unwrap());
    }
}
