//! Core has terminals, not TUI workspaces/panes. Only explicit Codr launches
//! with an exact OpenCode argv/session proof enter discovery; names are decoration.
use super::*;
use std::collections::BTreeMap;

const REQUIRED: &[&str] = &[
    "hello",
    "server.status",
    "terminal.run",
    "terminal.list",
    "terminal.get",
];
const LEGACY: &[&str] = &[
    "session.snapshot",
    "workspace.create",
    "agent.start",
    "agent.get",
    "agent.prompt",
    "agent.send_keys",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    Tui,
    Core { id: String },
}

fn methods(value: &Value, discovery: bool) -> Result<HashSet<String>> {
    let rows = value
        .as_array()
        .filter(|r| !r.is_empty() && r.len() <= 1024)
        .ok_or_else(|| anyhow::anyhow!("Missing or malformed Herdr capabilities"))?;
    let mut names = HashSet::new();
    for row in rows {
        let name = if discovery { &row["name"] } else { row };
        let name = name
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .ok_or_else(|| anyhow::anyhow!("Malformed Herdr method"))?;
        ensure!(names.insert(name.into()), "Duplicate Herdr capability");
    }
    Ok(names)
}

pub async fn preflight(socket: &Socket) -> Result<Backend> {
    let response = match socket
        .exchange(
            json!({"jsonrpc":"2.0","method":"rpc.discover","params":{}}),
            false,
        )
        .await
    {
        Ok(response) => response,
        // Old TUI servers can reject the Core envelope itself. Only these two
        // explicit protocol errors permit a read-only old snapshot proof.
        // Timeouts, malformed replies and arbitrary errors never select a backend.
        Err(error)
            if error.is::<crate::wire::MethodNotFound>()
                || error.is::<crate::wire::InvalidRequest>() =>
        {
            super::legacy_snapshot(socket).await?;
            return Ok(Backend::Tui);
        }
        Err(error) => return Err(error),
    };
    let names = methods(&response["result"]["methods"], true)?;
    let core = names.contains("terminal.run");
    let legacy = LEGACY.iter().any(|m| names.contains(*m));
    ensure!(!(core && legacy), "Mixed Core/TUI endpoint refused");
    if !core {
        ensure!(
            LEGACY.iter().all(|m| names.contains(*m)),
            "Unsupported Herdr endpoint"
        );
        super::legacy_snapshot(socket).await?;
        return Ok(Backend::Tui);
    }
    ensure!(
        response["jsonrpc"] == "2.0" && REQUIRED.iter().all(|m| names.contains(*m)),
        "Unsupported Core capabilities"
    );
    ensure!(
        response["result"]["openrpc"] == "1.3.2"
            && response["result"]["info"]["version"] == "0.1.22",
        "Malformed or unsupported Core discovery document"
    );
    let hello = socket
        .core("hello", json!({"generation":1,"framed":false}))
        .await?;
    let advertised = methods(&hello["methods"], false)?;
    ensure!(
        hello["generation"] == 1
            && hello["version"] == "0.1.22"
            && hello["framed"] == false
            && hello["features"]
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string))
            && REQUIRED.iter().all(|m| advertised.contains(*m))
            && !LEGACY.iter().any(|m| advertised.contains(*m)),
        "Unsupported or malformed Core handshake"
    );
    let id = hello["core"]["id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or_else(|| anyhow::anyhow!("Core identity missing"))?;
    ensure!(hello["core"]["name"].is_string(), "Malformed Core identity");
    Ok(Backend::Core { id: id.into() })
}

#[derive(Deserialize)]
struct Status {
    state: String,
}

#[derive(Deserialize)]
pub(super) struct Terminal {
    address: String,
    argv: Vec<String>,
    cwd: String,
    state: String,
    created_ms: u64,
    labels: BTreeMap<String, String>,
    screen: String,
    #[serde(rename = "cols")]
    _cols: u16,
    #[serde(rename = "rows")]
    _rows: u16,
    #[serde(default)]
    status: Option<Status>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    reported: bool,
}

impl Terminal {
    fn agent(self, core: &str) -> Result<Option<Agent>> {
        ensure!(
            !self.address.is_empty()
                && self.address.len() <= 128
                && matches!(self.state.as_str(), "live" | "finished")
                && matches!(self.screen.as_str(), "exact" | "inexact"),
            "Malformed Core terminal"
        );
        let Some(session) = self.labels.get("codr.opencode-session") else {
            return Ok(None);
        };
        ensure!(
            session.starts_with("ses_")
                && session.len() <= 128
                && self.argv.len() == 3
                && self.argv[1] == "--session"
                && self.argv[2] == *session
                && std::path::Path::new(&self.argv[0]).is_absolute()
                && std::path::Path::new(&self.cwd).is_absolute(),
            "Core OpenCode session proof differs"
        );
        if self.state != "live" || self.reason.is_some() {
            return Ok(None);
        }
        // Neither live nor a PID implies idle. Core stores only program-reported
        // status. Unknown stays unknown and cannot satisfy room readiness.
        let status = if self.reported {
            self.status
                .map(|s| s.state)
                .unwrap_or_else(|| "unknown".into())
        } else {
            "unknown".into()
        };
        let source = format!(
            "hc:{}",
            serde_json::to_string(&(
                core,
                &self.address,
                self.created_ms,
                &self.argv[0],
                &self.cwd
            ))?
        );
        Ok(Some(Agent {
            pane_id: self.address.clone(),
            terminal_id: source.clone(),
            tab_id: self.address,
            tab_label: None,
            workspace_id: core.into(),
            workspace_label: None,
            agent: "opencode".into(),
            agent_status: status,
            agent_session: Some(Session {
                source,
                agent: "opencode".into(),
                kind: "id".into(),
                value: session.clone(),
            }),
            state_change_seq: 0,
            cwd: self.cwd,
            foreground_cwd: None,
            terminal_title_stripped: None,
        }))
    }
}

pub(super) async fn snapshot(socket: &Socket, id: &str) -> Result<Vec<Agent>> {
    let response = socket.core("terminal.list", json!({})).await?;
    ensure!(
        response["hidden_finished"].is_u64() && response["machines"].is_array(),
        "Malformed Core terminal list"
    );
    let rows = response["terminals"]
        .as_array()
        .filter(|r| r.len() <= 10000)
        .ok_or_else(|| anyhow::anyhow!("Malformed Core terminal list"))?;
    let mut seen = HashSet::new();
    let mut sessions = HashSet::new();
    let mut agents = Vec::new();
    for row in rows {
        let terminal: Terminal = serde_json::from_value(row.clone())?;
        ensure!(
            seen.insert(terminal.address.clone()),
            "Duplicate Core terminal address"
        );
        // Remote records need the remote core's identity, which this contract
        // does not provide. Never collapse machine/t7 into the local core namespace.
        if terminal.address.contains('/') {
            continue;
        }
        if let Some(agent) = terminal.agent(id)? {
            ensure!(
                sessions.insert(agent.agent_session.as_ref().unwrap().value.clone()),
                "Duplicate Core native session"
            );
            agents.push(agent);
        }
    }
    Ok(agents)
}

pub(super) async fn current(socket: &Socket, agent: &Agent) -> Result<Agent> {
    let Backend::Core { id } = preflight(socket).await? else {
        anyhow::bail!("Core endpoint changed")
    };
    let value = socket
        .core("terminal.get", json!({"terminal":agent.pane_id}))
        .await?;
    serde_json::from_value::<Terminal>(value)?
        .agent(&id)?
        .ok_or_else(|| anyhow::anyhow!("Core terminal no longer has a live native session"))
}

pub(crate) async fn launch(
    socket: &Socket,
    id: &str,
    binary: &std::path::Path,
    directory: &std::path::Path,
    session: &str,
) -> Result<Agent> {
    // No shell, templates, parent notifications, keep/restart, task or input.
    // This effect is attempted once; even spawn_pending must retain its journal.
    let response = socket.core("terminal.run", json!({"argv":[binary,"--session",session],"cwd":directory,
        "labels":{"codr.opencode-session":session},"no_parent":true,"no_notify":true,"keep":false,"wait":false})).await?;
    let terminal: Terminal = serde_json::from_value(response)?;
    ensure!(
        terminal.reason.is_none(),
        "Core spawn unconfirmed; preserve intent, never retry terminal.run"
    );
    ensure!(
        terminal.argv
            == [
                binary.to_string_lossy().into_owned(),
                "--session".into(),
                session.into()
            ],
        "Core launch argv differs"
    );
    let agent = terminal
        .agent(id)?
        .ok_or_else(|| anyhow::anyhow!("Core launch unconfirmed; do not retry"))?;
    ensure!(
        agent.agent_session.as_ref().unwrap().value == session
            && agent.cwd == directory.to_string_lossy(),
        "Core launch identity/location differs"
    );
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io::AsyncWriteExt, net::UnixListener};

    #[tokio::test]
    async fn launch_wire_shape_and_uncertain_spawn_are_one_shot() {
        for reason in [None, Some("spawn_pending")] {
            let root = tempfile::tempdir().unwrap();
            let socket = Socket {
                path: root.path().join("core.sock"),
            };
            let listener = UnixListener::bind(&socket.path).unwrap();
            let job = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(stream);
                let request = crate::wire::read_line(&mut reader).await.unwrap();
                assert_eq!(request["jsonrpc"], "2.0");
                assert_eq!(request["method"], "terminal.run");
                assert_eq!(
                    request["params"],
                    json!({"argv":["/fixture/opencode","--session","ses_fresh"],
                    "cwd":"/fixture/project","labels":{"codr.opencode-session":"ses_fresh"},
                    "no_parent":true,"no_notify":true,"keep":false,"wait":false})
                );
                let result = json!({"address":"t1","argv":request["params"]["argv"],"cwd":"/fixture/project",
                    "labels":request["params"]["labels"],"state":"live","created_ms":1234,
                    "screen":"exact","cols":200,"rows":120,"reason":reason});
                reader
                    .get_mut()
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                // A duplicate creation would connect here; the launch call must
                // return without issuing another request even for spawn_pending.
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                        .await
                        .is_err()
                );
            });
            let result = launch(
                &socket,
                "core-one",
                std::path::Path::new("/fixture/opencode"),
                std::path::Path::new("/fixture/project"),
                "ses_fresh",
            )
            .await;
            if reason.is_some() {
                assert!(result.is_err());
            } else {
                let agent = result.unwrap();
                assert!(!agent.settled(), "No program report is not idle");
                assert_eq!(agent.pane_id, "t1");
            }
            job.await.unwrap();
        }
    }
}
