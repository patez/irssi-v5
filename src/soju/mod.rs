use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use rand::Rng;
use tokio::process::Command;
use tracing::info;

pub struct Manager {
    socket_path: PathBuf,
    sessions_dir: PathBuf,
    soju_addr: String,
    // Full soju IRC address, e.g:
    //   irc+insecure://irc.swepipe.net        plain text, default port
    //   irc+insecure://irc.swepipe.net:6667   plain text, explicit port
    //   ircs://irc.libera.chat                TLS, default port
    //   ircs://irc.libera.chat:6697           TLS, explicit port
    irc_addr: String,
    // Short name for the network, used in soju and irssi
    // e.g. "swepipe", "libera", "ircnet"
    irc_network_name: String,
    /// Tracks users provisioned in this process run (avoids redundant sojuctl calls)
    provisioned: Arc<DashMap<String, ()>>,
}

impl Manager {
    pub fn new(
        socket_path: PathBuf,
        sessions_dir: PathBuf,
        soju_addr: String,
        irc_addr: String,
        irc_network_name: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            socket_path,
            sessions_dir,
            soju_addr,
            irc_addr,
            irc_network_name,
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
        let config_path = user_dir.join("config");

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
        let result = self
            .sojuctl(&[
                "user", "run",
                username,
                "network", "create",
                "-name", &self.irc_network_name,
                "-addr", &self.irc_addr,
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
r#"chatnets = {{
  {network_name} = {{
    type = "IRC";
    sasl_mechanism = "PLAIN";
    sasl_username = "{username}/{network_name}";
    sasl_password = "{password}";
  }};
}};

servers = ({{
  address = "{soju_host}";
  port = {soju_port};
  use_ssl = no;
  chatnet = "{network_name}";
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
            network_name = self.irc_network_name,
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
            .sojuctl(&["user", "delete", username])
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
        let output = Command::new("sojuctl")
            .arg("-config")
            .arg("/etc/soju/config")
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