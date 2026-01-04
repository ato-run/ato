//! Subscription State Management
//!
//! Manages subscription state for billing integration.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Subscription status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Active,
    Canceled,
    Revoked,
    PastDue,
    Trialing,
    Incomplete,
}

impl std::fmt::Display for SubscriptionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Canceled => write!(f, "canceled"),
            Self::Revoked => write!(f, "revoked"),
            Self::PastDue => write!(f, "past_due"),
            Self::Trialing => write!(f, "trialing"),
            Self::Incomplete => write!(f, "incomplete"),
        }
    }
}

impl std::str::FromStr for SubscriptionStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "canceled" => Ok(Self::Canceled),
            "revoked" => Ok(Self::Revoked),
            "past_due" => Ok(Self::PastDue),
            "trialing" => Ok(Self::Trialing),
            "incomplete" => Ok(Self::Incomplete),
            _ => Err(anyhow::anyhow!("Unknown subscription status: {}", s)),
        }
    }
}

/// Subscription record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub customer_id: String,
    pub product_id: String,
    pub status: SubscriptionStatus,
    pub current_period_end: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Customer record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Customer {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub created_at: String,
}

/// Subscription store trait
#[async_trait]
pub trait SubscriptionStore: Send + Sync {
    /// Create or update a subscription
    async fn upsert_subscription(
        &self,
        id: &str,
        customer_id: &str,
        product_id: &str,
        status: &str,
        period_end: Option<&str>,
    ) -> Result<()>;

    /// Update subscription status
    async fn update_subscription_status(&self, id: &str, status: &str) -> Result<()>;

    /// Get subscription by ID
    async fn get_subscription(&self, id: &str) -> Result<Option<(String, String)>>;

    /// Create or update a customer
    async fn upsert_customer(&self, id: &str, email: &str, name: Option<&str>) -> Result<()>;
}

/// In-memory subscription store (for testing/development)
pub struct InMemorySubscriptionStore {
    subscriptions: RwLock<std::collections::HashMap<String, Subscription>>,
    customers: RwLock<std::collections::HashMap<String, Customer>>,
}

impl InMemorySubscriptionStore {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(std::collections::HashMap::new()),
            customers: RwLock::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for InMemorySubscriptionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SubscriptionStore for InMemorySubscriptionStore {
    async fn upsert_subscription(
        &self,
        id: &str,
        customer_id: &str,
        product_id: &str,
        status: &str,
        period_end: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let status = status.parse()?;

        let mut subs = self.subscriptions.write().await;

        if let Some(existing) = subs.get_mut(id) {
            existing.status = status;
            existing.current_period_end = period_end.map(String::from);
            existing.updated_at = now;
        } else {
            subs.insert(
                id.to_string(),
                Subscription {
                    id: id.to_string(),
                    customer_id: customer_id.to_string(),
                    product_id: product_id.to_string(),
                    status,
                    current_period_end: period_end.map(String::from),
                    created_at: now.clone(),
                    updated_at: now,
                },
            );
        }

        Ok(())
    }

    async fn update_subscription_status(&self, id: &str, status: &str) -> Result<()> {
        let status = status.parse()?;
        let now = chrono::Utc::now().to_rfc3339();

        let mut subs = self.subscriptions.write().await;
        if let Some(sub) = subs.get_mut(id) {
            sub.status = status;
            sub.updated_at = now;
        }

        Ok(())
    }

    async fn get_subscription(&self, id: &str) -> Result<Option<(String, String)>> {
        let subs = self.subscriptions.read().await;
        Ok(subs
            .get(id)
            .map(|s| (s.customer_id.clone(), s.status.to_string())))
    }

    async fn upsert_customer(&self, id: &str, email: &str, name: Option<&str>) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut customers = self.customers.write().await;

        customers.insert(
            id.to_string(),
            Customer {
                id: id.to_string(),
                email: email.to_string(),
                name: name.map(String::from),
                created_at: now,
            },
        );

        Ok(())
    }
}

/// Check if a customer has an active subscription
pub async fn is_customer_subscribed(
    _store: &dyn SubscriptionStore,
    _customer_id: &str,
) -> Result<bool> {
    // This is a simple implementation - in production, you'd want to query by customer_id
    // For now, we'll return false as we don't have a get_by_customer method
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_upsert_subscription() {
        let store = InMemorySubscriptionStore::new();

        store
            .upsert_subscription("sub_1", "cus_1", "prod_1", "active", Some("2024-02-01"))
            .await
            .unwrap();

        let result = store.get_subscription("sub_1").await.unwrap();
        assert!(result.is_some());
        let (customer_id, status) = result.unwrap();
        assert_eq!(customer_id, "cus_1");
        assert_eq!(status, "active");
    }

    #[tokio::test]
    async fn test_update_subscription_status() {
        let store = InMemorySubscriptionStore::new();

        store
            .upsert_subscription("sub_1", "cus_1", "prod_1", "active", None)
            .await
            .unwrap();

        store
            .update_subscription_status("sub_1", "canceled")
            .await
            .unwrap();

        let result = store.get_subscription("sub_1").await.unwrap();
        let (_, status) = result.unwrap();
        assert_eq!(status, "canceled");
    }

    #[tokio::test]
    async fn test_upsert_customer() {
        let store = InMemorySubscriptionStore::new();

        store
            .upsert_customer("cus_1", "test@example.com", Some("Test User"))
            .await
            .unwrap();

        let customers = store.customers.read().await;
        assert!(customers.contains_key("cus_1"));
        let customer = customers.get("cus_1").unwrap();
        assert_eq!(customer.email, "test@example.com");
        assert_eq!(customer.name, Some("Test User".to_string()));
    }
}
