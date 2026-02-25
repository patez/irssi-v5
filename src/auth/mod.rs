use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

static USERNAME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^a-z0-9-]").unwrap());

/// Verified identity derived from a Cloudflare Access JWT.
#[derive(Debug, Clone)]
pub struct User {
    pub username: String, // sanitized email prefix
    pub email: String,
    pub is_admin: bool,
}

/// CF JWT claims we care about
#[derive(Debug, Deserialize)]
struct CfClaims {
    email: String,
    aud: Vec<String>,
    iss: String,
    exp: u64,
}

/// A single JWK key from Cloudflare's JWKS endpoint
#[derive(Debug, Deserialize, Clone)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<Jwk>,
}

struct JwksCache {
    keys: Vec<Jwk>,
    fetched_at: Instant,
}

pub struct Validator {
    aud: String,
    issuer: String,
    jwks_url: String,
    cache_ttl: Duration,
    admin_users: HashSet<String>,
    cache: RwLock<Option<JwksCache>>,
}

impl Validator {
    pub fn new(
        team_domain: &str,
        aud: &str,
        cache_ttl: Duration,
        admin_users: HashSet<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            aud: aud.to_string(),
            issuer: format!("https://{}", team_domain),
            jwks_url: format!("https://{}/cdn-cgi/access/certs", team_domain),
            cache_ttl,
            admin_users,
            cache: RwLock::new(None),
        })
    }

    /// Validate a CF Access JWT token string and return the verified User.
    pub async fn validate(&self, token: &str) -> Result<User> {
        let header = decode_header(token).context("failed to decode JWT header")?;
        let kid = header.kid.ok_or_else(|| anyhow!("JWT missing kid"))?;

        let keys = self.get_keys().await?;
        let jwk = keys
            .iter()
            .find(|k| k.kid == kid)
            .ok_or_else(|| anyhow!("no matching key for kid={}", kid))?;

        let decoding_key = DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
            .context("failed to build decoding key from JWK")?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.aud]);
        validation.set_issuer(&[&self.issuer]);

        let token_data = decode::<CfClaims>(token, &decoding_key, &validation)
            .context("JWT validation failed")?;

        let email = token_data.claims.email;
        let username = email_to_username(&email);
        let is_admin = self.admin_users.contains(&username);

        Ok(User { username, email, is_admin })
    }

    async fn get_keys(&self) -> Result<Vec<Jwk>> {
        // Fast path: check cache with read lock
        {
            let cache = self.cache.read().await;
            if let Some(ref c) = *cache {
                if c.fetched_at.elapsed() < self.cache_ttl {
                    return Ok(c.keys.clone());
                }
            }
        }

        // Slow path: fetch fresh keys
        let mut cache = self.cache.write().await;

        // Double-check after acquiring write lock
        if let Some(ref c) = *cache {
            if c.fetched_at.elapsed() < self.cache_ttl {
                return Ok(c.keys.clone());
            }
        }

        info!("Fetching CF JWKS from {}", self.jwks_url);

        let response = reqwest::get(&self.jwks_url)
            .await
            .context("failed to fetch JWKS")?;

        if !response.status().is_success() {
            // Return stale cache if available rather than hard-failing
            if let Some(ref c) = *cache {
                warn!("JWKS fetch failed ({}), using stale cache", response.status());
                return Ok(c.keys.clone());
            }
            return Err(anyhow!("JWKS fetch failed: {}", response.status()));
        }

        let jwks: JwksResponse = response.json().await.context("failed to parse JWKS")?;

        let keys = jwks.keys;
        *cache = Some(JwksCache {
            keys: keys.clone(),
            fetched_at: Instant::now(),
        });

        Ok(keys)
    }
}

/// Convert an email address to a safe username:
/// strips domain, lowercases, removes non-alphanumeric/hyphen chars, truncates to 39.
/// e.g. "John.Doe@gmail.com" → "johndoe"
pub fn email_to_username(email: &str) -> String {
    let prefix = email.split('@').next().unwrap_or("user");
    let lower = prefix.to_lowercase();
    let cleaned = USERNAME_RE.replace_all(&lower, "");
    let truncated = &cleaned[..cleaned.len().min(39)];
    if truncated.is_empty() {
        "user".to_string()
    } else {
        truncated.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_to_username() {
        assert_eq!(email_to_username("john.doe@gmail.com"), "johndoe");
        assert_eq!(email_to_username("jane-smith@org.com"), "jane-smith");
        assert_eq!(email_to_username("UPPER@example.com"), "upper");
        assert_eq!(email_to_username("a_b_c@example.com"), "abc");
        assert_eq!(email_to_username("@example.com"), "user");
    }
}