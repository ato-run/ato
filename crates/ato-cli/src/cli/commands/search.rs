use crate::search::SearchResult;

pub fn print_search_result(result: &SearchResult) {
    if result.total == 0 {
        println!("🔍 No packages found.");
    } else {
        println!("🔍 Found {} package(s):", result.total);
    }

    for (index, capsule) in result.capsules.iter().enumerate() {
        println!();
        println!("{}. {} ({})", index + 1, capsule.name, capsule.slug);
        if !capsule.description.is_empty() {
            println!("   {}", capsule.description);
        }
        println!(
            "   Category: {} | Type: {} | Version: {}",
            capsule.category,
            capsule.capsule_type,
            capsule.latest_version.as_deref().unwrap_or("unknown")
        );
        println!(
            "   Publisher: {}{} | Downloads: {}",
            capsule.publisher.handle,
            if capsule.publisher.verified {
                " ✓"
            } else {
                ""
            },
            capsule.downloads
        );
        if capsule.price == 0 {
            println!("   Price: Free");
        } else {
            println!("   Price: {} {}", capsule.price, capsule.currency);
        }
        let scoped_id = capsule
            .scoped_id
            .clone()
            .unwrap_or_else(|| format!("{}/{}", capsule.publisher.handle, capsule.slug));
        println!("   Install: ato install {}", scoped_id);
    }

    if let Some(next) = result.next_cursor.as_deref() {
        println!();
        println!("📄 Next cursor: {}", next);
        println!("   Continue: ato search --cursor {}", next);
    }
}
