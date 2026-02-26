use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub base_url: String,

    // Cloudflare Access
    pub cf_aud: String,
    pub cf_team_domain: String,
    pub cf_jwks_cache_ttl: Duration,

    // Dev mode
    pub dev_mode: bool,
    pub dev_user: String,

    // Admin users (email prefixes)
    pub admin_users: HashSet<String>,

    // Soju
    pub soju_addr: String,
    pub soju_socket: PathBuf,
    pub soju_config: String,

    // IRC upstream
    pub irc_server: String,
    pub irc_port: u16,

    // ttyd port range
    pub ttyd_base_port: u16,

    // Filesystem
    pub data_dir: PathBuf,
    pub sessions_dir: PathBuf,
    pub public_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        // Load .env if present (dev convenience)
        let _ = dotenvy::dotenv();

        let data_dir = if PathBuf::from("/data").exists() {
            PathBuf::from("/data")
        } else {
            let d = PathBuf::from("./data");
            std::fs::create_dir_all(d.join("sessions"))
                .context("failed to create ./data/sessions")?;
            d
        };

        let admin_users: HashSet<String> = std::env::var("ADMIN_USERS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        let cf_jwks_cache_ttl = std::env::var("CF_JWKS_CACHE_TTL")
            .ok()
            .and_then(|s| humantime::parse_duration(&s).ok())
            .unwrap_or(Duration::from_secs(6 * 3600));

        Ok(Config {
            port: env_var("PORT", "3001").parse().context("invalid PORT")?,
            base_url: env_var("BASE_URL", "http://localhost:3001"),
            cf_aud: env_var("CF_AUD", ""),
            cf_team_domain: env_var("CF_TEAM_DOMAIN", ""),
            cf_jwks_cache_ttl,
            dev_mode: env_var("DEV_MODE", "false") == "true",
            dev_user: env_var("DEV_USER", "devuser"),
            admin_users,
            soju_addr: env_var("SOJU_ADDR", "soju:6667"),
            soju_socket: PathBuf::from(env_var("SOJU_SOCKET", "/soju/soju.sock")),
            soju_config: env_var("SOJU_CONFIG", "/etc/soju/config"),
            irc_server: env_var("IRC_SERVER", "irc.libera.chat"),
            irc_port: env_var("IRC_PORT", "6697").parse().context("invalid IRC_PORT")?,
            ttyd_base_port: env_var("TTYD_BASE_PORT", "7100").parse().context("invalid TTYD_BASE_PORT")?,
            sessions_dir: data_dir.join("sessions"),
            public_dir: PathBuf::from(env_var("PUBLIC_DIR", "./public")),
            data_dir,
        })
    }
}

fn env_var(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}