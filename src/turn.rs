//! Backend-only TURN issuance. No World or Participant authorization lives here.
use crate::error::ApiError;
use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct IceServer {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}
#[derive(Clone, Serialize)]
pub(crate) struct IceConfiguration {
    pub ice_servers: Vec<IceServer>,
    pub expires_at: DateTime<Utc>,
    pub principal: Uuid,
}

#[derive(Clone)]
pub struct TurnProvider {
    kind: Provider,
    ttl: u32,
    concurrent: Arc<Semaphore>,
    metrics: Arc<Metrics>,
}
#[derive(Clone)]
enum Provider {
    Disabled,
    Coturn {
        urls: Vec<String>,
        secret: String,
    },
    Cloudflare {
        key: String,
        token: String,
        http: reqwest::Client,
        endpoint: String,
    },
}

#[derive(Default)]
struct Metrics {
    issued: AtomicU64,
    failed: AtomicU64,
    revoked: AtomicU64,
    revoke_failed: AtomicU64,
    unavailable: AtomicBool,
}

impl TurnProvider {
    pub fn disabled() -> Self {
        Self {
            kind: Provider::Disabled,
            ttl: 21600,
            concurrent: Arc::new(Semaphore::new(4)),
            metrics: Arc::new(Metrics::default()),
        }
    }
    pub fn coturn(urls: Vec<String>, secret: String, ttl: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (300..=86400).contains(&ttl),
            "TURN_TTL_SECONDS must be 300..86400"
        );
        anyhow::ensure!(
            secret.len() >= 32 && secret.len() <= 256,
            "coturn secret must be 32..256 bytes"
        );
        anyhow::ensure!(
            !urls.is_empty()
                && urls.len() <= 4
                && urls.iter().all(|u| valid_url(u))
                && urls.iter().any(|u| u.starts_with("turn:")),
            "coturn requires bounded UDP ICE URLs"
        );
        Ok(Self {
            kind: Provider::Coturn { urls, secret },
            ttl,
            ..Self::disabled()
        })
    }
    pub fn from_env() -> anyhow::Result<Self> {
        let ttl = std::env::var("TURN_TTL_SECONDS")
            .unwrap_or_else(|_| "21600".into())
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("invalid TURN_TTL_SECONDS"))?;
        anyhow::ensure!(
            (300..=86400).contains(&ttl),
            "TURN_TTL_SECONDS must be 300..86400"
        );
        match std::env::var("TURN_PROVIDER")
            .as_deref()
            .unwrap_or("disabled")
        {
            "disabled" => Ok(Self::disabled()),
            "coturn" => Self::coturn(
                required("COTURN_URLS")?
                    .split(',')
                    .map(str::to_owned)
                    .collect(),
                required("COTURN_SHARED_SECRET")?,
                ttl,
            ),
            "cloudflare" => {
                let key = required("CLOUDFLARE_TURN_KEY_ID")?;
                let token = required("CLOUDFLARE_TURN_API_TOKEN")?;
                anyhow::ensure!(
                    key.len() <= 128 && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
                    "invalid TURN key identifier"
                );
                let http = reqwest::Client::builder()
                    .user_agent(concat!("rookframe-community/", env!("CARGO_PKG_VERSION")))
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(5))
                    .connect_timeout(Duration::from_secs(3))
                    .build()?;
                Ok(Self {
                    kind: Provider::Cloudflare {
                        key,
                        token,
                        http,
                        endpoint: "https://rtc.live.cloudflare.com/v1/turn/keys".into(),
                    },
                    ttl,
                    ..Self::disabled()
                })
            }
            _ => anyhow::bail!("TURN_PROVIDER must be disabled, cloudflare or coturn"),
        }
    }
    pub(crate) fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl.into())
    }

    pub(crate) fn ready(&self) -> bool {
        !matches!(self.kind, Provider::Disabled)
            && !self.metrics.unavailable.load(Ordering::Relaxed)
    }
    pub(crate) fn metrics(&self) -> String {
        format!(
            "# TYPE community_turn_issued_total counter\ncommunity_turn_issued_total {}\n# TYPE community_turn_failed_total counter\ncommunity_turn_failed_total {}\n# TYPE community_turn_revoked_total counter\ncommunity_turn_revoked_total {}\n# TYPE community_turn_revoke_failed_total counter\ncommunity_turn_revoke_failed_total {}\n",
            self.metrics.issued.load(Ordering::Relaxed),
            self.metrics.failed.load(Ordering::Relaxed),
            self.metrics.revoked.load(Ordering::Relaxed),
            self.metrics.revoke_failed.load(Ordering::Relaxed)
        )
    }
    pub(crate) async fn issue(&self, principal: Uuid) -> Result<IceConfiguration, ApiError> {
        let result = self.issue_inner(principal).await;
        self.metrics
            .unavailable
            .store(result.is_err(), Ordering::Relaxed);
        if result.is_ok() {
            &self.metrics.issued
        } else {
            &self.metrics.failed
        }
        .fetch_add(1, Ordering::Relaxed);
        result
    }
    async fn issue_inner(&self, principal: Uuid) -> Result<IceConfiguration, ApiError> {
        let _permit = self.concurrent.try_acquire().map_err(|_| unavailable())?;
        let expires_at = Utc::now() + chrono::Duration::seconds(self.ttl.into());
        let ice_servers = match &self.kind {
            Provider::Disabled => return Err(unavailable()),
            Provider::Coturn { urls, secret } => {
                let username = format!("{}:{principal}", expires_at.timestamp());
                let mut mac =
                    Hmac::<Sha1>::new_from_slice(secret.as_bytes()).map_err(|_| unavailable())?;
                mac.update(username.as_bytes());
                let credential = STANDARD.encode(mac.finalize().into_bytes());
                urls.iter()
                    .map(|url| IceServer {
                        urls: vec![url.clone()],
                        username: url.starts_with("turn:").then(|| username.clone()),
                        credential: url.starts_with("turn:").then(|| credential.clone()),
                    })
                    .collect()
            }
            Provider::Cloudflare {
                key,
                token,
                http,
                endpoint,
            } => {
                let response = http.post(format!("{endpoint}/{key}/credentials/generate-ice-servers"))
                    .bearer_auth(token).json(&serde_json::json!({"ttl":self.ttl,"customIdentifier":principal.to_string()}))
                    .send().await.map_err(|_| unavailable())?;
                if !response.status().is_success() {
                    return Err(unavailable());
                }
                #[derive(Deserialize)]
                struct Reply {
                    #[serde(rename = "iceServers")]
                    ice_servers: Vec<IceServer>,
                }
                let reply: Reply = serde_json::from_slice(&bounded_body(response).await?)
                    .map_err(|_| unavailable())?;
                if reply.ice_servers.len() > 8 {
                    return Err(unavailable());
                }
                let mut servers = Vec::new();
                for mut server in reply.ice_servers {
                    if server.urls.len() > 16 {
                        return Err(unavailable());
                    }
                    server.urls.retain(|url| valid_url(url));
                    if server.urls.is_empty() {
                        continue;
                    }
                    if server.urls.iter().any(|u| u.starts_with("turn:"))
                        && (!valid_credential(server.username.as_deref())
                            || !valid_credential(server.credential.as_deref()))
                    {
                        return Err(unavailable());
                    }
                    servers.push(server);
                }
                if servers.iter().any(|s| s.urls.len() > 4)
                    || servers.iter().map(|s| s.urls.len()).sum::<usize>() > 8
                {
                    return Err(unavailable());
                }
                if !servers
                    .iter()
                    .any(|s| s.urls.iter().any(|u| u.starts_with("turn:")))
                {
                    return Err(unavailable());
                }
                servers
            }
        };
        Ok(IceConfiguration {
            ice_servers,
            expires_at,
            principal,
        })
    }

    // Coturn REST credentials have no per-credential revocation API. Revocation
    // denies further issuance; existing credentials expire. Operators can rotate
    // the shared secret and close allocations for emergency provider-wide denial.
    pub(crate) async fn revoke(&self, configuration: &IceConfiguration) -> Result<bool, ApiError> {
        let result = self.revoke_inner(configuration).await;
        match result {
            Ok(true) => {
                self.metrics.revoked.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.metrics.revoke_failed.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        result
    }

    async fn revoke_inner(&self, configuration: &IceConfiguration) -> Result<bool, ApiError> {
        let Provider::Cloudflare {
            key,
            token,
            http,
            endpoint,
        } = &self.kind
        else {
            return Ok(false);
        };
        let _permit = self.concurrent.try_acquire().map_err(|_| unavailable())?;
        let username = configuration
            .ice_servers
            .iter()
            .find_map(|s| s.username.as_deref())
            .ok_or_else(unavailable)?;
        let mut url = reqwest::Url::parse(&format!("{endpoint}/{key}/credentials/"))
            .map_err(|_| unavailable())?;
        url.path_segments_mut()
            .map_err(|_| unavailable())?
            .pop_if_empty()
            .push(username)
            .push("revoke");
        let response = http
            .post(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if response.status() != StatusCode::NO_CONTENT {
            return Err(unavailable());
        }
        Ok(true)
    }
}

fn required(name: &str) -> anyhow::Result<String> {
    std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{name} is required"))
}
fn valid_credential(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v.len() <= 1024 && !v.chars().any(char::is_control))
}
fn valid_url(value: &str) -> bool {
    if value.len() > 512 || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = value.split_once(':') else {
        return false;
    };
    if !matches!(scheme, "stun" | "turn") {
        return false;
    }
    let authority = rest.strip_suffix("?transport=udp").unwrap_or(rest);
    if authority.contains(['?', '/', '#', '@']) {
        return false;
    }
    let Ok(url) = url::Url::parse(&format!("http://{authority}")) else {
        return false;
    };
    url.host_str().is_some()
        && url.port().unwrap_or(3478) != 0
        && url.username().is_empty()
        && url.password().is_none()
}
async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, ApiError> {
    if response.content_length().is_some_and(|n| n > 16384) {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 16384 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
pub(crate) fn unavailable() -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "relay_unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::HeaderMap, routing::post};

    #[tokio::test]
    async fn cloudflare_http_contract_keeps_token_backend_only_filters_udp_and_revokes() {
        let fixture = Router::new()
            .route("/test-key/credentials/generate-ice-servers", post(|headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                assert_eq!(headers["authorization"], "Bearer test-backend-token");
                assert_eq!(body["ttl"], 21600);
                assert!(Uuid::parse_str(body["customIdentifier"].as_str().unwrap()).is_ok());
                (StatusCode::CREATED, Json(serde_json::json!({"iceServers":[
                    {"urls":["stun:stun.cloudflare.com:3478"]},
                    {"urls":["turn:turn.cloudflare.com:3478?transport=udp","turn:turn.cloudflare.com:3478?transport=tcp","turns:turn.cloudflare.com:443?transport=tcp"],
                        "username":"temporary-user", "credential":"temporary-password"}
                ]})))
            }))
            .route("/test-key/credentials/temporary-user/revoke", post(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer test-backend-token");
                StatusCode::NO_CONTENT
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
        let provider = TurnProvider {
            kind: Provider::Cloudflare {
                key: "test-key".into(),
                token: "test-backend-token".into(),
                http: reqwest::Client::builder().no_proxy().build().unwrap(),
                endpoint,
            },
            ..TurnProvider::disabled()
        };
        let result = provider
            .issue(Uuid::new_v4())
            .await
            .unwrap_or_else(|_| panic!("expected a normalized credential"));
        assert_eq!(result.ice_servers.len(), 2);
        assert_eq!(
            result.ice_servers[1].urls,
            ["turn:turn.cloudflare.com:3478?transport=udp"]
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("test-backend-token")
        );
        assert!(provider.revoke(&result).await.unwrap_or(false));
        assert!(
            provider
                .metrics()
                .contains("community_turn_revoked_total 1")
        );
        assert!(!provider.metrics().contains("temporary"));
        server.abort();
    }
}
