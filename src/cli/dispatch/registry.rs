use std::io::IsTerminal;

use anyhow::Result;

pub(crate) struct SearchCommandArgs {
    pub(crate) query: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) cursor: Option<String>,
    pub(crate) registry: Option<String>,
    pub(crate) json: bool,
    pub(crate) no_tui: bool,
    pub(crate) show_manifest: bool,
}

pub(crate) fn execute_registry_command(command: crate::RegistryCommands) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        match command {
            crate::RegistryCommands::Resolve { domain, json } => {
                let resolver = crate::registry::RegistryResolver::default();
                match resolver.resolve(&domain).await {
                    Ok(info) => {
                        if json {
                            println!("{}", serde_json::to_string_pretty(&info)?);
                        } else {
                            println!("📡 Registry for {}:", domain);
                            println!("   URL:    {}", info.url);
                            if let Some(name) = &info.name {
                                println!("   Name:   {}", name);
                            }
                            if let Some(key) = &info.public_key {
                                println!("   Key:    {}", key);
                            }
                            println!("   Source: {:?}", info.source);
                        }
                    }
                    Err(error) => {
                        if json {
                            println!(r#"{{"error": "{}"}}"#, error);
                        } else {
                            eprintln!("❌ Failed to resolve registry: {}", error);
                        }
                    }
                }
                Ok(())
            }
            crate::RegistryCommands::List { json } => {
                let resolver = crate::registry::RegistryResolver::default();
                let info = resolver.resolve_for_app("default").await?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&[&info])?);
                } else {
                    println!("📋 Configured registries:");
                    println!(
                        "   • {} ({})",
                        info.url,
                        format!("{:?}", info.source).to_lowercase()
                    );
                }
                Ok(())
            }
            crate::RegistryCommands::ClearCache => {
                let cache = crate::registry::RegistryCache::new();
                cache.clear()?;
                println!("✅ Registry cache cleared");
                Ok(())
            }
            crate::RegistryCommands::Serve {
                port,
                data_dir,
                host,
                auth_token,
            } => {
                if host != "127.0.0.1"
                    && auth_token
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or("")
                        .is_empty()
                {
                    anyhow::bail!("--auth-token is required when --host is not 127.0.0.1");
                }
                crate::registry::serve::serve(crate::registry::serve::RegistryServerConfig {
                    host,
                    port,
                    data_dir,
                    auth_token,
                })
                .await
            }
        }
    })
}

pub(crate) fn execute_source_sync_status_command(
    source_id: String,
    sync_run_id: String,
    registry: Option<String>,
    json: bool,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result = crate::commands::source::fetch_sync_run_status(
            &source_id,
            &sync_run_id,
            registry.as_deref(),
            json,
        )
        .await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Ok(())
    })
}

pub(crate) fn execute_source_rebuild_command(
    source_id: String,
    reference: Option<String>,
    wait: bool,
    registry: Option<String>,
    json: bool,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result = crate::commands::source::rebuild_source(
            &source_id,
            reference.as_deref(),
            wait,
            registry.as_deref(),
            json,
        )
        .await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Ok(())
    })
}

pub(crate) fn execute_search_command(args: SearchCommandArgs) -> Result<()> {
    if should_use_search_tui(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        args.json,
        args.no_tui,
    ) {
        let selected = crate::tui::run_search_tui(crate::tui::SearchTuiArgs {
            query: args.query.clone(),
            category: args.category.clone(),
            tags: args.tags.clone(),
            limit: args.limit,
            cursor: args.cursor.clone(),
            registry: args.registry.clone(),
            show_manifest: args.show_manifest,
        })?;
        if let Some(scoped_id) = selected {
            println!("{}", scoped_id);
        }
        return Ok(());
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result = crate::search::search_capsules(
            args.query.as_deref(),
            args.category.as_deref(),
            Some(args.tags.as_slice()),
            args.limit,
            args.cursor.as_deref(),
            args.registry.as_deref(),
        )
        .await?;

        if args.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            crate::commands::search::print_search_result(&result);
        }
        Ok(())
    })
}

pub(crate) fn should_use_search_tui(
    stdin_is_tty: bool,
    stdout_is_tty: bool,
    json: bool,
    no_tui: bool,
) -> bool {
    crate::tui::can_launch_tui(stdin_is_tty, stdout_is_tty) && !json && !no_tui
}
