use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use rand::Rng;
use tokio::process::Command;
use tracing::info;

pub struct Manager {
    soju_config: String,
    sessions_dir: PathBuf,
    soju_addr: String,
    irc_server: String,
    irc_port: u16,
    /// Tracks users provisioned in this process run (avoids redundant sojuctl calls)
    provisioned: Arc<DashMap<String, ()>>,
}

impl Manager {
    pub fn new(
        soju_config: String,
        sessions_dir: PathBuf,
        soju_addr: String,
        irc_server: String,
        irc_port: u16,
    ) -> Arc<Self> {
        Arc::new(Self {
            soju_config,
            sessions_dir,
            soju_addr,
            irc_server,
            irc_port,
            provisioned: Arc::new(DashMap::new()),
        })
    }

    /// Ensure a soju account and irssi config exist for this user.
    /// Idempotent — safe to call on every login.
    pub async fn ensure_user(&self, username: &str) -> Result<()> {
        if self.provisioned.contains_key(username) {
            return Ok(());
        }

        let user_dir = self.sessions_dir.join(username);
        let config_path = user_dir.join("irssi.conf");

        // Already provisioned from a previous run
        if config_path.exists() {
            self.provisioned.insert(username.to_string(), ());
            return Ok(());
        }

        let password = random_password();

        // Create soju user
        let result = self
            .sojuctl(&[
                "user", "create",
                "-username", username,
                "-password", &password,
            ])
            .await;

        if let Err(e) = result {
            if !e.to_string().contains("already exists") {
                return Err(e).context("soju user create failed");
            }
        }

        // Add upstream IRC network
        let network_name = self.irc_server.replace('.', "-");
        let irc_addr = format!("ircs://{}:{}", self.irc_server, self.irc_port);

        let result = self
            .sojuctl(&[
                "network", "create",
                "-user", username,
                "-name", &network_name,
                "-addr", &irc_addr,
                "-nick", username,
            ])
            .await;

        if let Err(e) = result {
            if !e.to_string().contains("already exists") {
                return Err(e).context("soju network create failed");
            }
        }

        // Write irssi config
        tokio::fs::create_dir_all(&user_dir)
            .await
            .context("failed to create user dir")?;

        let (soju_host, soju_port) = split_addr(&self.soju_addr);
        let irssi_conf = format!(
            r#"servers = ({{
  address = "{soju_host}";
  port = {soju_port};
  use_ssl = no;
  password = "{username}/{network_name}:{password}";
  autoconnect = yes;
}});

settings = {{
  core = {{
    real_name = "{username}";
    user_name = "{username}";
    nick = "{username}";
  }};
  "fe-text" = {{ term_charset = "UTF-8"; }};
  "fe-common/core" = {{ term_charset = "UTF-8"; }};
}};
"#,
        );

        tokio::fs::write(&config_path, irssi_conf)
            .await
            .context("failed to write irssi config")?;

        info!("Provisioned soju user: {}", username);
        self.provisioned.insert(username.to_string(), ());
        Ok(())
    }

    pub fn user_dir(&self, username: &str) -> PathBuf {
        self.sessions_dir.join(username)
    }

    pub async fn delete_user(&self, username: &str) -> Result<()> {
        self.provisioned.remove(username);

        let _ = self
            .sojuctl(&["user", "delete", "-username", username])
            .await;

        let user_dir = self.sessions_dir.join(username);
        if user_dir.exists() {
            tokio::fs::remove_dir_all(&user_dir)
                .await
                .context("failed to remove user dir")?;
        }
        Ok(())
    }

    async fn sojuctl(&self, args: &[&str]) -> Result<()> {
    let output = Command::new("podman")
        .args(["exec", "soju", "sojuctl", "-config", &self.soju_config])
        .args(args)
        .output()
        .await
        .context("failed to run sojuctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("sojuctl error: {}", stderr.trim()));
    }
    Ok(())
}
}

fn random_password() -> String {
    let bytes: Vec<u8> = (0..16).map(|_| rand::thread_rng().gen()).collect();
    hex::encode(bytes)
}

fn split_addr(addr: &str) -> (&str, &str) {
    match addr.rsplit_once(':') {
        Some((host, port)) => (host, port),
        None => (addr, "6667"),
    }
}