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
    irc_addr: String,
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
    ///
    /// The password is stored in <user_dir>/soju_password so that if soju's
    /// database is wiped (e.g. the soju stack is redeployed) but the app
    /// volume is intact, we can re-create the soju user with the same password
    /// the irssi config already has. Without this the passwords diverge and
    /// SASL auth breaks.
    pub async fn ensure_user(&self, username: &str) -> Result<()> {
        if self.provisioned.contains_key(username) {
            return Ok(());
        }

        let user_dir = self.sessions_dir.join(username);
        let config_path = user_dir.join("config");
        let password_path = user_dir.join("soju_password");

        let password = if password_path.exists() {
            // Password file exists — read it and (re-)provision soju in case
            // its DB was wiped. Sojuctl calls are idempotent so this is safe.
            tokio::fs::read_to_string(&password_path)
                .await
                .context("failed to read soju_password")?
                .trim()
                .to_string()
        } else {
            // First time — generate a fresh password and write it out.
            tokio::fs::create_dir_all(&user_dir)
                .await
                .context("failed to create user dir")?;
            let pw = random_password();
            tokio::fs::write(&password_path, &pw)
                .await
                .context("failed to write soju_password")?;
            pw
        };

        // (Re-)create soju user with the stored password
        let result = self
            .sojuctl(&[
                "user", "create",
                "-username", username,
                "-password", &password,
            ])
            .await;

        if let Err(e) = result {
            let msg = e.to_string();
            if msg.contains("already exists") {
                // User exists in soju DB — update the password to match our file
                // in case it drifted (e.g. manual sojuctl intervention).
                self.sojuctl(&[
                    "user", "update",
                    "-username", username,
                    "-password", &password,
                ])
                .await
                .context("soju user update failed")?;
            } else {
                return Err(e).context("soju user create failed");
            }
        }

        // Add upstream IRC network — idempotent, ignore "already exists"
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

        // Write irssi config only if it doesn't already exist
        if !config_path.exists() {
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
        }

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