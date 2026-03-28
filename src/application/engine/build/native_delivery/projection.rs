pub fn execute_project(
    derived_app_path: &Path,
    launcher_dir: Option<&Path>,
) -> Result<ProjectResult> {
    if !host_supports_projection() {
        bail!("ato project currently supports macOS, Linux, and Windows hosts only");
    }

    let launcher_dir = resolve_launcher_dir(launcher_dir)?;
    let metadata_root = projections_root()?;
    project_with_roots(derived_app_path, &launcher_dir, &metadata_root)
}

pub fn execute_project_ls() -> Result<ProjectionListResult> {
    if !host_supports_projection() {
        bail!("ato project ls currently supports macOS, Linux, and Windows hosts only");
    }

    list_projections(&projections_root()?)
}

pub fn execute_unproject(reference: &str) -> Result<UnprojectResult> {
    if !host_supports_projection() {
        bail!("ato unproject currently supports macOS, Linux, and Windows hosts only");
    }

    unproject_with_metadata_root(reference, &projections_root()?)
}

fn project_with_roots(
    derived_app_path: &Path,
    launcher_dir: &Path,
    metadata_root: &Path,
) -> Result<ProjectResult> {
    let projected_command_dir = resolve_projected_command_dir_for_host(launcher_dir)?;
    project_with_roots_and_command_dir(
        derived_app_path,
        launcher_dir,
        metadata_root,
        &projected_command_dir,
    )
}

fn project_with_roots_and_command_dir(
    derived_app_path: &Path,
    launcher_dir: &Path,
    metadata_root: &Path,
    projected_command_dir: &Path,
) -> Result<ProjectResult> {
    let derived_plan = DerivedProjectionPlan {
        launcher_dir: launcher_dir.to_path_buf(),
        metadata_root: metadata_root.to_path_buf(),
        projected_command_dir: projected_command_dir.to_path_buf(),
    };
    project_with_derived_plan(derived_app_path, &derived_plan)
}

fn project_with_derived_plan(
    derived_app_path: &Path,
    derived_plan: &DerivedProjectionPlan,
) -> Result<ProjectResult> {
    let source = load_projection_source(derived_app_path)?;
    fs::create_dir_all(&derived_plan.launcher_dir).with_context(|| {
        format!(
            "Failed to create launcher directory: {}",
            derived_plan.launcher_dir.display()
        )
    })?;
    fs::create_dir_all(&derived_plan.metadata_root).with_context(|| {
        format!(
            "Failed to create projection metadata directory: {}",
            derived_plan.metadata_root.display()
        )
    })?;

    let launcher_dir = absolute_path(&derived_plan.launcher_dir)?;
    let projected_command_dir = absolute_path(&derived_plan.projected_command_dir)?;
    let display_name =
        projection_display_name(&source.derived_app_path, source.scoped_id.as_deref())?;
    let command_name =
        projection_command_name(&source.derived_app_path, source.scoped_id.as_deref())?;
    let projected_path = projection_output_path(
        source.projection_kind,
        &launcher_dir,
        &source.derived_app_path,
        &command_name,
    )?;
    let projected_candidates = projection_candidate_paths(&projected_path);
    let projected_command_path = source
        .projected_command_target
        .as_ref()
        .map(|_| projected_command_dir.join(&command_name));

    let existing = load_projection_records(&derived_plan.metadata_root)?;
    for record in &existing {
        if paths_match(&record.metadata.derived_app_path, &source.derived_app_path)? {
            let status = inspect_projection(&record.metadata, &record.metadata_path)?;
            if status.state == "ok" {
                return Ok(ProjectResult {
                    projection_id: record.metadata.projection_id.clone(),
                    metadata_path: record.metadata_path.clone(),
                    launcher_dir: record.metadata.launcher_dir.clone(),
                    projected_path: record.metadata.projected_path.clone(),
                    derived_app_path: source.derived_app_path.clone(),
                    parent_digest: source.parent_digest.clone(),
                    derived_digest: source.derived_digest.clone(),
                    state: status.state,
                    problems: status.problems,
                    created: false,
                    schema_version: record.metadata.schema_version.clone(),
                });
            }
            bail!(
                "Derived app is already projected via '{}' (id {}). Use 'ato unproject' first.",
                record.metadata.projected_path.display(),
                record.metadata.projection_id
            );
        }
        let mut candidate_conflict = false;
        for candidate in &projected_candidates {
            if paths_match(&record.metadata.projected_path, candidate)? {
                candidate_conflict = true;
                break;
            }
        }
        if candidate_conflict {
            bail!(
                "Projection name conflict: '{}' is already managed by projection {}",
                record.metadata.projected_path.display(),
                record.metadata.projection_id
            );
        }
        if let (Some(existing_command_path), Some(projected_command_path)) = (
            record.metadata.projected_command_path.as_ref(),
            projected_command_path.as_ref(),
        ) {
            if paths_match(existing_command_path, projected_command_path)? {
                bail!(
                    "Projection command conflict: '{}' is already managed by projection {}",
                    projected_command_path.display(),
                    record.metadata.projection_id
                );
            }
        }
    }

    if source.projection_kind == ProjectionKind::Symlink {
        if let Some(existing_path) =
            find_existing_projection_path(&projected_path, &source.derived_app_path)?
        {
            let projection_id = build_projection_id(
                &source.derived_app_path,
                &existing_path,
                &source.derived_digest,
            );
            let metadata_path = derived_plan
                .metadata_root
                .join(format!("{}.json", projection_id));
            return Ok(ProjectResult {
                projection_id,
                metadata_path,
                launcher_dir,
                projected_path: existing_path,
                derived_app_path: source.derived_app_path.clone(),
                parent_digest: source.parent_digest.clone(),
                derived_digest: source.derived_digest.clone(),
                state: "ok".to_string(),
                problems: Vec::new(),
                created: false,
                schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            });
        }
    }

    if let Some(conflict_path) = first_existing_projection_candidate(&projected_path)? {
        bail!(
            "Projection name conflict: launcher path already exists: {}",
            conflict_path.display()
        );
    }
    if let (Some(projected_command_path), Some(projected_command_target)) = (
        projected_command_path.as_ref(),
        source.projected_command_target.as_ref(),
    ) {
        fs::create_dir_all(&projected_command_dir).with_context(|| {
            format!(
                "Failed to create projection command directory: {}",
                projected_command_dir.display()
            )
        })?;
        if (projected_command_path.exists() || fs::symlink_metadata(projected_command_path).is_ok())
            && !is_managed_projection_to(projected_command_path, projected_command_target)?
        {
            bail!(
                "Projection command conflict: command path already exists: {}",
                projected_command_path.display()
            );
        }
    }

    let source_projection_kind = source.projection_kind;
    let mut created_projected_path: Option<PathBuf> = None;
    let mut created_command_path = false;
    let mut written_metadata_path: Option<PathBuf> = None;
    let result = (|| -> Result<ProjectResult> {
        let projected_path = match source.projection_kind {
            ProjectionKind::Symlink => {
                let created = create_projection_symlink(&source.derived_app_path, &projected_path)
                    .with_context(|| {
                        format!(
                            "Failed to create projection {} -> {}",
                            projected_path.display(),
                            source.derived_app_path.display()
                        )
                    })?;
                created_projected_path = Some(created.clone());
                created
            }
            ProjectionKind::LinuxDesktopEntry => {
                let projected_command_path = projected_command_path
                    .as_ref()
                    .context("linux command path missing")?;
                let projected_command_target = source
                    .projected_command_target
                    .as_ref()
                    .context("linux command target missing")?;
                if !is_managed_projection_to(projected_command_path, projected_command_target)? {
                    create_projection_symlink(projected_command_target, projected_command_path)
                        .with_context(|| {
                            format!(
                                "Failed to create command symlink {} -> {}",
                                projected_command_path.display(),
                                projected_command_target.display()
                            )
                        })?;
                    created_command_path = true;
                }
                fs::write(
                    &projected_path,
                    render_linux_desktop_entry(
                        &display_name,
                        projected_command_path,
                        &source.derived_app_path,
                    ),
                )
                .with_context(|| {
                    format!("Failed to write desktop entry {}", projected_path.display())
                })?;
                created_projected_path = Some(projected_path.clone());
                projected_path.clone()
            }
        };
        let projection_id = build_projection_id(
            &source.derived_app_path,
            &projected_path,
            &source.derived_digest,
        );
        let metadata_path = derived_plan
            .metadata_root
            .join(format!("{}.json", projection_id));
        written_metadata_path = Some(metadata_path.clone());
        let metadata = ProjectionMetadata {
            schema_version: DELIVERY_SCHEMA_VERSION.to_string(),
            projection_id: projection_id.clone(),
            projection_kind: source.projection_kind.as_str().to_string(),
            projected_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            launcher_dir: launcher_dir.clone(),
            projected_path: projected_path.clone(),
            derived_app_path: source.derived_app_path.clone(),
            projected_command_path: projected_command_path.clone(),
            projected_command_target: source.projected_command_target.clone(),
            provenance_path: source.provenance_path.clone(),
            parent_digest: source.parent_digest.clone(),
            derived_digest: source.derived_digest.clone(),
            scoped_id: source.scoped_id.clone(),
            version: source.version.clone(),
            registry: source.registry.clone(),
            artifact_blake3: source.artifact_blake3.clone(),
            framework: source.framework.clone(),
            target: source.target.clone(),
            finalized_at: source.finalized_at.clone(),
        };
        write_json_pretty(&metadata_path, &metadata)?;
        let status = inspect_projection(&metadata, &metadata_path)?;
        Ok(ProjectResult {
            projection_id,
            metadata_path: metadata_path.clone(),
            launcher_dir,
            projected_path: projected_path.clone(),
            derived_app_path: source.derived_app_path,
            parent_digest: source.parent_digest,
            derived_digest: source.derived_digest,
            state: status.state,
            problems: status.problems,
            created: true,
            schema_version: metadata.schema_version.clone(),
        })
    })();

    if result.is_err() {
        if let Some(path) = created_projected_path.as_ref() {
            let _ = remove_projected_path(path, source_projection_kind.as_str());
        }
        if created_command_path {
            if let Some(projected_command_path) = projected_command_path.as_ref() {
                let _ = remove_projection_path(projected_command_path);
            }
        }
        if let Some(metadata_path) = written_metadata_path.as_ref() {
            let _ = fs::remove_file(metadata_path);
        }
    }
    result
}

fn list_projections(metadata_root: &Path) -> Result<ProjectionListResult> {
    let projections = load_projection_records(metadata_root)?
        .into_iter()
        .map(|record| inspect_projection(&record.metadata, &record.metadata_path))
        .collect::<Result<Vec<_>>>()?;
    let broken = projections
        .iter()
        .filter(|item| item.state == "broken")
        .count();
    Ok(ProjectionListResult {
        total: projections.len(),
        broken,
        projections,
    })
}

fn unproject_with_metadata_root(reference: &str, metadata_root: &Path) -> Result<UnprojectResult> {
    let record = find_projection_record(reference, metadata_root)?;
    let status = inspect_projection(&record.metadata, &record.metadata_path)?;
    let schema_version = record.metadata.schema_version.clone();

    let removed_projected_path = remove_projected_path(
        &record.metadata.projected_path,
        &record.metadata.projection_kind,
    )
    .with_context(|| {
        format!(
            "Failed to remove projected path: {}",
            record.metadata.projected_path.display()
        )
    })?;

    if let Some(projected_command_path) = record.metadata.projected_command_path.as_ref() {
        remove_projected_path(projected_command_path, PROJECTION_KIND_SYMLINK).with_context(
            || {
                format!(
                    "Failed to remove projection command path: {}",
                    projected_command_path.display()
                )
            },
        )?;
    }

    fs::remove_file(&record.metadata_path).with_context(|| {
        format!(
            "Failed to remove projection metadata: {}",
            record.metadata_path.display()
        )
    })?;

    Ok(UnprojectResult {
        projection_id: record.metadata.projection_id,
        metadata_path: record.metadata_path,
        projected_path: record.metadata.projected_path,
        removed_projected_path,
        removed_metadata: true,
        state_before: status.state,
        problems_before: status.problems,
        schema_version,
    })
}

fn load_projection_source(derived_app_path: &Path) -> Result<ProjectionSource> {
    let absolute_path = absolute_path(derived_app_path)?;
    let derived_dir = absolute_path.parent().ok_or_else(|| {
        anyhow::anyhow!("Projection input must be an ato finalize output with a parent directory")
    })?;
    let provenance_path = derived_dir.join(PROVENANCE_FILE);
    let raw = fs::read_to_string(&provenance_path).with_context(|| {
        format!(
            "ato project requires an ato finalize output containing {} next to the derived app: {}",
            PROVENANCE_FILE,
            provenance_path.display()
        )
    })?;
    let provenance: LocalDerivationProvenance = serde_json::from_str(&raw).with_context(|| {
        format!(
            "Failed to parse finalize provenance: {}",
            provenance_path.display()
        )
    })?;
    validate_delivery_schema(&provenance.schema_version, "local-derivation.json")?;
    if !provenance.finalized_locally {
        bail!("Projection input must be finalized locally via `ato finalize`");
    }
    if !supports_projection_target(&provenance.target) {
        bail!(
            "Projection input target '{}' is unsupported; expected a darwin/<arch>, linux/<arch>, or windows/<arch> target",
            provenance.target
        );
    }
    if !host_supports_projection_target(&provenance.target) {
        let expected_target = host_projection_os_family()
            .map(|family| format!("{family}/<arch>"))
            .unwrap_or_else(|| "darwin/<arch>, linux/<arch>, or windows/<arch>".to_string());
        bail!(
            "Projection input target '{}' is unsupported on this host; expected a {} target",
            provenance.target,
            expected_target
        );
    }
    let projection_kind = ProjectionKind::for_target(&provenance.target).ok_or_else(|| {
        anyhow::anyhow!(
            "Projection input target '{}' is unsupported",
            provenance.target
        )
    })?;
    validate_projection_input_shape(&absolute_path, &provenance.target, projection_kind)?;
    let derived_app_path = fs::canonicalize(&absolute_path).with_context(|| {
        format!(
            "Failed to canonicalize finalized app path: {}",
            absolute_path.display()
        )
    })?;
    let projected_command_target = match projection_kind {
        ProjectionKind::Symlink => None,
        ProjectionKind::LinuxDesktopEntry => {
            Some(resolve_linux_projection_command_target(&derived_app_path)?)
        }
    };

    let actual_digest = compute_tree_digest(&derived_app_path)?;
    if actual_digest != provenance.derived_digest {
        bail!(
            "Derived artifact digest mismatch: expected {}, got {}",
            provenance.derived_digest,
            actual_digest
        );
    }

    Ok(ProjectionSource {
        derived_app_path,
        provenance_path,
        projection_kind,
        projected_command_target,
        parent_digest: provenance.parent_digest,
        derived_digest: provenance.derived_digest,
        scoped_id: provenance.scoped_id,
        version: provenance.version,
        registry: provenance.registry,
        artifact_blake3: provenance.artifact_blake3,
        framework: provenance.framework,
        target: provenance.target,
        finalized_at: provenance.finalized_at,
    })
}

fn validate_projection_input_shape(
    path: &Path,
    target: &str,
    projection_kind: ProjectionKind,
) -> Result<()> {
    if !path.is_dir() {
        bail!(
            "Projection input must be a finalized directory artifact: {}",
            path.display()
        );
    }
    if projection_kind == ProjectionKind::Symlink
        && delivery_target_os_family(target) == Some("darwin")
        && path.extension().and_then(|ext| ext.to_str()) != Some("app")
    {
        bail!("Projection input must be a .app bundle: {}", path.display());
    }
    Ok(())
}

fn resolve_linux_projection_command_target(derived_app_path: &Path) -> Result<PathBuf> {
    let preferred = derived_app_path.join(
        derived_app_path
            .file_stem()
            .or_else(|| derived_app_path.file_name())
            .ok_or_else(|| anyhow::anyhow!("Derived app path has no terminal name"))?,
    );
    if preferred.is_file() && is_executable_file(&preferred)? {
        return Ok(preferred);
    }

    let mut candidates = Vec::new();
    for entry in WalkDir::new(derived_app_path)
        .min_depth(1)
        .max_depth(LINUX_PROJECTION_EXEC_SEARCH_MAX_DEPTH)
    {
        let entry = entry.with_context(|| {
            format!(
                "Failed to inspect projection command candidates in {}",
                derived_app_path.display()
            )
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if is_executable_file(&path)? {
            candidates.push(path);
        }
    }
    candidates.sort();
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => bail!(
            "Projection input is missing an executable command within {} levels of {}",
            LINUX_PROJECTION_EXEC_SEARCH_MAX_DEPTH,
            derived_app_path.display()
        ),
        _ => {
            let joined = candidates
                .iter()
                .map(|path| {
                    path.strip_prefix(derived_app_path)
                        .unwrap_or(path)
                        .display()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Projection input contains multiple executable command candidates within {} levels of {}: {}",
                LINUX_PROJECTION_EXEC_SEARCH_MAX_DEPTH,
                derived_app_path.display(),
                joined
            )
        }
    }
}

fn is_executable_file(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Failed to stat executable candidate {}", path.display()))?;
    if !metadata.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        Ok(metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        Ok(true)
    }
}

fn load_projection_records(metadata_root: &Path) -> Result<Vec<StoredProjection>> {
    if !metadata_root.exists() {
        return Ok(Vec::new());
    }

    let mut entries = fs::read_dir(metadata_root)
        .with_context(|| format!("Failed to read {}", metadata_root.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("Failed to enumerate {}", metadata_root.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut out = Vec::new();
    for entry in entries {
        let metadata_path = entry.path();
        if metadata_path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&metadata_path)
            .with_context(|| format!("Failed to read {}", metadata_path.display()))?;
        let metadata: ProjectionMetadata = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", metadata_path.display()))?;
        out.push(StoredProjection {
            metadata_path,
            metadata,
        });
    }
    Ok(out)
}

fn find_projection_record(reference: &str, metadata_root: &Path) -> Result<StoredProjection> {
    let records = load_projection_records(metadata_root)?;
    if records.is_empty() {
        bail!("No projection metadata found in {}", metadata_root.display());
    }

    let mut matches = Vec::new();
    let reference_path = PathBuf::from(reference);
    let reference_abs = absolute_path(&reference_path).ok();
    for record in records {
        if record.metadata.projection_id == reference {
            matches.push(record);
            continue;
        }
        if let Some(reference_abs) = reference_abs.as_ref() {
            if paths_match(reference_abs, &record.metadata.projected_path)?
                || paths_match(reference_abs, &record.metadata.derived_app_path)?
                || paths_match(reference_abs, &record.metadata_path)?
            {
                matches.push(record);
            }
        }
    }

    match matches.len() {
        0 => bail!("Projection not found: {}", reference),
        1 => Ok(matches.remove(0)),
        _ => bail!("Projection reference is ambiguous: {}", reference),
    }
}

fn inspect_projection(
    metadata: &ProjectionMetadata,
    metadata_path: &Path,
) -> Result<ProjectionStatus> {
    let mut problems = Vec::new();
    if !is_supported_delivery_schema(&metadata.schema_version) {
        problems.push(format!(
            "unsupported_schema_version:{}",
            metadata.schema_version
        ));
    }
    if !matches!(
        metadata.projection_kind.as_str(),
        PROJECTION_KIND_SYMLINK | PROJECTION_KIND_LINUX_DESKTOP_ENTRY
    ) {
        problems.push(format!(
            "unsupported_projection_kind:{}",
            metadata.projection_kind
        ));
    }
    if !supports_projection_target(&metadata.target) {
        problems.push(format!("unsupported_target:{}", metadata.target));
    }

    inspect_projected_path(metadata, &mut problems)?;

    if let Some(projected_command_path) = metadata.projected_command_path.as_ref() {
        let Some(projected_command_target) = metadata.projected_command_target.as_ref() else {
            problems.push("projected_command_target_missing".to_string());
            return finalize_projection_status(metadata, metadata_path, problems);
        };
        match inspect_projection_path(projected_command_path, projected_command_target)? {
            ProjectionPathStatus::MatchesTarget => {}
            ProjectionPathStatus::TargetMismatch => {
                problems.push("projected_command_target_mismatch".to_string())
            }
            ProjectionPathStatus::Replaced => {
                problems.push("projected_command_replaced".to_string())
            }
            ProjectionPathStatus::Missing => problems.push("projected_command_missing".to_string()),
        }
    } else if metadata.projection_kind == PROJECTION_KIND_LINUX_DESKTOP_ENTRY {
        problems.push("projected_command_missing".to_string());
    }

    if !metadata.derived_app_path.exists() {
        problems.push("derived_app_missing".to_string());
    } else if !metadata.derived_app_path.is_dir() {
        problems.push("derived_app_replaced".to_string());
    } else {
        let digest = compute_tree_digest(&metadata.derived_app_path)?;
        if digest != metadata.derived_digest {
            problems.push("derived_digest_mismatch".to_string());
        }
    }

    finalize_projection_status(metadata, metadata_path, problems)
}

fn inspect_projected_path(metadata: &ProjectionMetadata, problems: &mut Vec<String>) -> Result<()> {
    match metadata.projection_kind.as_str() {
        PROJECTION_KIND_SYMLINK => {
            match inspect_projection_path(&metadata.projected_path, &metadata.derived_app_path)? {
                ProjectionPathStatus::MatchesTarget => {}
                ProjectionPathStatus::TargetMismatch => {
                    problems.push("projected_symlink_target_mismatch".to_string())
                }
                ProjectionPathStatus::Replaced => problems.push("projected_path_replaced".to_string()),
                ProjectionPathStatus::Missing => problems.push("projected_path_missing".to_string()),
            }
            Ok(())
        }
        PROJECTION_KIND_LINUX_DESKTOP_ENTRY => inspect_linux_desktop_entry(metadata, problems),
        _ => Ok(()),
    }
}

fn inspect_linux_desktop_entry(
    metadata: &ProjectionMetadata,
    problems: &mut Vec<String>,
) -> Result<()> {
    match fs::symlink_metadata(&metadata.projected_path) {
        Ok(projected_meta) if projected_meta.is_file() => {
            let Some(projected_command_path) = metadata.projected_command_path.as_ref() else {
                problems.push("projected_command_missing".to_string());
                return Ok(());
            };
            let expected = render_linux_desktop_entry(
                &projection_display_name(
                    &metadata.derived_app_path,
                    metadata.scoped_id.as_deref(),
                )?,
                projected_command_path,
                &metadata.derived_app_path,
            );
            let actual = fs::read_to_string(&metadata.projected_path).with_context(|| {
                format!(
                    "Failed to read desktop entry: {}",
                    metadata.projected_path.display()
                )
            })?;
            if actual != expected {
                problems.push("projected_desktop_entry_mismatch".to_string());
            }
        }
        Ok(_) => problems.push("projected_path_replaced".to_string()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            problems.push("projected_path_missing".to_string())
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to inspect projected path: {}",
                    metadata.projected_path.display()
                )
            })
        }
    }
    Ok(())
}

fn finalize_projection_status(
    metadata: &ProjectionMetadata,
    metadata_path: &Path,
    problems: Vec<String>,
) -> Result<ProjectionStatus> {
    Ok(ProjectionStatus {
        projection_id: metadata.projection_id.clone(),
        metadata_path: metadata_path.to_path_buf(),
        launcher_dir: metadata.launcher_dir.clone(),
        projected_path: metadata.projected_path.clone(),
        derived_app_path: metadata.derived_app_path.clone(),
        parent_digest: metadata.parent_digest.clone(),
        derived_digest: metadata.derived_digest.clone(),
        state: if problems.is_empty() {
            "ok".to_string()
        } else {
            "broken".to_string()
        },
        problems,
        projected_at: metadata.projected_at.clone(),
        projection_kind: metadata.projection_kind.clone(),
        schema_version: metadata.schema_version.clone(),
    })
}

fn projections_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(PROJECTIONS_DIR))
}

fn resolve_launcher_dir(launcher_dir: Option<&Path>) -> Result<PathBuf> {
    match launcher_dir {
        Some(path) => absolute_path(path),
        None => Ok(dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(default_launcher_dir_for_host())),
    }
}

fn default_launcher_dir_for_host() -> &'static str {
    match host_projection_os_family() {
        Some("linux") => DEFAULT_LINUX_DESKTOP_ENTRY_DIR,
        _ => DEFAULT_MACOS_LAUNCHER_DIR,
    }
}

fn resolve_projected_command_dir_for_host(launcher_dir: &Path) -> Result<PathBuf> {
    match host_projection_os_family() {
        Some("linux") => Ok(dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(DEFAULT_LINUX_BIN_DIR)),
        _ => absolute_path(launcher_dir),
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("Failed to read current working directory")?
            .join(path))
    }
}

fn build_projection_id(
    derived_app_path: &Path,
    projected_path: &Path,
    derived_digest: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(derived_app_path.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(projected_path.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(derived_digest.as_bytes());
    hex::encode(&hasher.finalize().as_bytes()[..8])
}

fn paths_match(left: &Path, right: &Path) -> Result<bool> {
    if left == right {
        return Ok(true);
    }
    let left_canon = fs::canonicalize(left).ok();
    let right_canon = fs::canonicalize(right).ok();
    if let (Some(left_canon), Some(right_canon)) = (left_canon, right_canon) {
        return Ok(left_canon == right_canon);
    }
    Ok(absolute_path(left)? == absolute_path(right)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionPathStatus {
    MatchesTarget,
    TargetMismatch,
    Replaced,
    Missing,
}

fn projection_candidate_paths(path: &Path) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        vec![path.to_path_buf(), projection_shortcut_path(path)]
    }
    #[cfg(not(windows))]
    {
        vec![path.to_path_buf()]
    }
}

fn first_existing_projection_candidate(path: &Path) -> Result<Option<PathBuf>> {
    for candidate in projection_candidate_paths(path) {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => return Ok(Some(candidate)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("Failed to stat {}", candidate.display()))
            }
        }
    }
    Ok(None)
}

fn find_existing_projection_path(path: &Path, target: &Path) -> Result<Option<PathBuf>> {
    for candidate in projection_candidate_paths(path) {
        if is_managed_projection_to(&candidate, target)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn is_managed_projection_to(path: &Path, target: &Path) -> Result<bool> {
    Ok(matches!(
        inspect_projection_path(path, target)?,
        ProjectionPathStatus::MatchesTarget
    ))
}

fn inspect_projection_path(path: &Path, target: &Path) -> Result<ProjectionPathStatus> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(ProjectionPathStatus::Missing)
        }
        Err(err) => return Err(err).with_context(|| format!("Failed to stat {}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        let link_target = fs::read_link(path)
            .with_context(|| format!("Failed to read symlink {}", path.display()))?;
        let resolved_target = if link_target.is_absolute() {
            link_target
        } else {
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .join(link_target)
        };
        return Ok(if paths_match(&resolved_target, target)? {
            ProjectionPathStatus::MatchesTarget
        } else {
            ProjectionPathStatus::TargetMismatch
        });
    }

    #[cfg(windows)]
    {
        if junction::exists(path)
            .with_context(|| format!("Failed to inspect junction {}", path.display()))?
        {
            let junction_target = junction::get_target(path)
                .with_context(|| format!("Failed to read junction {}", path.display()))?;
            return Ok(if paths_match(&junction_target, target)? {
                ProjectionPathStatus::MatchesTarget
            } else {
                ProjectionPathStatus::TargetMismatch
            });
        }
        if is_projection_shortcut(path, &metadata) {
            let shortcut_target = resolve_projection_shortcut_target(path).with_context(|| {
                format!(
                    "Failed to validate projection shortcut target for {}",
                    path.display()
                )
            })?;
            return Ok(if paths_match(&shortcut_target, target)? {
                ProjectionPathStatus::MatchesTarget
            } else {
                ProjectionPathStatus::TargetMismatch
            });
        }
    }

    Ok(ProjectionPathStatus::Replaced)
}

fn remove_projection_path(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to inspect projected path: {}", path.display()))
        }
    };

    if metadata.file_type().is_symlink() {
        remove_projection_symlink(path)
            .with_context(|| format!("Failed to remove projection symlink: {}", path.display()))?;
        return Ok(true);
    }

    #[cfg(windows)]
    {
        if junction::exists(path)
            .with_context(|| format!("Failed to inspect junction {}", path.display()))?
        {
            junction::delete(path).with_context(|| {
                format!("Failed to remove projection junction: {}", path.display())
            })?;
            return Ok(true);
        }
        if is_projection_shortcut(path, &metadata) {
            fs::remove_file(path).with_context(|| {
                format!("Failed to remove projection shortcut: {}", path.display())
            })?;
            return Ok(true);
        }
    }

    bail!(
        "Refusing to remove unmanaged projected path: {}",
        path.display()
    )
}

#[cfg(unix)]
fn create_projection_symlink(target: &Path, destination: &Path) -> std::io::Result<PathBuf> {
    symlink(target, destination)?;
    Ok(destination.to_path_buf())
}

#[cfg(windows)]
fn create_projection_symlink(target: &Path, destination: &Path) -> std::io::Result<PathBuf> {
    match symlink_dir(target, destination) {
        Ok(()) => Ok(destination.to_path_buf()),
        Err(symlink_err) => match junction::create(target, destination) {
            Ok(()) => Ok(destination.to_path_buf()),
            Err(junction_err) => {
                let shortcut_path = projection_shortcut_path(destination);
                match create_projection_shortcut(target, &shortcut_path) {
                    Ok(()) => Ok(shortcut_path),
                    Err(shortcut_err) => Err(io::Error::new(
                        shortcut_err.kind(),
                        format!(
                            "Failed to create projection after attempting symlink, junction, and shortcut fallbacks: symlink failed: {}; junction failed: {}; shortcut failed: {}",
                            symlink_err, junction_err, shortcut_err
                        ),
                    )),
                }
            }
        },
    }
}

#[cfg(unix)]
fn remove_projection_symlink(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

#[cfg(windows)]
fn remove_projection_symlink(path: &Path) -> io::Result<()> {
    fs::remove_dir(path).or_else(|_| fs::remove_file(path))
}

#[cfg(windows)]
fn projection_shortcut_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "projection".to_string());
    path.with_file_name(format!("{file_name}.lnk"))
}

#[cfg(windows)]
fn create_projection_shortcut(target: &Path, destination: &Path) -> io::Result<()> {
    let shortcut = ShellLink::new(target).map_err(|err| {
        io::Error::other(format!(
            "Failed to prepare shortcut target {}: {}",
            target.display(),
            err
        ))
    })?;
    shortcut.create_lnk(destination).map_err(|err| {
        io::Error::other(format!(
            "Failed to write shortcut {}: {}",
            destination.display(),
            err
        ))
    })
}

#[cfg(windows)]
fn resolve_projection_shortcut_target(path: &Path) -> Result<PathBuf> {
    if !path.is_file() {
        bail!(
            "Projection shortcut does not exist as a file: {}",
            path.display()
        );
    }
    let output = powershell_command()
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ws = New-Object -ComObject WScript.Shell; $shortcut = $ws.CreateShortcut($args[0]); if (-not $shortcut.TargetPath) { exit 1 }; [Console]::Out.Write($shortcut.TargetPath)",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to resolve projection shortcut {}", path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to resolve projection shortcut {}: {}",
            path.display(),
            stderr.trim()
        );
    }
    let target = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(&['\r', '\n'][..])
        .to_string();
    if target.is_empty() {
        bail!("Projection shortcut target is empty: {}", path.display());
    }
    Ok(PathBuf::from(target))
}

#[cfg(windows)]
fn powershell_command() -> Command {
    if let Ok(system_root) = std::env::var("SYSTEMROOT") {
        let candidate = PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if candidate.is_file() {
            return Command::new(candidate);
        }
    }
    Command::new("powershell")
}

#[cfg(windows)]
fn is_projection_shortcut(path: &Path, metadata: &fs::Metadata) -> bool {
    metadata.is_file()
        && path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false)
}

fn host_supports_projection() -> bool {
    host_projection_os_family().is_some()
}

fn projection_output_path(
    projection_kind: ProjectionKind,
    launcher_dir: &Path,
    derived_app_path: &Path,
    command_name: &str,
) -> Result<PathBuf> {
    Ok(match projection_kind {
        ProjectionKind::Symlink => launcher_dir.join(
            derived_app_path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Derived app path has no terminal name"))?,
        ),
        ProjectionKind::LinuxDesktopEntry => launcher_dir.join(format!("{command_name}.desktop")),
    })
}

fn projection_display_name(derived_app_path: &Path, scoped_id: Option<&str>) -> Result<String> {
    let raw = derived_app_path
        .file_stem()
        .or_else(|| derived_app_path.file_name())
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            scoped_id
                .and_then(|value| value.rsplit('/').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| anyhow::anyhow!("Derived app path has no usable launcher name"))?;
    Ok(raw)
}

fn projection_command_name(derived_app_path: &Path, scoped_id: Option<&str>) -> Result<String> {
    let seed = scoped_id
        .and_then(|value| value.rsplit('/').next())
        .or_else(|| {
            derived_app_path
                .file_stem()
                .or_else(|| derived_app_path.file_name())
                .and_then(|value| value.to_str())
        })
        .ok_or_else(|| anyhow::anyhow!("Derived app path has no usable command name"))?;
    Ok(sanitize_projection_segment(seed))
}

fn sanitize_projection_segment(value: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;
    for ch in value.chars() {
        let normalized = if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if normalized == '-' {
            if !previous_dash {
                out.push('-');
            }
            previous_dash = true;
        } else {
            out.push(normalized);
            previous_dash = false;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "ato-app".to_string()
    } else {
        trimmed.to_string()
    }
}

fn render_linux_desktop_entry(
    display_name: &str,
    projected_command_path: &Path,
    derived_app_path: &Path,
) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={}\nExec={}\nPath={}\nTerminal=false\n",
        escape_desktop_entry_string_value(display_name),
        escape_desktop_entry_exec_value(projected_command_path),
        escape_desktop_entry_string_value(&derived_app_path.to_string_lossy()),
    )
}

fn escape_desktop_entry_string_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn escape_desktop_entry_exec_value(path: &Path) -> String {
    escape_desktop_entry_string_value(&path.to_string_lossy())
        .replace(' ', "\\ ")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn remove_projected_path(path: &Path, projection_kind: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(path)?;
            Ok(true)
        }
        Ok(metadata)
            if projection_kind == PROJECTION_KIND_LINUX_DESKTOP_ENTRY && metadata.is_file() =>
        {
            fs::remove_file(path)?;
            Ok(true)
        }
        Ok(_) => bail!(
            "Refusing to remove unexpected projected path: {}",
            path.display()
        ),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => {
            Err(err).with_context(|| format!("Failed to inspect projected path: {}", path.display()))
        }
    }
}
