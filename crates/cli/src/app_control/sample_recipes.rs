use std::path::PathBuf;

use anyhow::Result;

pub struct SampleRecipeBinding {
    pub slug: &'static str,
    pub display_name: &'static str,
    pub aliases: &'static [&'static str],
    pub github: Option<(&'static str, &'static str)>,
    pub manifest_content: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSampleRecipe {
    pub slug: String,
    pub display_name: String,
    pub manifest_path: PathBuf,
    pub canonical_handle: Option<String>,
}

static SAMPLE_RECIPE_CATALOG: &[SampleRecipeBinding] = &[
    SampleRecipeBinding {
        slug: "memos",
        display_name: "Memos",
        aliases: &["memos"],
        github: Some(("usememos", "memos")),
        manifest_content: include_str!("../../../../samples/recipes/memos/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "uptime-kuma",
        display_name: "Uptime Kuma",
        aliases: &["uptime-kuma", "uptimekuma"],
        github: Some(("louislam", "uptime-kuma")),
        manifest_content: include_str!("../../../../samples/recipes/uptime-kuma/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "n8n",
        display_name: "n8n",
        aliases: &["n8n"],
        github: Some(("n8n-io", "n8n")),
        manifest_content: include_str!("../../../../samples/recipes/n8n/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "open-webui",
        display_name: "Open WebUI",
        aliases: &["open-webui", "openwebui"],
        github: Some(("open-webui", "open-webui")),
        manifest_content: include_str!("../../../../samples/recipes/open-webui/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "excalidraw",
        display_name: "Excalidraw",
        aliases: &["excalidraw"],
        github: Some(("excalidraw", "excalidraw")),
        manifest_content: include_str!("../../../../samples/recipes/excalidraw/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "blinko",
        display_name: "Blinko",
        aliases: &["blinko"],
        github: Some(("blinkospace", "blinko")),
        manifest_content: include_str!("../../../../samples/recipes/blinko/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "affine",
        display_name: "AFFiNE",
        aliases: &["affine", "affine-pro"],
        github: Some(("toeverything", "AFFiNE")),
        manifest_content: include_str!("../../../../samples/recipes/affine/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "dify",
        display_name: "Dify",
        aliases: &["dify"],
        github: Some(("langgenius", "dify")),
        manifest_content: include_str!("../../../../samples/recipes/dify/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "pocketbase",
        display_name: "PocketBase",
        aliases: &["pocketbase", "pocket-base"],
        github: Some(("pocketbase", "pocketbase")),
        manifest_content: include_str!("../../../../samples/recipes/pocketbase/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "homepage",
        display_name: "Homepage",
        aliases: &["homepage", "gethomepage"],
        github: Some(("gethomepage", "homepage")),
        manifest_content: include_str!("../../../../samples/recipes/homepage/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "node-red",
        display_name: "Node-RED",
        aliases: &["node-red", "nodered"],
        github: Some(("node-red", "node-red")),
        manifest_content: include_str!("../../../../samples/recipes/node-red/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "fresh-rss",
        display_name: "FreshRSS",
        aliases: &["fresh-rss", "freshrss"],
        github: Some(("FreshRSS", "FreshRSS")),
        manifest_content: include_str!("../../../../samples/recipes/freshrss/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "filebrowser",
        display_name: "File Browser",
        aliases: &["filebrowser", "file-browser"],
        github: Some(("filebrowser", "filebrowser")),
        manifest_content: include_str!("../../../../samples/recipes/filebrowser/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "openlist-google-drive-crypt",
        display_name: "OpenList Google Drive Crypt",
        aliases: &[
            "openlist-google-drive-crypt",
            "openlist-gdrive-crypt",
            "openlist",
        ],
        github: Some(("openlistteam", "openlist")),
        manifest_content: include_str!(
            "../../../../samples/recipes/openlist-google-drive-crypt/capsule.toml"
        ),
    },
    SampleRecipeBinding {
        slug: "mailpit",
        display_name: "Mailpit",
        aliases: &["mailpit"],
        github: Some(("axllent", "mailpit")),
        manifest_content: include_str!("../../../../samples/recipes/mailpit/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "pgweb",
        display_name: "pgweb",
        aliases: &["pgweb"],
        github: Some(("sosedoff", "pgweb")),
        manifest_content: include_str!("../../../../samples/recipes/pgweb/capsule.toml"),
    },
    SampleRecipeBinding {
        slug: "adminer",
        display_name: "Adminer",
        aliases: &["adminer"],
        github: Some(("vrana", "adminer")),
        manifest_content: include_str!("../../../../samples/recipes/adminer/capsule.toml"),
    },
];

fn materialize_recipe(binding: &SampleRecipeBinding) -> Result<PathBuf> {
    let root = capsule::common::paths::ato_path_or_workspace_tmp("sample-recipes");
    let dir = root.join(binding.slug);
    let manifest_path = dir.join("capsule.toml");

    std::fs::create_dir_all(&dir)?;

    let should_write = if manifest_path.exists() {
        let existing = std::fs::read_to_string(&manifest_path).unwrap_or_default();
        existing != binding.manifest_content
    } else {
        true
    };

    if should_write {
        std::fs::write(&manifest_path, binding.manifest_content)?;
    }

    Ok(manifest_path)
}

fn resolve_for_alias(input: &str) -> Option<&'static SampleRecipeBinding> {
    let normalized = input.trim().to_ascii_lowercase();
    SAMPLE_RECIPE_CATALOG.iter().find(|binding| {
        binding.slug == normalized || binding.aliases.iter().any(|alias| *alias == normalized)
    })
}

fn resolve_for_github(owner: &str, repo: &str) -> Option<&'static SampleRecipeBinding> {
    let owner_lower = owner.to_ascii_lowercase();
    let repo_lower = repo.to_ascii_lowercase();
    SAMPLE_RECIPE_CATALOG.iter().find(|binding| {
        binding.github.is_some_and(|(gh_owner, gh_repo)| {
            gh_owner.eq_ignore_ascii_case(&owner_lower) && gh_repo.eq_ignore_ascii_case(&repo_lower)
        })
    })
}

pub fn resolve_sample_recipe_for_input(input: &str) -> Result<Option<ResolvedSampleRecipe>> {
    let binding = match resolve_for_alias(input) {
        Some(b) => b,
        None => return Ok(None),
    };
    let manifest_path = materialize_recipe(binding)?;
    Ok(Some(ResolvedSampleRecipe {
        slug: binding.slug.to_string(),
        display_name: binding.display_name.to_string(),
        manifest_path,
        canonical_handle: binding
            .github
            .map(|(owner, repo)| format!("capsule://github.com/{owner}/{repo}")),
    }))
}

pub fn resolve_sample_recipe_for_github(
    owner: &str,
    repo: &str,
) -> Result<Option<ResolvedSampleRecipe>> {
    let binding = match resolve_for_github(owner, repo) {
        Some(b) => b,
        None => return Ok(None),
    };
    let manifest_path = materialize_recipe(binding)?;
    Ok(Some(ResolvedSampleRecipe {
        slug: binding.slug.to_string(),
        display_name: binding.display_name.to_string(),
        manifest_path,
        canonical_handle: Some(format!("capsule://github.com/{owner}/{repo}")),
    }))
}

#[allow(dead_code)]
pub fn is_sample_recipe_alias(input: &str) -> bool {
    resolve_for_alias(input).is_some()
}

#[allow(dead_code)]
pub fn is_sample_recipe_github(owner: &str, repo: &str) -> bool {
    resolve_for_github(owner, repo).is_some()
}

#[allow(dead_code)]
pub fn materialize_all_sample_recipes() -> Result<()> {
    for binding in SAMPLE_RECIPE_CATALOG {
        materialize_recipe(binding)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_memos_by_alias() {
        let result = resolve_sample_recipe_for_input("memos")
            .expect("no error")
            .expect("memos alias");
        assert_eq!(result.slug, "memos");
        assert!(result.manifest_path.exists());
        let content = std::fs::read_to_string(&result.manifest_path).unwrap();
        assert!(content.contains("neosmemo/memos"));
        assert_eq!(
            result.canonical_handle.as_deref(),
            Some("capsule://github.com/usememos/memos")
        );
    }

    #[test]
    fn resolves_uptime_kuma_by_alias() {
        let result = resolve_sample_recipe_for_input("uptime-kuma")
            .expect("no error")
            .expect("uptime-kuma");
        assert_eq!(result.slug, "uptime-kuma");
        assert!(result.manifest_path.exists());
    }

    #[test]
    fn resolves_memos_by_github_handle() {
        let result = resolve_sample_recipe_for_github("usememos", "memos")
            .expect("no error")
            .expect("github memos");
        assert_eq!(result.slug, "memos");
        assert!(result.manifest_path.exists());
    }

    #[test]
    fn resolves_open_webui_by_github_handle() {
        let result = resolve_sample_recipe_for_github("open-webui", "open-webui")
            .expect("no error")
            .expect("github open-webui");
        assert_eq!(result.slug, "open-webui");
    }

    #[test]
    fn resolves_blinko_by_alias_and_github_handle() {
        let alias = resolve_sample_recipe_for_input("blinko")
            .expect("no error")
            .expect("blinko alias");
        assert_eq!(alias.slug, "blinko");

        let github = resolve_sample_recipe_for_github("blinkospace", "blinko")
            .expect("no error")
            .expect("github blinko");
        assert_eq!(github.slug, "blinko");
    }

    #[test]
    fn resolves_affine_and_dify_by_github_handle() {
        let affine = resolve_sample_recipe_for_github("toeverything", "AFFiNE")
            .expect("no error")
            .expect("github affine");
        assert_eq!(affine.slug, "affine");

        let dify = resolve_sample_recipe_for_github("langgenius", "dify")
            .expect("no error")
            .expect("github dify");
        assert_eq!(dify.slug, "dify");
    }

    #[test]
    fn resolves_openlist_google_drive_crypt_by_alias_and_github_handle() {
        let alias = resolve_sample_recipe_for_input("openlist-gdrive-crypt")
            .expect("no error")
            .expect("openlist alias");
        assert_eq!(alias.slug, "openlist-google-drive-crypt");

        let github = resolve_sample_recipe_for_github("openlistteam", "openlist")
            .expect("no error")
            .expect("github openlist");
        assert_eq!(github.slug, "openlist-google-drive-crypt");
    }

    #[test]
    fn unknown_alias_returns_none() {
        assert!(
            resolve_sample_recipe_for_input("unknown-app")
                .expect("no error")
                .is_none()
        );
    }

    #[test]
    fn unknown_github_returns_none() {
        assert!(
            resolve_sample_recipe_for_github("unknown", "repo")
                .expect("no error")
                .is_none()
        );
    }

    #[test]
    fn is_alias_check() {
        assert!(is_sample_recipe_alias("memos"));
        assert!(is_sample_recipe_alias("n8n"));
        assert!(!is_sample_recipe_alias("unknown-app"));
    }

    #[test]
    fn is_github_check() {
        assert!(is_sample_recipe_github("usememos", "memos"));
        assert!(!is_sample_recipe_github("unknown", "repo"));
    }

    #[test]
    fn case_insensitive_alias() {
        assert!(
            resolve_sample_recipe_for_input("Memos")
                .expect("no error")
                .is_some()
        );
        assert!(
            resolve_sample_recipe_for_input("N8N")
                .expect("no error")
                .is_some()
        );
    }

    #[test]
    fn case_insensitive_github() {
        assert!(
            resolve_sample_recipe_for_github("Usememos", "Memos")
                .expect("no error")
                .is_some()
        );
    }

    #[test]
    fn materialized_content_matches_embedded() {
        // Materializes under an ATO_HOME-derived path and reads it back;
        // serialize against env-mutating tests or the path moves mid-test.
        let _env = crate::tests::env_lock().lock().unwrap();
        let result = resolve_sample_recipe_for_input("memos")
            .expect("no error")
            .expect("memos alias");
        let embedded = include_str!("../../../../samples/recipes/memos/capsule.toml");
        let on_disk = std::fs::read_to_string(&result.manifest_path).unwrap();
        assert_eq!(on_disk, embedded);
    }

    #[test]
    fn materialize_all_writes_all_recipes() {
        let _env = crate::tests::env_lock().lock().unwrap();
        materialize_all_sample_recipes().expect("materialize all");
        for binding in SAMPLE_RECIPE_CATALOG {
            let root = capsule::common::paths::ato_path_or_workspace_tmp("sample-recipes");
            let manifest_path = root.join(binding.slug).join("capsule.toml");
            assert!(
                manifest_path.exists(),
                "materialized manifest should exist for {}",
                binding.slug
            );
        }
    }

    #[test]
    fn catalog_manifests_are_publishable_to_community() {
        for binding in SAMPLE_RECIPE_CATALOG {
            let manifest: toml::Value = toml::from_str(binding.manifest_content)
                .unwrap_or_else(|err| panic!("{} manifest parses: {err}", binding.slug));

            let source_repo = manifest
                .get("source")
                .and_then(|source| source.get("repository"))
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("{} has [source].repository", binding.slug));
            let expected_repo = binding
                .github
                .map(|(owner, repo)| format!("{owner}/{repo}"))
                .expect("catalog entries are public GitHub recipes");
            assert_eq!(
                source_repo, expected_repo,
                "{} source repository",
                binding.slug
            );

            assert_valid_schema_ids(binding.slug, &manifest);
        }
    }

    fn assert_valid_schema_ids(slug: &str, value: &toml::Value) {
        match value {
            toml::Value::String(value) if value.starts_with("sha256:") => {
                let hash = &value["sha256:".len()..];
                assert_eq!(hash.len(), 64, "{slug} schema_id length: {value}");
                assert!(
                    hash.bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
                    "{slug} schema_id must be lowercase hex: {value}"
                );
            }
            toml::Value::Array(values) => {
                for value in values {
                    assert_valid_schema_ids(slug, value);
                }
            }
            toml::Value::Table(values) => {
                for value in values.values() {
                    assert_valid_schema_ids(slug, value);
                }
            }
            _ => {}
        }
    }
}
