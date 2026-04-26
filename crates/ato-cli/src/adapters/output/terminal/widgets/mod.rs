use crate::search::CapsuleSummary;

pub fn detail_lines(item: Option<&CapsuleSummary>) -> Vec<String> {
    match item {
        None => vec!["No item selected".to_string()],
        Some(capsule) => {
            let scoped = capsule
                .scoped_id
                .clone()
                .unwrap_or_else(|| format!("{}/{}", capsule.publisher.handle, capsule.slug));
            let mut lines = vec![
                format!("Scoped ID: {}", scoped),
                format!("Name: {}", capsule.name),
                format!(
                    "Version: {}",
                    capsule.latest_version.as_deref().unwrap_or("unknown")
                ),
                format!("Category: {}", capsule.category),
                format!("Type: {}", capsule.capsule_type),
                format!("Downloads: {}", capsule.downloads),
                format!(
                    "Publisher: {}{}",
                    capsule.publisher.handle,
                    if capsule.publisher.verified {
                        " ✓"
                    } else {
                        ""
                    }
                ),
            ];

            if capsule.price == 0 {
                lines.push("Price: Free".to_string());
            } else {
                lines.push(format!("Price: {} {}", capsule.price, capsule.currency));
            }

            lines.push(String::new());
            lines.push("Description:".to_string());
            if capsule.description.trim().is_empty() {
                lines.push("(empty)".to_string());
            } else {
                lines.push(capsule.description.clone());
            }
            lines
        }
    }
}
