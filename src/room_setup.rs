//! Fresh-only onboarding journal. No bridge discovery runs in this module.
//! A pending effect is deliberately not resumable: inspect it, never allocate a
//! replacement account/session or replay a possibly successful operation.
use crate::{
    BridgeConfig, BridgeState, bridge::Bridge, config::phone_hex, messaging::EmbeddedMessaging,
    wire::Socket,
};
use anyhow::{Result, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Reserve an unused home, create a transport and publish its invite details.
    Init(Options),
    /// Read the exact authenticated target roster, without discovery or invocation.
    Roster {
        #[arg(long)]
        root: PathBuf,
    },
    /// Create and claim a fresh native session, then bind the accepted target room.
    Bind {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        roles: PathBuf,
    },
}

#[derive(Clone, Debug, Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    #[arg(long)]
    pub root: PathBuf,
    #[arg(long)]
    pub owner_npub: String,
    #[arg(long)]
    pub target_group: String,
    #[arg(long)]
    pub alias: String,
    #[arg(long)]
    pub directory: PathBuf,
    #[arg(long)]
    pub authorised_inviter: String,
    #[arg(long)]
    pub herdr_socket: PathBuf,
    #[arg(long)]
    pub opencode_service_file: PathBuf,
    #[arg(long)]
    pub opencode_bin: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    options: Options,
    phase: String,
    account: Option<String>,
    native_session: Option<String>,
    key: Option<String>,
}

fn private_metadata(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    // Never repair permissions or follow a final symlink on retained state.
    ensure!(
        !m.file_type().is_symlink()
            && m.uid() == unsafe { libc::geteuid() }
            && m.permissions().mode() & 0o077 == 0
            && if directory {
                m.is_dir()
            } else {
                m.is_file() && m.nlink() == 1
            },
        "Setup state must be private and owner-controlled"
    );
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

impl Journal {
    fn save(&self) -> Result<()> {
        private_metadata(&self.options.root, true)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.options.root)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.as_file().sync_all()?;
        file.persist(self.options.root.join("setup.json"))?;
        fs::File::open(&self.options.root)?.sync_all()?;
        Ok(())
    }
    fn phase(&mut self, phase: &str) -> Result<()> {
        self.phase = phase.into();
        self.save()
    }
    fn load(root: &Path) -> Result<Self> {
        ensure!(
            fs::canonicalize(root)? == root,
            "Use the original physical setup root"
        );
        private_metadata(root, true)?;
        private_metadata(&root.join("setup.json"), false)?;
        ensure!(
            fs::metadata(root.join("setup.json"))?.len() <= 64 * 1024,
            "Setup journal exceeds limit"
        );
        let journal: Self = serde_json::from_slice(&fs::read(root.join("setup.json"))?)?;
        ensure!(journal.options.root == root, "Setup root changed");
        Ok(journal)
    }
    fn config_text(&self) -> Result<String> {
        let o = &self.options;
        // JSON strings are also TOML basic strings for these validated inputs.
        let q = |v: &str| serde_json::to_string(v).unwrap();
        Ok(format!(
            "phone_npub = {}\naccount_id_hex = {}\nherdr_socket = {}\nwn_agent_socket = {}\nstate_path = {}\nlaunch_root = {}\nopencode_service_file = {}\nopencode_bin = {}\ncontrol_chat = false\nportr_enabled = true\n",
            q(&o.owner_npub),
            q(self.account.as_deref().unwrap_or(&"0".repeat(64))),
            q(o.herdr_socket.to_str().unwrap()),
            q(o.root.join("wn.sock").to_str().unwrap()),
            q(o.root.join("state.json").to_str().unwrap()),
            q(o.directory.to_str().unwrap()),
            q(o.opencode_service_file.to_str().unwrap()),
            q(o.opencode_bin.to_str().unwrap())
        ))
    }
    fn config(&self) -> Result<BridgeConfig> {
        BridgeConfig::from_toml(&self.config_text()?)
    }
}

fn validate(o: &Options) -> Result<()> {
    phone_hex(&o.owner_npub)?;
    phone_hex(&o.authorised_inviter)?;
    ensure!(
        !o.target_group.is_empty()
            && o.target_group.len() <= 256
            && o.target_group.len().is_multiple_of(2)
            && o.target_group
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "Canonical target group hex required"
    );
    ensure!(
        !o.alias.is_empty()
            && o.alias.len() <= 32
            && o.alias
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
        "Stable lowercase alias required"
    );
    ensure!(
        o.root.is_absolute() && o.root.to_str().is_some_and(|s| s.len() <= 60),
        "Use an absolute short root (at most 60 bytes)"
    );
    let parent = o
        .root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Setup parent required"))?;
    ensure!(
        fs::canonicalize(parent)? == parent,
        "Use a physical setup parent, not a symlink"
    );
    let m = fs::metadata(parent)?;
    ensure!(
        m.uid() == unsafe { libc::geteuid() } && m.permissions().mode() & 0o022 == 0,
        "Setup parent must be owner-controlled"
    );
    for path in [
        &o.directory,
        &o.herdr_socket,
        &o.opencode_service_file,
        &o.opencode_bin,
    ] {
        ensure!(
            path.is_absolute() && path.to_str().is_some() && fs::canonicalize(path)? == *path,
            "Explicit physical existing paths required"
        );
    }
    ensure!(
        o.directory.is_dir() && o.directory.parent().is_some() && o.opencode_bin.is_file(),
        "Project directory and OpenCode executable required"
    );
    private_metadata(&o.opencode_service_file, false)?;
    Ok(())
}

fn refuse_retained_installation(home: &Path, config_home: &Path) -> Result<()> {
    // This is not a handover or migration. Custom installations must also be
    // inspected by the receiving agent, never stopped or replaced by this path.
    for path in [
        home.join(".marmot-agents/herdr"),
        home.join(".config/herdr-white-noise-bridge"),
        config_home.join("herdr-white-noise-bridge"),
        home.join("Library/LaunchAgents/io.github.tidley.codr.wn-agent-herdr.plist"),
        home.join("Library/LaunchAgents/io.github.tidley.codr.herdr-white-noise-bridge.plist"),
    ] {
        ensure!(
            matches!(fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
            "Existing Codr configuration/account/service refused; no fresh setup over a retained installation"
        );
    }
    Ok(())
}

async fn messaging(j: &Journal) -> Result<EmbeddedMessaging> {
    EmbeddedMessaging::start(&j.config()?, j.options.root.join("messaging")).await
}

async fn accounts(config: &BridgeConfig) -> Result<Value> {
    let socket = Socket {
        path: config.wn_agent_socket.clone(),
    };
    // Only the read is retried while this process's connector binds its socket.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match socket.marmot(json!({"type":"account_list"})).await {
                Err(e) if e.is::<crate::wire::NotSubmitted>() => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                result => return result,
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("Messaging readiness unavailable"))?
}

fn fresh_accounts(response: &Value) -> Result<()> {
    ensure!(
        response["type"] == "account_list"
            && response["accounts"].as_array().is_some_and(Vec::is_empty),
        "Fresh setup refuses every existing account; bootstrap reuse is forbidden"
    );
    Ok(())
}

async fn context(j: &Journal) -> Result<BTreeSet<String>> {
    let config = j.config()?;
    let list = accounts(&config).await?;
    let rows = list["accounts"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Account list unavailable"))?;
    ensure!(
        rows.len() == 1 && rows[0]["account_id_hex"] == config.account_id_hex,
        "Fresh setup account cohort changed"
    );
    let value = Socket { path: config.wn_agent_socket.clone() }.marmot(json!({"type":"room_context",
        "account_id_hex":config.account_id_hex,"group_id_hex":j.options.target_group,"message_id_hex":null})).await?;
    target_members(&config, &j.options.target_group, &value)
}

fn target_members(
    config: &BridgeConfig,
    target_group: &str,
    value: &Value,
) -> Result<BTreeSet<String>> {
    ensure!(
        value["type"] == "room_context"
            && value["account_id_hex"] == config.account_id_hex
            && value["group_id_hex"] == target_group,
        "Exact accepted target room required; welcomer allowlist does not select a room"
    );
    let members: Vec<String> = serde_json::from_value(value["members"].clone())?;
    let unique: BTreeSet<_> = members.iter().cloned().collect();
    ensure!(
        !members.is_empty()
            && members.len() <= 256
            && members.len() == unique.len()
            && members.iter().all(|m| crate::rooms::valid_id(m))
            && unique.contains(&phone_hex(&config.phone_npub)?)
            && unique.contains(&config.account_id_hex),
        "Authenticated owner and transport roster required"
    );
    Ok(unique)
}

pub async fn execute(action: &Action) -> Result<()> {
    match action {
        Action::Init(options) => init(options.clone()).await,
        Action::Roster { root } | Action::Bind { root, .. } => {
            let mut journal = Journal::load(root)?;
            ensure!(
                journal.phase == "invitation_required",
                "Setup has retained effects; inspect journal, never replay or replace them"
            );
            let _lock = crate::state::lock(&root.join("state.json"))?;
            let runtime = messaging(&journal).await?;
            let result = async {
                let members = context(&journal).await?;
                match action {
                    Action::Roster { .. } => { println!("{}", json!({"members":members,"owner":phone_hex(&journal.options.owner_npub)?,"transport":journal.account})); Ok(()) },
                    Action::Bind { roles, .. } => {
                        let roles: BTreeMap<String, crate::rooms::AccountRole> = serde_json::from_slice(&fs::read(roles)?)?;
                        ensure!(roles.keys().cloned().collect::<BTreeSet<_>>() == members, "Owner must explicitly classify the full authenticated roster");
                        let mut policy = crate::rooms::RoomPolicy::new(phone_hex(&journal.options.owner_npub)?, journal.account.clone().unwrap())?;
                        policy.set_roster(&phone_hex(&journal.options.owner_npub)?, roles)?;
                        bind(&mut journal, policy).await
                    },
                    _ => unreachable!(),
                }
            }.await;
            runtime.shutdown().await?;
            result
        }
    }
}

/// Generated onboarding configurations cannot fall into ordinary private
/// discovery after a crash halfway through setup. Legacy configs are unchanged.
pub fn validate_run(path: &Path, config: &BridgeConfig) -> Result<()> {
    let Some(root) = path.parent() else {
        return Ok(());
    };
    match fs::symlink_metadata(root.join("setup.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        result => {
            result?;
        }
    }
    let root = fs::canonicalize(root)?;
    let journal = Journal::load(&root)?;
    ensure!(
        journal.phase == "bound" && fs::read_to_string(path)? == journal.config_text()?,
        "Incomplete or changed room setup must not start discovery"
    );
    private_metadata(&root.join("adapter-installed.json"), false)?;
    let state = BridgeState::load(&config.state_path)?;
    let mapping = journal.key.as_ref().and_then(|key| state.mappings.get(key));
    ensure!(
        state.mappings.len() == 1
            && mapping.is_some_and(|m| m.room.is_some()
                && m.group_id_hex.as_deref() == Some(journal.options.target_group.as_str())),
        "Exact strict room mapping required before discovery"
    );
    Ok(())
}

async fn init(options: Options) -> Result<()> {
    validate(&options)?;
    let home = PathBuf::from(
        std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("Owner home required"))?,
    );
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    refuse_retained_installation(&home, &config_home)?;
    let mut j = Journal {
        options,
        phase: "messaging_home_pending".into(),
        account: None,
        native_session: None,
        key: None,
    };
    let registration = crate::output::service::registration(&j.config()?).await?;
    ensure!(
        registration
            .password
            .as_ref()
            .is_some_and(|s| !s.is_empty()),
        "Authenticated local OpenCode service required before account provisioning"
    );
    // create() refuses even an empty existing root, including failed attempts.
    fs::DirBuilder::new().mode(0o700).create(&j.options.root)?;
    j.save()?;
    let _lock = crate::state::lock(&j.options.root.join("state.json"))?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(j.options.root.join("messaging"))?;
    j.phase("connector_pending")?;
    let runtime = messaging(&j).await?;
    let result = async {
        fresh_accounts(&accounts(&j.config()?).await?)?;
        j.phase("bootstrap_pending")?;
        // This call can create an account, publish a KeyPackage and set policy.
        // Any failure leaves bootstrap_pending and cannot be retried by this CLI.
        let result = agent_connector::run_bootstrap(agent_connector::BootstrapOptions {
            home: j.options.root.join("messaging"), socket: j.options.root.join("wn.sock"),
            label: format!("codr-room-{}", j.options.alias), account_id_hex: None, auth_token: None,
            relays: agent_connector::DEFAULT_RELAYS.iter().map(|r| r.to_string()).collect(),
            allow_welcomers: vec![j.options.authorised_inviter.clone()],
            invite_policy: Some(serde_json::from_value(json!("allowlist"))?),
            create_if_missing: true, publish_key_package: true,
            wait_for_socket: Duration::from_secs(10), request_timeout: Duration::from_secs(30),
        }).await.map_err(|_| anyhow::anyhow!("Bootstrap unconfirmed; preserve setup journal and account home; no automatic retry"))?;
        ensure!(result.created && result.local_signing && result.key_package_published, "Fresh bootstrap was not confirmed");
        j.account = Some(result.account_id_hex);
        j.phase("invitation_required")?;
        println!("{}", json!({"npub":result.npub,"nprofile":result.nprofile,"target_group":j.options.target_group,"authorised_inviter":j.options.authorised_inviter,"next":"Authorised inviter must send a signed invitation; then read roster and obtain explicit owner classifications"}));
        Ok(())
    }.await;
    runtime.shutdown().await?;
    result
}

async fn native_api(config: &BridgeConfig, body: &Value) -> Result<Value> {
    let registration = crate::output::service::registration(config).await?;
    ensure!(
        registration
            .password
            .as_ref()
            .is_some_and(|s| !s.is_empty()),
        "Authenticated local OpenCode service required"
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()?;
    let mut response = client
        .post(format!("{}api/session", registration.url))
        .basic_auth("opencode", registration.password)
        .json(body)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Native session creation unconfirmed; do not retry"))?;
    ensure!(
        response.status().is_success()
            && response.content_length().is_none_or(|n| n <= 1024 * 1024),
        "Native session creation unconfirmed"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= 1024 * 1024,
            "Native response exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

async fn bind(j: &mut Journal, policy: crate::rooms::RoomPolicy) -> Result<()> {
    let config = j.config()?;
    ensure!(
        !config.state_path.exists()
            && !j.options.root.join("config.toml").exists()
            && !j.options.root.join("room-input.json").exists(),
        "Existing configuration, mapping or binding refused"
    );
    j.phase("state_pending")?;
    BridgeState::default().save(&config.state_path)?;
    write_new(
        &j.options.root.join("config.toml"),
        j.config_text()?.as_bytes(),
    )?;
    let session = format!("ses_codr_{}", uuid::Uuid::new_v4().simple());
    j.native_session = Some(session.clone());
    j.phase("native_session_pending")?;
    // Persist ownership BEFORE creating the native session or making it visible
    // through Herdr. A lock alone vanishes on a crash; this reservation does not.
    let guard =
        crate::native_rooms::guard(&config, ["opencode".into(), "id".into(), session.clone()])?;
    ensure!(
        !crate::native_rooms::claimed(&config, &["opencode".into(), "id".into(), session.clone()])?,
        "Native session already has a retained claim; no creation attempted"
    );
    guard.reserve_setup(&config, &j.options.target_group)?;
    let created = native_api(
        &config,
        &json!({"id":session,"title":j.options.alias,"location":{"directory":j.options.directory}}),
    )
    .await?;
    ensure!(
        created["data"]["id"] == session
            && created["data"]["location"]["directory"]
                == j.options.directory.to_string_lossy().as_ref(),
        "Native session identity/location unconfirmed"
    );
    j.phase("workspace_pending")?;
    let socket = Socket {
        path: config.herdr_socket.clone(),
    };
    let workspace = socket
        .herdr(
            "workspace.create",
            json!({"cwd":j.options.directory,"label":j.options.alias,"focus":false}),
        )
        .await?;
    let result = &workspace["result"];
    ensure!(
        result["type"] == "workspace_created",
        "Workspace creation unconfirmed"
    );
    let pane = result["root_pane"]["pane_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Workspace pane unconfirmed"))?;
    let terminal = result["root_pane"]["terminal_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Workspace terminal unconfirmed"))?;
    j.phase("agent_start_pending")?;
    let name = format!("codr-{}", uuid::Uuid::new_v4().simple());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let started = loop {
        let result = socket
            .herdr(
                "agent.start",
                json!({"name":name,"kind":"opencode","pane_id":pane,"args":["--session",session]}),
            )
            .await;
        // Herdr explicitly returns this before submitting input while the new
        // pane's shell starts. No other failed start can be replayed.
        if result
            .as_ref()
            .is_err_and(|e| e.to_string() == "Remote request failed: agent_pane_busy")
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(250)).await;
        } else {
            break result?;
        }
    };
    ensure!(
        started["result"]["type"] == "agent_started",
        "Agent start unconfirmed; do not retry"
    );
    let agent = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let agents = crate::herdr::snapshot(&socket).await?;
            if let Some(agent) = agents.into_iter().find(|a| {
                a.pane_id == pane
                    && a.terminal_id == terminal
                    && a.settled()
                    && a.agent_session.as_ref().is_some_and(|s| {
                        s.agent == "opencode" && s.kind == "id" && s.value == session
                    })
            }) {
                return Ok::<_, anyhow::Error>(agent);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Exact native session not confirmed in Herdr; do not restart or allocate another"
        )
    })??;
    let snapshot = crate::output::service::Reader::default()
        .read(&config, &session)
        .await?;
    ensure!(
        snapshot.history.user_inputs.is_empty()
            && snapshot.history.finals.is_empty()
            && snapshot.history.turns.is_empty()
            && snapshot.history.recent_messages.is_empty(),
        "Native session is not empty"
    );
    j.key = Some(agent.key());
    j.phase("claim_pending")?;
    j.phase("binding_pending")?;
    let mut bridge = Bridge::open(config.clone())?;
    bridge
        .attach_reserved_room(
            j.key.as_ref().unwrap(),
            &j.options.target_group,
            &j.options.alias,
            &guard,
        )
        .await?;
    ensure!(
        context(j).await? == policy.accounts().into_iter().collect(),
        "Authenticated roster changed during setup; retain binding and inspect before invocation"
    );
    // The policy has a complete, explicitly owner-classified roster but NO
    // grants, not even for the owner, and no enabled agent budget.
    bridge
        .state
        .mappings
        .get_mut(j.key.as_ref().unwrap())
        .unwrap()
        .room = Some(policy);
    bridge.save()?;
    write_new(
        &j.options.root.join("room-input.json"),
        &serde_json::to_vec_pretty(&json!({"sessionID":session,"key":j.key,"terminal":terminal,
        "account":j.account,"group":j.options.target_group,"statePath":config.state_path,"socket":crate::porter::socket_path(&config.state_path)}))?,
    )?;
    j.phase("bound")?;
    println!(
        "{}",
        json!({"bound":true,"config":j.options.root.join("config.toml"),"binding":j.options.root.join("room-input.json"),"invocation_grants":"empty","agent_budget":"disabled"})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn journal(root: &Path) -> Journal {
        use bech32::ToBase32;
        let npub =
            |byte| bech32::encode("npub", [byte; 32].to_base32(), bech32::Variant::Bech32).unwrap();
        Journal {
            options: Options {
                root: root.into(),
                owner_npub: npub(1),
                authorised_inviter: npub(2),
                target_group: "aa".into(),
                alias: "builder".into(),
                directory: root.into(),
                herdr_socket: root.join("herdr.sock"),
                opencode_service_file: root.join("service.json"),
                opencode_bin: root.join("opencode"),
            },
            phase: "invitation_required".into(),
            account: Some("03".repeat(32)),
            native_session: None,
            key: None,
        }
    }

    #[test]
    fn target_members_require_exact_authenticated_group_and_owner_not_inviter() {
        let root = tempfile::tempdir().unwrap();
        let j = journal(root.path());
        let config = j.config().unwrap();
        let owner = phone_hex(&j.options.owner_npub).unwrap();
        let inviter = phone_hex(&j.options.authorised_inviter).unwrap();
        let value = json!({"type":"room_context","account_id_hex":config.account_id_hex,"group_id_hex":"aa","members":[owner,config.account_id_hex,inviter]});
        assert!(target_members(&config, "aa", &value).is_ok());
        assert!(target_members(&config, "bb", &value).is_err());
        let mut wrong = value.clone();
        wrong["account_id_hex"] = json!(inviter);
        assert!(target_members(&config, "aa", &wrong).is_err());
        wrong = value.clone();
        wrong["members"] = json!([inviter, config.account_id_hex]);
        assert!(target_members(&config, "aa", &wrong).is_err());
        wrong["members"] = json!([owner, config.account_id_hex, owner]);
        assert!(target_members(&config, "aa", &wrong).is_err());
        // Admin/welcomer status confers no role or grant. Owner supplies all roles.
        let mut policy =
            crate::rooms::RoomPolicy::new(owner.clone(), config.account_id_hex.clone()).unwrap();
        let members = target_members(&config, "aa", &value).unwrap();
        assert!(!policy.authorizes(&owner, true, &members));
        policy
            .set_roster(
                &owner,
                [
                    (owner.clone(), crate::rooms::AccountRole::Human),
                    (
                        config.account_id_hex.clone(),
                        crate::rooms::AccountRole::Agent,
                    ),
                    (inviter.clone(), crate::rooms::AccountRole::Human),
                ]
                .into(),
            )
            .unwrap();
        assert!(!policy.authorizes(&owner, true, &members));
        assert!(!policy.authorizes(&inviter, true, &members));
    }

    #[tokio::test]
    async fn pending_operations_stop_before_connector_or_discovery() {
        // tempfile inherits the process umask for directories. The journal
        // correctly rejects a public root, so give this fixture a private one.
        let directory = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let mut j = journal(&root);
        for phase in [
            "bootstrap_pending",
            "native_session_pending",
            "workspace_pending",
            "agent_start_pending",
            "binding_pending",
            "bound",
        ] {
            j.phase(phase).unwrap();
            let config = j.config().unwrap();
            let path = root.join("config.toml");
            fs::write(&path, j.config_text().unwrap()).unwrap();
            assert!(validate_run(&path, &config).is_err());
            assert!(
                execute(&Action::Bind {
                    root: root.clone(),
                    roles: root.join("missing-roles")
                })
                .await
                .is_err()
            );
            assert!(!root.join("wn.sock").exists());
            assert!(!root.join("state.json").exists());
            assert_eq!(Journal::load(&root).unwrap().phase, phase);
        }
    }

    #[test]
    fn retained_installations_and_symlinks_are_not_adopted() {
        let root = tempfile::tempdir().unwrap();
        assert!(refuse_retained_installation(root.path(), &root.path().join("config")).is_ok());
        let config = root.path().join("config");
        fs::create_dir(&config).unwrap();
        std::os::unix::fs::symlink(
            root.path().join("missing"),
            config.join("herdr-white-noise-bridge"),
        )
        .unwrap();
        assert!(refuse_retained_installation(root.path(), &config).is_err());
        assert!(
            fs::symlink_metadata(config.join("herdr-white-noise-bridge"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn bootstrap_reuse_is_always_refused() {
        assert!(fresh_accounts(&json!({"type":"account_list","accounts":[]})).is_ok());
        assert!(
            fresh_accounts(&json!({"type":"account_list","accounts":[{"account_id_hex":"aa"}]}))
                .is_err()
        );
        assert!(fresh_accounts(&json!({"accounts":[]})).is_err());
    }
    #[test]
    fn retained_pending_journal_cannot_be_treated_as_fresh() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("intent");
        write_new(&path, b"pending").unwrap();
        assert!(write_new(&path, b"replacement").is_err());
        assert_eq!(fs::read(path).unwrap(), b"pending");
    }
}
