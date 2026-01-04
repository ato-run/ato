//! Polar Webhook Handler
//!
//! Implements Standard Webhooks specification for Polar integration.
//! https://www.standardwebhooks.com/
//! https://polar.sh/docs/integrate/webhooks
//!
//! Security:
//! - HMAC-SHA256 signature verification
//! - Constant-time signature comparison to prevent timing attacks
//! - Timestamp validation to prevent replay attacks

use anyhow::{anyhow, Context, Result};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use tracing::{debug, error, info, warn};

use super::subscription::SubscriptionStore;

/// Standard Webhooks header names
const HEADER_WEBHOOK_ID: &str = "webhook-id";
const HEADER_WEBHOOK_TIMESTAMP: &str = "webhook-timestamp";
const HEADER_WEBHOOK_SIGNATURE: &str = "webhook-signature";

/// Maximum age of webhook timestamp (5 minutes)
const MAX_TIMESTAMP_AGE_SECS: u64 = 300;

type HmacSha256 = Hmac<Sha256>;

/// Polar webhook configuration
#[derive(Clone)]
pub struct PolarWebhookConfig {
    /// Webhook signing secret (base64 encoded, may have `whsec_` prefix)
    pub secret: String,
    /// Whether to skip signature verification (for testing only)
    pub skip_verification: bool,
}

impl PolarWebhookConfig {
    pub fn from_env() -> Result<Self> {
        let secret = std::env::var("POLAR_WEBHOOK_SECRET")
            .context("POLAR_WEBHOOK_SECRET environment variable not set")?;

        Ok(Self {
            secret,
            skip_verification: false,
        })
    }

    /// Decode the secret for HMAC verification
    fn decode_secret(&self) -> Result<Vec<u8>> {
        let secret = self.secret.strip_prefix("whsec_").unwrap_or(&self.secret);
        BASE64
            .decode(secret)
            .context("Failed to decode webhook secret")
    }
}

/// Polar webhook event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolarEventType {
    #[serde(rename = "subscription.created")]
    SubscriptionCreated,
    #[serde(rename = "subscription.active")]
    SubscriptionActive,
    #[serde(rename = "subscription.updated")]
    SubscriptionUpdated,
    #[serde(rename = "subscription.canceled")]
    SubscriptionCanceled,
    #[serde(rename = "subscription.uncanceled")]
    SubscriptionUncanceled,
    #[serde(rename = "subscription.revoked")]
    SubscriptionRevoked,
    #[serde(rename = "order.created")]
    OrderCreated,
    #[serde(rename = "order.paid")]
    OrderPaid,
    #[serde(rename = "order.updated")]
    OrderUpdated,
    #[serde(rename = "order.refunded")]
    OrderRefunded,
    #[serde(rename = "customer.created")]
    CustomerCreated,
    #[serde(rename = "customer.updated")]
    CustomerUpdated,
    #[serde(rename = "customer.deleted")]
    CustomerDeleted,
    #[serde(rename = "customer.state_changed")]
    CustomerStateChanged,
    #[serde(rename = "checkout.created")]
    CheckoutCreated,
    #[serde(rename = "checkout.updated")]
    CheckoutUpdated,
    #[serde(rename = "benefit_grant.created")]
    BenefitGrantCreated,
    #[serde(rename = "benefit_grant.updated")]
    BenefitGrantUpdated,
    #[serde(rename = "benefit_grant.revoked")]
    BenefitGrantRevoked,
    #[serde(other)]
    Unknown,
}

/// Polar webhook payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolarWebhookPayload {
    /// Event type
    #[serde(rename = "type")]
    pub event_type: String,
    /// ISO 8601 timestamp of when the event occurred
    pub timestamp: Option<String>,
    /// Event data
    pub data: serde_json::Value,
}

/// Subscription data from Polar
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionData {
    pub id: String,
    pub customer_id: String,
    pub product_id: String,
    pub status: String,
    pub current_period_start: Option<String>,
    pub current_period_end: Option<String>,
    pub cancel_at_period_end: Option<bool>,
    pub canceled_at: Option<String>,
    pub ended_at: Option<String>,
}

/// Customer data from Polar
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomerData {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
}

/// Order data from Polar
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderData {
    pub id: String,
    pub customer_id: String,
    pub product_id: String,
    pub subscription_id: Option<String>,
    pub status: String,
    pub amount: i64,
    pub currency: String,
    pub billing_reason: Option<String>,
}

/// Polar webhook handler state
pub struct PolarWebhookHandler {
    config: PolarWebhookConfig,
    subscription_store: Arc<dyn SubscriptionStore + Send + Sync>,
}

impl PolarWebhookHandler {
    pub fn new(
        config: PolarWebhookConfig,
        subscription_store: Arc<dyn SubscriptionStore + Send + Sync>,
    ) -> Self {
        Self {
            config,
            subscription_store,
        }
    }

    /// Verify webhook signature according to Standard Webhooks spec
    pub fn verify_signature(&self, headers: &HeaderMap, body: &[u8]) -> Result<()> {
        if self.config.skip_verification {
            warn!("Webhook signature verification is disabled!");
            return Ok(());
        }

        // Extract headers
        let msg_id = headers
            .get(HEADER_WEBHOOK_ID)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow!("Missing {} header", HEADER_WEBHOOK_ID))?;

        let timestamp_str = headers
            .get(HEADER_WEBHOOK_TIMESTAMP)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow!("Missing {} header", HEADER_WEBHOOK_TIMESTAMP))?;

        let signature_header = headers
            .get(HEADER_WEBHOOK_SIGNATURE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow!("Missing {} header", HEADER_WEBHOOK_SIGNATURE))?;

        // Validate timestamp to prevent replay attacks
        let timestamp: u64 = timestamp_str.parse().context("Invalid timestamp format")?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if now > timestamp && now - timestamp > MAX_TIMESTAMP_AGE_SECS {
            return Err(anyhow!("Webhook timestamp is too old"));
        }

        if timestamp > now + 60 {
            return Err(anyhow!("Webhook timestamp is in the future"));
        }

        // Build signed content: msg_id.timestamp.payload
        let signed_content = format!(
            "{}.{}.{}",
            msg_id,
            timestamp_str,
            String::from_utf8_lossy(body)
        );

        // Decode secret
        let secret = self.config.decode_secret()?;

        // Create HMAC
        let mut mac = HmacSha256::new_from_slice(&secret).context("Failed to create HMAC")?;
        mac.update(signed_content.as_bytes());
        let expected_signature = mac.finalize().into_bytes();

        // Parse signatures (space-delimited, may have v1, prefix)
        let signatures: Vec<&str> = signature_header.split(' ').collect();

        for sig in signatures {
            // Handle v1,<signature> format
            if let Some(sig_value) = sig.strip_prefix("v1,") {
                // Decode and compare using constant-time comparison
                // to prevent timing attacks
                if let Ok(received_sig) = BASE64.decode(sig_value) {
                    if received_sig.len() == expected_signature.len()
                        && bool::from(received_sig.ct_eq(&expected_signature[..]))
                    {
                        debug!("Webhook signature verified successfully");
                        return Ok(());
                    }
                }
            }
        }

        Err(anyhow!("Invalid webhook signature"))
    }

    /// Process a webhook event
    pub async fn process_event(&self, payload: &PolarWebhookPayload) -> Result<()> {
        info!("Processing Polar webhook event: {}", payload.event_type);

        match payload.event_type.as_str() {
            "subscription.created" | "subscription.active" | "subscription.updated" => {
                let sub: SubscriptionData = serde_json::from_value(payload.data.clone())
                    .context("Failed to parse subscription data")?;
                self.handle_subscription_update(&sub).await?;
            }
            "subscription.canceled" => {
                let sub: SubscriptionData = serde_json::from_value(payload.data.clone())
                    .context("Failed to parse subscription data")?;
                self.handle_subscription_canceled(&sub).await?;
            }
            "subscription.revoked" => {
                let sub: SubscriptionData = serde_json::from_value(payload.data.clone())
                    .context("Failed to parse subscription data")?;
                self.handle_subscription_revoked(&sub).await?;
            }
            "order.paid" => {
                let order: OrderData = serde_json::from_value(payload.data.clone())
                    .context("Failed to parse order data")?;
                self.handle_order_paid(&order).await?;
            }
            "customer.created" => {
                let customer: CustomerData = serde_json::from_value(payload.data.clone())
                    .context("Failed to parse customer data")?;
                self.handle_customer_created(&customer).await?;
            }
            _ => {
                debug!("Ignoring unhandled event type: {}", payload.event_type);
            }
        }

        Ok(())
    }

    async fn handle_subscription_update(&self, sub: &SubscriptionData) -> Result<()> {
        info!(
            "Subscription update: id={}, status={}, customer={}",
            sub.id, sub.status, sub.customer_id
        );

        self.subscription_store
            .upsert_subscription(
                &sub.id,
                &sub.customer_id,
                &sub.product_id,
                &sub.status,
                sub.current_period_end.as_deref(),
            )
            .await
    }

    async fn handle_subscription_canceled(&self, sub: &SubscriptionData) -> Result<()> {
        info!(
            "Subscription canceled: id={}, customer={}",
            sub.id, sub.customer_id
        );

        self.subscription_store
            .update_subscription_status(&sub.id, "canceled")
            .await
    }

    async fn handle_subscription_revoked(&self, sub: &SubscriptionData) -> Result<()> {
        info!(
            "Subscription revoked: id={}, customer={}",
            sub.id, sub.customer_id
        );

        self.subscription_store
            .update_subscription_status(&sub.id, "revoked")
            .await
    }

    async fn handle_order_paid(&self, order: &OrderData) -> Result<()> {
        info!(
            "Order paid: id={}, amount={} {}, customer={}",
            order.id, order.amount, order.currency, order.customer_id
        );

        // If this is a subscription renewal, update the subscription
        if let Some(ref sub_id) = order.subscription_id {
            if order.billing_reason.as_deref() == Some("subscription_cycle") {
                info!("Subscription {} renewed via order {}", sub_id, order.id);
            }
        }

        Ok(())
    }

    async fn handle_customer_created(&self, customer: &CustomerData) -> Result<()> {
        info!(
            "Customer created: id={}, email={}",
            customer.id, customer.email
        );

        self.subscription_store
            .upsert_customer(&customer.id, &customer.email, customer.name.as_deref())
            .await
    }
}

/// Axum handler for Polar webhooks
pub async fn polar_webhook_handler(
    State(handler): State<Arc<PolarWebhookHandler>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify signature
    if let Err(e) = handler.verify_signature(&headers, &body) {
        error!("Webhook signature verification failed: {}", e);
        return (StatusCode::FORBIDDEN, "Invalid signature");
    }

    // Parse payload
    let payload: PolarWebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to parse webhook payload: {}", e);
            return (StatusCode::BAD_REQUEST, "Invalid payload");
        }
    };

    // Process event (async, but respond immediately)
    if let Err(e) = handler.process_event(&payload).await {
        error!("Failed to process webhook event: {}", e);
        // Still return 202 to prevent retries for processing errors
        // Real errors should be handled via monitoring
    }

    (StatusCode::ACCEPTED, "")
}

/// Create Axum router for Polar webhooks
pub fn polar_webhook_router(handler: Arc<PolarWebhookHandler>) -> Router {
    Router::new()
        .route("/webhook/polar", post(polar_webhook_handler))
        .with_state(handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock subscription store for testing
    struct MockSubscriptionStore {
        subscriptions: Mutex<HashMap<String, (String, String)>>,
        customers: Mutex<HashMap<String, String>>,
    }

    impl MockSubscriptionStore {
        fn new() -> Self {
            Self {
                subscriptions: Mutex::new(HashMap::new()),
                customers: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl SubscriptionStore for MockSubscriptionStore {
        async fn upsert_subscription(
            &self,
            id: &str,
            customer_id: &str,
            _product_id: &str,
            status: &str,
            _period_end: Option<&str>,
        ) -> Result<()> {
            self.subscriptions.lock().unwrap().insert(
                id.to_string(),
                (customer_id.to_string(), status.to_string()),
            );
            Ok(())
        }

        async fn update_subscription_status(&self, id: &str, status: &str) -> Result<()> {
            if let Some(sub) = self.subscriptions.lock().unwrap().get_mut(id) {
                sub.1 = status.to_string();
            }
            Ok(())
        }

        async fn get_subscription(&self, id: &str) -> Result<Option<(String, String)>> {
            Ok(self.subscriptions.lock().unwrap().get(id).cloned())
        }

        async fn upsert_customer(&self, id: &str, email: &str, _name: Option<&str>) -> Result<()> {
            self.customers
                .lock()
                .unwrap()
                .insert(id.to_string(), email.to_string());
            Ok(())
        }
    }

    fn create_test_config() -> PolarWebhookConfig {
        // Test secret: "test-secret" base64 encoded
        PolarWebhookConfig {
            secret: "whsec_dGVzdC1zZWNyZXQ=".to_string(),
            skip_verification: false,
        }
    }

    fn create_test_signature(secret: &str, msg_id: &str, timestamp: u64, body: &str) -> String {
        let secret_bytes = BASE64
            .decode(secret.strip_prefix("whsec_").unwrap_or(secret))
            .unwrap();
        let signed_content = format!("{}.{}.{}", msg_id, timestamp, body);

        let mut mac = HmacSha256::new_from_slice(&secret_bytes).unwrap();
        mac.update(signed_content.as_bytes());
        let signature = mac.finalize().into_bytes();

        format!("v1,{}", BASE64.encode(signature))
    }

    #[test]
    fn test_verify_signature_valid() {
        let config = create_test_config();
        let store = Arc::new(MockSubscriptionStore::new());
        let handler = PolarWebhookHandler::new(config.clone(), store);

        let msg_id = "msg_test123";
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let body = r#"{"type":"subscription.created","data":{}}"#;

        let signature = create_test_signature(&config.secret, msg_id, timestamp, body);

        let mut headers = HeaderMap::new();
        headers.insert(HEADER_WEBHOOK_ID, msg_id.parse().unwrap());
        headers.insert(
            HEADER_WEBHOOK_TIMESTAMP,
            timestamp.to_string().parse().unwrap(),
        );
        headers.insert(HEADER_WEBHOOK_SIGNATURE, signature.parse().unwrap());

        let result = handler.verify_signature(&headers, body.as_bytes());
        assert!(result.is_ok(), "Signature verification should succeed");
    }

    #[test]
    fn test_verify_signature_invalid() {
        let config = create_test_config();
        let store = Arc::new(MockSubscriptionStore::new());
        let handler = PolarWebhookHandler::new(config, store);

        let msg_id = "msg_test123";
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let body = r#"{"type":"subscription.created","data":{}}"#;

        let mut headers = HeaderMap::new();
        headers.insert(HEADER_WEBHOOK_ID, msg_id.parse().unwrap());
        headers.insert(
            HEADER_WEBHOOK_TIMESTAMP,
            timestamp.to_string().parse().unwrap(),
        );
        headers.insert(
            HEADER_WEBHOOK_SIGNATURE,
            "v1,invalid_signature".parse().unwrap(),
        );

        let result = handler.verify_signature(&headers, body.as_bytes());
        assert!(result.is_err(), "Signature verification should fail");
    }

    #[test]
    fn test_verify_signature_expired_timestamp() {
        let config = create_test_config();
        let store = Arc::new(MockSubscriptionStore::new());
        let handler = PolarWebhookHandler::new(config.clone(), store);

        let msg_id = "msg_test123";
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 600; // 10 minutes ago
        let body = r#"{"type":"subscription.created","data":{}}"#;

        let signature = create_test_signature(&config.secret, msg_id, timestamp, body);

        let mut headers = HeaderMap::new();
        headers.insert(HEADER_WEBHOOK_ID, msg_id.parse().unwrap());
        headers.insert(
            HEADER_WEBHOOK_TIMESTAMP,
            timestamp.to_string().parse().unwrap(),
        );
        headers.insert(HEADER_WEBHOOK_SIGNATURE, signature.parse().unwrap());

        let result = handler.verify_signature(&headers, body.as_bytes());
        assert!(result.is_err(), "Expired timestamp should fail");
    }

    #[tokio::test]
    async fn test_process_subscription_created() {
        let config = PolarWebhookConfig {
            secret: "test".to_string(),
            skip_verification: true,
        };
        let store = Arc::new(MockSubscriptionStore::new());
        let handler = PolarWebhookHandler::new(config, store.clone());

        let payload = PolarWebhookPayload {
            event_type: "subscription.created".to_string(),
            timestamp: Some("2024-01-01T00:00:00Z".to_string()),
            data: serde_json::json!({
                "id": "sub_123",
                "customer_id": "cus_456",
                "product_id": "prod_789",
                "status": "active",
                "current_period_end": "2024-02-01T00:00:00Z"
            }),
        };

        let result = handler.process_event(&payload).await;
        assert!(result.is_ok());

        let sub = store.subscriptions.lock().unwrap();
        assert!(sub.contains_key("sub_123"));
        let (customer_id, status) = sub.get("sub_123").unwrap();
        assert_eq!(customer_id, "cus_456");
        assert_eq!(status, "active");
    }

    #[tokio::test]
    async fn test_process_customer_created() {
        let config = PolarWebhookConfig {
            secret: "test".to_string(),
            skip_verification: true,
        };
        let store = Arc::new(MockSubscriptionStore::new());
        let handler = PolarWebhookHandler::new(config, store.clone());

        let payload = PolarWebhookPayload {
            event_type: "customer.created".to_string(),
            timestamp: Some("2024-01-01T00:00:00Z".to_string()),
            data: serde_json::json!({
                "id": "cus_456",
                "email": "test@example.com",
                "name": "Test User"
            }),
        };

        let result = handler.process_event(&payload).await;
        assert!(result.is_ok());

        let customers = store.customers.lock().unwrap();
        assert_eq!(
            customers.get("cus_456"),
            Some(&"test@example.com".to_string())
        );
    }
}
