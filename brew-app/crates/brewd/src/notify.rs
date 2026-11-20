//! Push notifications through ntfy (<https://ntfy.sh>, or a self-hosted server).
//!
//! Published as JSON to the server root rather than as headers to the topic URL:
//! headers are ASCII, and "⅓ sugar break" and "°C" are not.

use std::time::Duration;

use brewcore::alert::{Alert, Rule};
use serde::Serialize;

use crate::config::Notify;

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Ntfy {
    http: reqwest::Client,
    server: String,
    topic: String,
    token: Option<String>,
}

#[derive(Serialize)]
struct Message<'a> {
    topic: &'a str,
    title: &'a str,
    message: &'a str,
    /// ntfy's 1–5; 4 is "high", which a phone may sound through do-not-disturb.
    priority: u8,
    tags: &'a [&'a str],
}

impl Ntfy {
    pub fn new(config: &Notify) -> reqwest::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().timeout(TIMEOUT).build()?,
            server: config.ntfy_server.clone(),
            topic: config.ntfy_topic.clone(),
            token: config.ntfy_token.clone(),
        })
    }

    pub async fn send_alert(&self, alert: &Alert) -> Result<(), String> {
        // Something to act on now is high; something to act on today is default.
        let (priority, tags): (u8, &[&str]) = match alert.rule {
            Rule::Stalled | Rule::TempHigh | Rule::TempLow | Rule::Silent => (4, &["warning"]),
            Rule::Finished | Rule::SugarBreak | Rule::TargetSg => (3, &["white_check_mark"]),
        };
        self.send(&alert.title(), &alert.detail, priority, tags)
            .await
    }

    pub async fn send(
        &self,
        title: &str,
        message: &str,
        priority: u8,
        tags: &[&str],
    ) -> Result<(), String> {
        let body = serde_json::to_vec(&Message {
            topic: &self.topic,
            title,
            message,
            priority,
            tags,
        })
        .map_err(|e| e.to_string())?;
        let mut request = self
            .http
            .post(&self.server)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let text = response.text().await.unwrap_or_default();
        Err(format!("{status}: {}", text.trim()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::{Router, extract::State, http::StatusCode, routing::post};
    use std::sync::{Arc, Mutex};

    /// A stand-in ntfy server that records what it is sent and answers `status`.
    pub(crate) async fn fake_ntfy(
        status: StatusCode,
    ) -> (Notify, Arc<Mutex<Vec<serde_json::Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/",
                post(
                    |State((seen, status)): State<(
                        Arc<Mutex<Vec<serde_json::Value>>>,
                        StatusCode,
                    )>,
                     body: axum::body::Bytes| async move {
                        seen.lock()
                            .unwrap()
                            .push(serde_json::from_slice(&body).unwrap());
                        status
                    },
                ),
            )
            .with_state((seen.clone(), status));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = Notify {
            ntfy_server: format!("http://{addr}/"),
            ntfy_topic: "brewery".into(),
            ntfy_token: None,
        };
        (config, seen)
    }

    #[tokio::test]
    async fn an_alert_is_published_as_json_and_a_refusal_is_an_error() {
        let (config, seen) = fake_ntfy(StatusCode::OK).await;
        let ntfy = Ntfy::new(&config).unwrap();
        let alert = Alert {
            id: 1,
            batch_id: 7,
            code: "SELTZER-007".into(),
            rule: Rule::SugarBreak,
            device: None,
            raised_at: brewcore::units::Timestamp::from_secs(0),
            detail: "SG 1.027, ⅓ break at 1.027".into(),
            acked_at: None,
            snoozed_until: None,
            notified_at: None,
        };
        ntfy.send_alert(&alert).await.unwrap();
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent[0]["topic"], "brewery");
        assert_eq!(sent[0]["title"], "SELTZER-007: ⅓ sugar break");
        assert_eq!(sent[0]["message"], "SG 1.027, ⅓ break at 1.027");
        assert_eq!(sent[0]["priority"], 3);

        let (config, _) = fake_ntfy(StatusCode::TOO_MANY_REQUESTS).await;
        let err = Ntfy::new(&config)
            .unwrap()
            .send_alert(&alert)
            .await
            .unwrap_err();
        assert!(err.starts_with("429"), "{err}");
    }
}
