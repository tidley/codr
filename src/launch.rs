//! Confirmed, owner-only workspace launches. No shell text or blind RPC retries.
use crate::{
    bridge::{Bridge, now},
    wire::NotSubmitted,
};
use anyhow::{Result, bail, ensure};
use rustix::fs::{Mode, OFlags, mkdirat, open, openat};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs::File,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

const USAGE: &str = "Use /new NAME FOLDER AGENT. Agents: opencode, codex, claude. Then use /new confirm, /new cancel, or /new status.";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    identities: Vec<(u64, u64)>,
    missing: usize,
}

impl Folder {
    pub(crate) fn exists(&self) -> bool {
        self.missing == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Awaiting,
    Preparing,
    CreatingSession,
    CreatingWorkspace,
    StartingAgent,
    Started,
    Cancelled,
    Failed,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub code: String,
    pub label: String,
    pub directory: PathBuf,
    pub root: PathBuf,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::catalogue::Model>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent: Option<crate::herdr::Agent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_workspace: Option<crate::herdr::Workspace>,
    #[serde(default)]
    pub reuse_existing: bool,
    folder: Folder,
    pub expires_at: u64,
    pub phase: Phase,
    pub workspace_id: Option<String>,
    pub pane_id: Option<String>,
    pub terminal_id: Option<String>,
}

impl Request {
    pub(crate) fn confirmation_name(&self) -> String {
        let name = self
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.label.clone());
        // Zero retains its cancellation meaning, even for an unusually named folder.
        if name == "0" || name.starts_with('/') {
            "/new confirm".into()
        } else {
            name
        }
    }
    fn destination_workspace(&self) -> Option<&str> {
        self.target_workspace
            .as_ref()
            .map(|w| w.workspace_id.as_str())
            .or_else(|| self.source_agent.as_ref().map(|a| a.workspace_id.as_str()))
    }
    fn agent_args(&self) -> Vec<String> {
        if let Some(session) = &self.native_session {
            vec!["--session".into(), session.clone()]
        } else if let Some(model) = &self.model
            && !model.id.is_empty()
        {
            vec!["--model".into(), model.id.clone()]
        } else {
            vec![]
        }
    }
    pub(crate) fn preview(&self) -> String {
        format!(
            "{}: {}\nWorkspace destination: {}\nAgent: {}\nModel: {}\nFolder: {}\n{}\nNothing has started. Confirm within ten minutes.\nReply confirm, yes or y (or type: {}).\n\n**0. Cancel**",
            if self.destination_workspace().is_some() {
                "New tab in existing workspace"
            } else {
                "New workspace"
            },
            self.label,
            self.target_workspace
                .as_ref()
                .map(|w| w.display())
                .or_else(|| self.source_agent.as_ref().map(|a| a.workspace_id.clone()))
                .unwrap_or_else(|| format!("Create new workspace {}", self.label)),
            crate::catalogue::agent_name(&self.kind),
            self.model
                .as_ref()
                .map(|model| model.display())
                .unwrap_or_else(|| "Backend default".into()),
            self.directory.display(),
            if self.folder.missing == 0 {
                if self.destination_workspace().is_some() {
                    "Folder already exists. A new tab and separate chat will be created. The old session, history, and queue stay unchanged."
                } else {
                    "Folder already exists. Its contents will be kept."
                }
            } else {
                "Folder will be created after confirmation."
            },
            self.confirmation_name(),
        )
    }

    pub(crate) fn blocks_replacement(&self) -> bool {
        matches!(
            self.phase,
            Phase::Preparing
                | Phase::CreatingSession
                | Phase::CreatingWorkspace
                | Phase::StartingAgent
                | Phase::Uncertain
        ) || (self.phase == Phase::Failed
            && (self.workspace_id.is_some() || self.native_session.is_some()))
    }

    fn status(&self) -> String {
        format!(
            "Launch {}: {:?}. Workspace: {}. Folder: {}. Agent: {}.{}",
            self.code,
            self.phase,
            self.workspace_id.as_deref().unwrap_or("not confirmed"),
            self.directory.display(),
            self.kind,
            if self.blocks_replacement() {
                " Inspect the local workspace before another launch. No automatic retry will run."
            } else {
                ""
            }
        )
    }
}

pub fn is_command(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("/new"))
}

pub fn proposal(text: &str, root: &Path, home: &Path, timestamp: u64) -> Result<Request> {
    let args: Vec<_> = text.split_whitespace().collect();
    ensure!(
        args.len() == 4 && is_command(text) && text.len() <= 2048,
        USAGE
    );
    let label = args[1];
    ensure!(
        !label.is_empty()
            && label.len() <= 48
            && label
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
        "Use a workspace name of 1-48 letters, digits, '-' or '_'."
    );
    let kind = args[3].to_ascii_lowercase();
    ensure!(
        matches!(kind.as_str(), "opencode" | "codex" | "claude"),
        "Choose opencode, codex, or claude. Custom commands and arguments are not allowed."
    );
    let path = if let Some(relative) = args[2].strip_prefix("~/") {
        home.join(relative)
    } else if Path::new(args[2]).is_absolute() {
        PathBuf::from(args[2])
    } else {
        root.join(args[2])
    };
    relative_parts(root, &path)?;
    let folder = inspect(root, &path)?;
    Ok(Request {
        code: format!("n{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
        label: label.into(),
        directory: path,
        root: root.into(),
        kind,
        model: None,
        source_agent: None,
        native_session: None,
        target_workspace: None,
        reuse_existing: false,
        folder,
        expires_at: timestamp.saturating_add(600),
        phase: Phase::Awaiting,
        workspace_id: None,
        pane_id: None,
        terminal_id: None,
    })
}

fn relative_parts<'a>(root: &Path, path: &'a Path) -> Result<Vec<&'a std::ffi::OsStr>> {
    ensure!(
        root.is_absolute()
            && root.parent().is_some()
            && !root
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
        "Launch root must be an absolute project directory."
    );
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("Folder must be below the configured launch root."))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("Parent traversal is not allowed.")
        };
        let name_text = name.to_str().unwrap_or("");
        ensure!(
            !name_text.is_empty()
                && !name_text.starts_with('.')
                && name_text.len() <= 128
                && name_text
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')),
            "Use simple folder names without spaces, hidden components, or shell syntax."
        );
        parts.push(name);
    }
    ensure!(
        !parts.is_empty() && parts.len() <= 8,
        "Choose a project folder below the launch root, at most eight levels deep."
    );
    Ok(parts)
}

pub(crate) fn directory(root: &Path) -> Result<File> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut parent = File::from(open("/", flags, Mode::empty())?);
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                parent = File::from(openat(&parent, name, flags, Mode::empty()).map_err(|_| {
                    anyhow::anyhow!(
                        "The launch root and its ancestors must exist and must not be symlinks."
                    )
                })?);
            }
            _ => bail!("Launch root must be an absolute project directory."),
        }
    }
    Ok(parent)
}

fn identity(file: &File) -> Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

pub fn inspect(root: &Path, path: &Path) -> Result<Folder> {
    let parts = relative_parts(root, path)?;
    let mut parent = directory(root)?;
    let mut identities = vec![identity(&parent)?];
    for (index, name) in parts.iter().enumerate() {
        match openat(
            &parent,
            *name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                parent = File::from(fd);
                identities.push(identity(&parent)?);
            }
            Err(rustix::io::Errno::NOENT) => {
                return Ok(Folder {
                    identities,
                    missing: parts.len() - index,
                });
            }
            Err(_) => bail!(
                "Folder is not an accessible regular directory. Symlinks and files are not allowed."
            ),
        }
    }
    Ok(Folder {
        identities,
        missing: 0,
    })
}

fn prepare_folder(request: &Request) -> Result<()> {
    ensure!(
        inspect(&request.root, &request.directory)? == request.folder,
        "Folder changed after confirmation. No workspace was started."
    );
    let parts = relative_parts(&request.root, &request.directory)?;
    let mut parent = directory(&request.root)?;
    let mut identities = vec![identity(&parent)?];
    ensure!(
        identities[0] == request.folder.identities[0],
        "Launch root changed."
    );
    for (index, name) in parts.iter().enumerate() {
        if index + 1 >= request.folder.identities.len() {
            mkdirat(&parent, *name, Mode::RWXU).map_err(|_| {
                anyhow::anyhow!(
                    "Folder creation stopped. Inspect the folder before another request."
                )
            })?;
            parent.sync_all()?;
        }
        parent = File::from(openat(
            &parent,
            *name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let id = identity(&parent)?;
        if let Some(expected) = request.folder.identities.get(index + 1) {
            ensure!(*expected == id, "Folder identity changed.");
        }
        identities.push(id);
    }
    parent.sync_all()?;
    ensure!(
        inspect(&request.root, &request.directory)?
            == Folder {
                identities,
                missing: 0
            },
        "Folder changed before workspace creation."
    );
    Ok(())
}

pub(crate) fn reply(
    bridge: &mut Bridge,
    key: &str,
    group: &str,
    id: &str,
    text: &str,
) -> Result<()> {
    let mapping = bridge.state.mappings.get_mut(key).unwrap();
    if !mapping.seen_messages.iter().any(|seen| seen == id) {
        mapping.seen_messages.push(id.into());
    }
    bridge
        .state
        .enqueue_manual(group, text, &format!("launch:{group}:{id}"));
    bridge.save()
}

pub async fn handle(
    bridge: &mut Bridge,
    key: &str,
    group: &str,
    id: &str,
    text: &str,
    plain: bool,
) -> Result<()> {
    if !plain {
        return reply(
            bridge,
            key,
            group,
            id,
            "Send /new as complete standalone text, without attachments or a voice transcript.",
        );
    }
    let args: Vec<_> = text.split_whitespace().collect();
    if args.len() == 2 && args[1].eq_ignore_ascii_case("status") {
        let status = bridge.state.mappings[key].launch.as_ref().map_or_else(
            || "No workspace launch request in this chat.".into(),
            Request::status,
        );
        return reply(bridge, key, group, id, &status);
    }
    if args.len() == 3 && matches!(args[1].to_ascii_lowercase().as_str(), "confirm" | "cancel") {
        let Some(request) = bridge.state.mappings[key]
            .launch
            .clone()
            .filter(|request| request.code == args[2])
        else {
            return reply(
                bridge,
                key,
                group,
                id,
                "No matching launch code in this chat. Use the latest /new confirmation card.",
            );
        };
        if request.phase != Phase::Awaiting {
            return reply(
                bridge,
                key,
                group,
                id,
                &format!("{} No launch was repeated.", request.status()),
            );
        }
        if args[1].eq_ignore_ascii_case("cancel") {
            bridge
                .state
                .mappings
                .get_mut(key)
                .unwrap()
                .launch
                .as_mut()
                .unwrap()
                .phase = Phase::Cancelled;
            return reply(
                bridge,
                key,
                group,
                id,
                "Workspace launch cancelled. No folder, workspace, or agent was created.",
            );
        }
        if now() >= request.expires_at || bridge.config.launch_root != request.root {
            bridge
                .state
                .mappings
                .get_mut(key)
                .unwrap()
                .launch
                .as_mut()
                .unwrap()
                .phase = Phase::Cancelled;
            return reply(
                bridge,
                key,
                group,
                id,
                "This confirmation expired or its launch root changed. Send /new again. Nothing was started.",
            );
        }
        if let Some(target) = &request.target_workspace {
            match crate::herdr::workspace(&bridge.herdr, &target.workspace_id).await {
                Ok(current) if current != *target => {
                    let mut changed = request;
                    changed.target_workspace = Some(current);
                    changed.code = format!("n{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
                    changed.expires_at = now().saturating_add(600);
                    let preview = format!(
                        "The workspace label changed. Confirm the updated destination.\n{}",
                        changed.preview()
                    );
                    bridge.state.mappings.get_mut(key).unwrap().launch = Some(changed);
                    return reply(bridge, key, group, id, &preview);
                }
                Err(_) => {
                    return reply(
                        bridge,
                        key,
                        group,
                        id,
                        "The selected workspace is unavailable. Nothing was created. Start /new again to choose a destination. No other workspace was used.",
                    );
                }
                _ => {}
            }
        }
        let current = inspect(&request.root, &request.directory);
        match current {
            Ok(folder) if folder != request.folder => {
                let mut changed = request;
                changed.folder = folder;
                changed.code = format!("n{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
                changed.expires_at = now().saturating_add(600);
                let preview = format!(
                    "The folder changed. Confirm the updated request.\n{}",
                    changed.preview()
                );
                bridge.state.mappings.get_mut(key).unwrap().launch = Some(changed);
                return reply(bridge, key, group, id, &preview);
            }
            Err(_) => {
                return reply(
                    bridge,
                    key,
                    group,
                    id,
                    "Folder validation failed. Nothing was started. Check the folder and send /new again.",
                );
            }
            _ => {}
        }
        if request.reuse_existing
            && crate::launch_flow::offer_existing(bridge, key, group, id, &request.directory)
                .await?
        {
            return Ok(());
        }
        if let Some(model) = &request.model {
            if !crate::catalogue::installed(&bridge.config).contains(&request.kind) {
                return reply(
                    bridge,
                    key,
                    group,
                    id,
                    "The selected agent is no longer installed. Start the configuration again.",
                );
            }
            let available =
                crate::catalogue::models(&bridge.config, &request.kind, &request.directory, true)
                    .await;
            if !available.is_ok_and(|models| {
                models
                    .iter()
                    .any(|candidate| candidate.same_reference(model))
            }) {
                return reply(
                    bridge,
                    key,
                    group,
                    id,
                    "The selected model could not be revalidated. Start the model selection again. Nothing was started.",
                );
            }
        }
        if let Some(source) = &request.source_agent {
            let current = crate::herdr::get(&bridge.herdr, source).await;
            if !current.is_ok_and(|current| {
                current.cwd == source.cwd
                    && current.workspace_id == source.workspace_id
                    && current.key() == key
            }) {
                return reply(
                    bridge,
                    key,
                    group,
                    id,
                    "The source session or project changed. Open /agent again. No new session was started.",
                );
            }
        }
        let before = bridge.state.clone();
        bridge
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .launch
            .as_mut()
            .unwrap()
            .phase = Phase::Preparing;
        bridge
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .seen_messages
            .push(id.into());
        if let Err(error) = bridge.save() {
            bridge.state = before;
            return Err(error);
        }
        return start(bridge, key, group, id, request).await;
    }
    if bridge.state.mappings[key]
        .launch
        .as_ref()
        .is_some_and(Request::blocks_replacement)
    {
        return reply(
            bridge,
            key,
            group,
            id,
            "A previous launch needs local inspection. Use /new status. No new request was created.",
        );
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    match proposal(text, &bridge.config.launch_root, &home, now()) {
        Ok(request) => {
            let before = bridge.state.clone();
            let preview = request.preview();
            bridge.state.mappings.get_mut(key).unwrap().launch = Some(request);
            bridge.state.mappings.get_mut(key).unwrap().launch_draft = None;
            if let Err(error) = reply(bridge, key, group, id, &preview) {
                bridge.state = before;
                return Err(error);
            }
            Ok(())
        }
        Err(error) => reply(bridge, key, group, id, &error.to_string()),
    }
}

async fn start(
    bridge: &mut Bridge,
    key: &str,
    group: &str,
    id: &str,
    request: Request,
) -> Result<()> {
    let folder_request = request.clone();
    if !matches!(
        tokio::task::spawn_blocking(move || prepare_folder(&folder_request)).await,
        Ok(Ok(()))
    ) {
        bridge
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .launch
            .as_mut()
            .unwrap()
            .phase = Phase::Failed;
        return reply(
            bridge,
            key,
            group,
            id,
            "Folder preparation failed. No workspace or agent was started. Keep any created folders and inspect them before another request.",
        );
    }
    if request.kind == "opencode"
        && let Some(model) = &request.model
    {
        let session = format!("ses_codr_{}", uuid::Uuid::new_v4().simple());
        let stored = bridge
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .launch
            .as_mut()
            .unwrap();
        stored.native_session = Some(session.clone());
        stored.phase = Phase::CreatingSession;
        bridge.save()?;
        let mut body =
            json!({"id":session,"title":request.label,"location":{"directory":request.directory}});
        if !model.id.is_empty() {
            body["model"] = model.reference();
        }
        let response =
            crate::catalogue::api(&bridge.config, "post", "/api/session", Some(&body)).await;
        if !response.is_ok_and(|r| {
            r["data"]["id"] == session
                && r["data"]["location"]["directory"]
                    == request.directory.to_string_lossy().as_ref()
                && (model.id.is_empty() || model.matches_reference(&r["data"]["model"]))
        }) {
            bridge
                .state
                .mappings
                .get_mut(key)
                .unwrap()
                .launch
                .as_mut()
                .unwrap()
                .phase = Phase::Uncertain;
            return reply(
                bridge,
                key,
                group,
                id,
                "OpenCode session creation was not confirmed. The folder was kept. Inspect the recorded native session locally. No workspace, agent start, or automatic retry followed.",
            );
        }
    }
    bridge
        .state
        .mappings
        .get_mut(key)
        .unwrap()
        .launch
        .as_mut()
        .unwrap()
        .phase = Phase::CreatingWorkspace;
    bridge.save()?;
    let (method, params) = if let Some(workspace) = request.destination_workspace() {
        let label = if let Some(source) = &request.source_agent {
            bridge.state.mappings[key]
                .agent_record
                .as_ref()
                .filter(|record| record.tab_id == source.tab_id)
                .and_then(|record| record.tab_label.as_deref())
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(&request.kind)
        } else {
            &request.label
        };
        (
            "tab.create",
            json!({"cwd":request.directory,"label":label,"workspace_id":workspace,"focus":false}),
        )
    } else {
        (
            "workspace.create",
            json!({"cwd":request.directory,"label":request.label,"focus":false}),
        )
    };
    let created = bridge.herdr.herdr(method, params).await;
    let response = match created {
        Ok(response) => response,
        Err(error) => {
            bridge
                .state
                .mappings
                .get_mut(key)
                .unwrap()
                .launch
                .as_mut()
                .unwrap()
                .phase = if error.is::<NotSubmitted>() {
                Phase::Failed
            } else {
                Phase::Uncertain
            };
            return reply(
                bridge,
                key,
                group,
                id,
                "Workspace creation was not confirmed. The folder was kept. Use /new status and inspect herdr before another launch. No automatic retry will run.",
            );
        }
    };
    let result = &response["result"];
    let workspace = if request.destination_workspace().is_some() {
        result["tab"]["workspace_id"].as_str()
    } else {
        result["workspace"]["workspace_id"].as_str()
    };
    let identifiers = (
        workspace,
        result["root_pane"]["pane_id"].as_str(),
        result["root_pane"]["terminal_id"].as_str(),
    );
    let (Some(workspace), Some(pane), Some(terminal)) = identifiers else {
        bridge
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .launch
            .as_mut()
            .unwrap()
            .phase = Phase::Uncertain;
        return reply(
            bridge,
            key,
            group,
            id,
            "Workspace response was incomplete. Inspect herdr. No agent was started or retried.",
        );
    };
    if result["type"]
        != if request.destination_workspace().is_some() {
            "tab_created"
        } else {
            "workspace_created"
        }
        || request
            .destination_workspace()
            .is_some_and(|selected| selected != workspace)
        || [workspace, pane, terminal].iter().any(|value| {
            value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
        })
    {
        bridge
            .state
            .mappings
            .get_mut(key)
            .unwrap()
            .launch
            .as_mut()
            .unwrap()
            .phase = Phase::Uncertain;
        return reply(
            bridge,
            key,
            group,
            id,
            "Workspace response was invalid. Inspect herdr. No agent was started or retried.",
        );
    }
    let stored = bridge
        .state
        .mappings
        .get_mut(key)
        .unwrap()
        .launch
        .as_mut()
        .unwrap();
    stored.workspace_id = Some(workspace.into());
    stored.pane_id = Some(pane.into());
    stored.terminal_id = Some(terminal.into());
    stored.phase = Phase::StartingAgent;
    if request.destination_workspace().is_some() {
        bridge
            .state
            .pending_chat_agents
            .insert(terminal.into(), request.kind.clone());
    }
    bridge.save()?;
    let name = format!("codr-{}", &request.code[1..]);
    let args = bridge.state.mappings[key]
        .launch
        .as_ref()
        .unwrap()
        .agent_args();
    // A fresh pane reports agent_pane_busy until its shell owns the foreground
    // (shell startup files may still be running). herdr returns that before
    // writing any input, so only that refusal is retried; every other result
    // keeps the non-retry rule.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let started = loop {
        let started = bridge
            .herdr
            .herdr(
                "agent.start",
                json!({"name":name,"kind":request.kind,"pane_id":pane,"args":&args}),
            )
            .await;
        let busy = started
            .as_ref()
            .is_err_and(|error| error.to_string() == "Remote request failed: agent_pane_busy");
        if !busy || tokio::time::Instant::now() >= deadline {
            break started;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    let confirmed = started.as_ref().is_ok_and(|response| {
        let result = &response["result"];
        result["type"] == "agent_started"
            && result["agent"]["name"] == name
            && result["agent"]["pane_id"] == pane
            && result["agent"]["workspace_id"] == workspace
            && result["agent"]["terminal_id"] == terminal
    });
    bridge
        .state
        .mappings
        .get_mut(key)
        .unwrap()
        .launch
        .as_mut()
        .unwrap()
        .phase = if confirmed {
        Phase::Started
    } else {
        Phase::Uncertain
    };
    if confirmed {
        bridge.save()?;
        // Startup requires a unique name. Clear only that temporary label once
        // herdr permits renaming. Its rename API accepts agent names, not terminal
        // IDs; the unique startup name also avoids targeting a reused pane.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        let renamed = loop {
            let renamed = bridge
                .herdr
                .herdr("agent.rename", json!({"target":name,"name":null}))
                .await;
            let pending = renamed.as_ref().is_err_and(|error| {
                error.to_string() == "Remote request failed: agent_launch_pending"
            });
            if !pending || tokio::time::Instant::now() >= deadline {
                break renamed;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        };
        let name_cleared = renamed.as_ref().is_ok_and(|response| {
            let result = &response["result"];
            result["type"] == "agent_info"
                && result["agent"]["name"].is_null()
                && result["agent"]["pane_id"] == pane
                && result["agent"]["workspace_id"] == workspace
                && result["agent"]["terminal_id"] == terminal
        });
        reply(
            bridge,
            key,
            group,
            id,
            &format!(
                "Started {} in workspace {}. {} A White Noise invitation follows once herdr reports its native session. No initial prompt was sent.{}",
                request.kind,
                request
                    .target_workspace
                    .as_ref()
                    .map(|w| w.display())
                    .unwrap_or_else(|| format!("{} ({workspace})", request.label)),
                if request.folder.missing == 0 {
                    "The existing folder and its contents were kept."
                } else {
                    "The confirmed folder was created."
                },
                if name_cleared {
                    ""
                } else {
                    " The agent started, but its temporary display name could not be cleared. No agent restart was attempted."
                },
            ),
        )
    } else {
        reply(
            bridge,
            key,
            group,
            id,
            &format!(
                "Agent startup was not confirmed in workspace {workspace}. Keep the workspace and inspect it locally. No automatic retry will run."
            ),
        )
    }
}

#[cfg(test)]
mod model_argument_tests {
    #[test]
    fn backend_launches_only_use_structured_model_or_session_arguments() {
        let directory = tempfile::tempdir().unwrap();
        // macOS temp directories sit behind the /var symlink; launch roots must be real paths.
        let root = directory.path().canonicalize().unwrap();
        for kind in ["codex", "claude"] {
            let mut request =
                super::proposal(&format!("/new project project {kind}"), &root, &root, 0).unwrap();
            assert!(request.agent_args().is_empty());
            request.model = Some(crate::catalogue::Model {
                id: "chosen-model".into(),
                label: "Chosen".into(),
                provider: None,
                variant: None,
                free: false,
            });
            assert_eq!(request.agent_args(), vec!["--model", "chosen-model"]);
        }
        let upper = super::proposal("/new project project OPENCODE", &root, &root, 0).unwrap();
        assert_eq!(upper.kind, "opencode");
        let mut request =
            super::proposal("/new project project opencode", &root, &root, 0).unwrap();
        request.native_session = Some("ses_chosen".into());
        assert_eq!(request.agent_args(), vec!["--session", "ses_chosen"]);
    }
}
