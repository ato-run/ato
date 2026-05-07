use super::*;

pub(crate) fn unpack_payload_tar(payload_tar: &[u8], destination: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(Cursor::new(payload_tar));
    let entries = archive
        .entries()
        .context("Failed to read payload.tar entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid payload.tar entry")?;
        let path = entry.path().context("Failed to read payload entry path")?;
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            bail!("Refusing to unpack unsafe payload path: {}", path.display());
        }
        entry.unpack_in(destination).with_context(|| {
            format!(
                "Failed to unpack payload entry into {}",
                destination.display()
            )
        })?;
    }
    Ok(())
}

pub(crate) fn compute_tree_digest(root: &Path) -> Result<String> {
    if !root.exists() {
        bail!("Digest root does not exist: {}", root.display());
    }
    let mut hasher = blake3::Hasher::new();
    hash_tree_node(root, Path::new(""), &mut hasher)?;
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

pub(crate) fn hash_tree_node(
    path: &Path,
    relative: &Path,
    hasher: &mut blake3::Hasher,
) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_dir() {
        if !relative.as_os_str().is_empty() {
            update_tree_header(hasher, b"dir", relative, mode_bits(&metadata));
        }
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("Failed to read directory {}", path.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("Failed to enumerate directory {}", path.display()))?;
        entries.sort_by_key(|left| left.file_name());
        for entry in entries {
            let child_path = entry.path();
            let child_relative = if relative.as_os_str().is_empty() {
                PathBuf::from(entry.file_name())
            } else {
                relative.join(entry.file_name())
            };
            hash_tree_node(&child_path, &child_relative, hasher)?;
        }
        return Ok(());
    }

    if file_type.is_symlink() {
        update_tree_header(hasher, b"symlink", relative, 0);
        let target = fs::read_link(path)
            .with_context(|| format!("Failed to read symlink {}", path.display()))?;
        hasher.update(target.as_os_str().to_string_lossy().as_bytes());
        hasher.update(b"\0");
        return Ok(());
    }

    if file_type.is_file() {
        update_tree_header(hasher, b"file", relative, mode_bits(&metadata));
        hasher.update(format!("{}\0", metadata.len()).as_bytes());
        let mut file =
            fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hasher.update(b"\0");
        return Ok(());
    }

    bail!(
        "Unsupported filesystem entry in digest walk: {}",
        path.display()
    )
}

pub(crate) fn update_tree_header(
    hasher: &mut blake3::Hasher,
    kind: &[u8],
    relative: &Path,
    mode: u32,
) {
    hasher.update(kind);
    hasher.update(b"\0");
    hasher.update(relative.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{:o}", mode).as_bytes());
    hasher.update(b"\0");
}

pub(crate) fn copy_recursively(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("Failed to stat {}", source.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("Failed to create directory {}", destination.display()))?;
        fs::set_permissions(destination, metadata.permissions())
            .with_context(|| format!("Failed to set permissions on {}", destination.display()))?;
        let mut entries = fs::read_dir(source)
            .with_context(|| format!("Failed to read directory {}", source.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("Failed to enumerate directory {}", source.display()))?;
        entries.sort_by_key(|left| left.file_name());
        for entry in entries {
            copy_recursively(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }

    if file_type.is_symlink() {
        #[cfg(unix)]
        {
            let target = fs::read_link(source)
                .with_context(|| format!("Failed to read symlink {}", source.display()))?;
            symlink(&target, destination).with_context(|| {
                format!(
                    "Failed to recreate symlink {} -> {}",
                    destination.display(),
                    target.display()
                )
            })?;
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            let _ = destination;
            bail!("Symlink copy is not supported on this platform")
        }
    }

    if file_type.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory {}", parent.display())
            })?;
        }
        fs::copy(source, destination).with_context(|| {
            format!(
                "Failed to copy file {} -> {}",
                source.display(),
                destination.display()
            )
        })?;
        fs::set_permissions(destination, metadata.permissions())
            .with_context(|| format!("Failed to set permissions on {}", destination.display()))?;
        return Ok(());
    }

    bail!(
        "Unsupported filesystem entry for copy: {}",
        source.display()
    )
}

pub(crate) fn ensure_tree_writable(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let file_type = metadata.file_type();

    if !file_type.is_symlink() {
        #[cfg(unix)]
        {
            let mode = metadata.permissions().mode();
            if mode & 0o200 == 0 {
                let mut permissions = metadata.permissions();
                permissions.set_mode(mode | 0o200);
                fs::set_permissions(path, permissions)
                    .with_context(|| format!("Failed to set permissions on {}", path.display()))?;
            }
        }

        #[cfg(windows)]
        {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                permissions.set_readonly(false);
                fs::set_permissions(path, permissions)
                    .with_context(|| format!("Failed to set permissions on {}", path.display()))?;
            }
        }
    }

    if file_type.is_dir() {
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("Failed to read directory {}", path.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("Failed to enumerate directory {}", path.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            ensure_tree_writable(&entry.path())?;
        }
    }

    Ok(())
}

pub(crate) fn validate_minimal_native_artifact_permissions(path: &Path) -> Result<()> {
    match NativeArtifactKind::from_path(path) {
        NativeArtifactKind::MacOsAppBundle => validate_minimal_macos_app_permissions(path),
        NativeArtifactKind::File if path_has_extension(path, "exe") => {
            validate_minimal_windows_executable(path)
        }
        NativeArtifactKind::File if path_has_extension(path, "deb") => Ok(()),
        NativeArtifactKind::File => validate_minimal_linux_elf_file(path),
        NativeArtifactKind::Directory => validate_minimal_linux_elf_directory(path),
    }
}

pub(crate) fn validate_minimal_macos_app_permissions(app_dir: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }

    let macos_dir = app_dir.join("Contents").join("MacOS");
    if !macos_dir.is_dir() {
        return Ok(());
    }

    let mut found_regular_file = false;
    for entry in fs::read_dir(&macos_dir)
        .with_context(|| format!("Failed to read directory {}", macos_dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("Failed to enumerate directory {}", macos_dir.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to stat {}", path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        found_regular_file = true;
        validate_unix_executable_permissions(&path, &metadata)?;
    }

    if !found_regular_file {
        bail!(
            "Finalize input is missing a regular executable in {}",
            macos_dir.display()
        );
    }

    Ok(())
}

pub(crate) fn validate_minimal_linux_elf_directory(root: &Path) -> Result<()> {
    let mut found_regular_elf = false;
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.into_path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to stat {}", path.display()))?;
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o111 == 0 {
            continue;
        }

        if !path_has_elf_magic(&path)? {
            continue;
        }

        validate_unix_executable_permissions(&path, &metadata)?;
        let bytes = fs::read(&path)
            .with_context(|| format!("Failed to read Linux executable {}", path.display()))?;
        validate_linux_elf_bytes(&path, &bytes)?;
        found_regular_elf = true;
    }

    if !found_regular_elf {
        bail!(
            "Native delivery input is missing a regular ELF executable in {}",
            root.display()
        );
    }

    Ok(())
}

pub(crate) fn validate_minimal_linux_elf_file(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    validate_unix_executable_permissions(path, &metadata)?;
    let bytes = fs::read(path)
        .with_context(|| format!("Failed to read Linux executable {}", path.display()))?;
    validate_linux_elf_bytes(path, &bytes)
}

pub(crate) fn validate_linux_elf_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let object = Object::parse(bytes).with_context(|| {
        format!(
            "Linux executable failed minimum ELF validation: {}",
            path.display()
        )
    })?;
    let Object::Elf(_) = object else {
        bail!(
            "Linux executable failed minimum ELF validation: {} is not an ELF image",
            path.display()
        );
    };
    Ok(())
}

pub(crate) fn path_has_elf_magic(path: &Path) -> Result<bool> {
    let mut file =
        fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut magic = [0u8; 4];
    let bytes_read = file
        .read(&mut magic)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    Ok(bytes_read == magic.len() && magic == *b"\x7FELF")
}

pub(crate) fn native_file_candidate_extension(path: &Path) -> Option<&'static str> {
    if path_has_extension(path, "exe") {
        Some("exe")
    } else if path_has_extension(path, "AppImage") {
        Some("AppImage")
    } else if path_has_extension(path, "deb") {
        Some("deb")
    } else {
        None
    }
}

pub(crate) fn native_file_candidate_label(path: &Path) -> Option<&'static str> {
    match native_file_candidate_extension(path) {
        Some("exe") => Some(".exe"),
        Some("AppImage") => Some(".AppImage"),
        Some("deb") => Some(".deb"),
        Some(_) | None => None,
    }
}

#[cfg(unix)]
pub(crate) fn validate_unix_executable_permissions(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<()> {
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        bail!(
            "Executable bit is missing for {} (mode {:o})",
            path.display(),
            mode & 0o777
        );
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn validate_unix_executable_permissions(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<()> {
    Ok(())
}

pub(crate) fn validate_minimal_windows_executable(path: &Path) -> Result<()> {
    if !path_has_extension(path, "exe") {
        return Ok(());
    }

    let bytes = fs::read(path)
        .with_context(|| format!("Failed to read Windows executable {}", path.display()))?;
    let object = Object::parse(&bytes).with_context(|| {
        format!(
            "Windows executable failed minimum PE validation: {}",
            path.display()
        )
    })?;
    let Object::PE(pe) = object else {
        bail!(
            "Windows executable failed minimum PE validation: {} is not a PE image",
            path.display()
        );
    };
    if pe.is_lib {
        bail!(
            "Windows executable failed minimum PE validation: {} is a DLL, not an .exe",
            path.display()
        );
    }

    Ok(())
}

pub(crate) fn path_has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

pub(crate) fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("Failed to serialize JSON")?;
    let mut file =
        fs::File::create(path).with_context(|| format!("Failed to create {}", path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("Failed to finalize {}", path.display()))?;
    Ok(())
}

pub(crate) fn append_tar_entry(
    builder: &mut tar::Builder<&mut Vec<u8>>,
    path: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, path, Cursor::new(bytes))?;
    Ok(())
}

pub(crate) fn build_capsule_archive(
    manifest: &capsule_core::types::CapsuleManifest,
    payload_tar_zst: &[u8],
    payload_tar: &[u8],
    capsule_lock_json: Option<&str>,
) -> Result<Vec<u8>> {
    let (_distribution_manifest, manifest_toml_bytes) =
        capsule_core::packers::payload::build_distribution_manifest(manifest, payload_tar)
            .map_err(anyhow::Error::from)
            .context("Failed to build distribution metadata for native capsule")?;
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        append_tar_entry(&mut builder, "capsule.toml", &manifest_toml_bytes, 0o644)?;
        if let Some(capsule_lock_json) = capsule_lock_json {
            append_tar_entry(
                &mut builder,
                "capsule.lock.json",
                capsule_lock_json.as_bytes(),
                0o644,
            )?;
        }
        append_tar_entry(&mut builder, "payload.tar.zst", payload_tar_zst, 0o644)?;
        builder.finish()?;
    }
    Ok(out)
}

pub(crate) fn create_payload_tar_from_directory(root: &Path) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        append_tree_to_tar(root, root, &mut builder)?;
        builder.finish()?;
    }
    Ok(out)
}

pub(crate) fn append_tree_to_tar(
    root: &Path,
    path: &Path,
    builder: &mut tar::Builder<&mut Vec<u8>>,
) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let file_type = metadata.file_type();

    if file_type.is_dir() {
        if !relative.is_empty() {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(mode_bits(&metadata));
            header.set_size(0);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            builder.append_data(&mut header, format!("{relative}/"), std::io::empty())?;
        }
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("Failed to read directory {}", path.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("Failed to enumerate directory {}", path.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            append_tree_to_tar(root, &entry.path(), builder)?;
        }
        return Ok(());
    }

    if file_type.is_symlink() {
        let target = fs::read_link(path)
            .with_context(|| format!("Failed to read symlink {}", path.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_link_name(&target)?;
        header.set_cksum();
        builder.append_data(&mut header, &relative, std::io::empty())?;
        return Ok(());
    }

    if file_type.is_file() {
        let mut header = tar::Header::new_gnu();
        header.set_mode(mode_bits(&metadata));
        header.set_size(metadata.len());
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        let mut file =
            fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
        builder.append_data(&mut header, &relative, &mut file)?;
        return Ok(());
    }

    bail!(
        "Unsupported filesystem entry for tar payload: {}",
        path.display()
    )
}

pub(crate) fn create_unique_output_dir(output_root: &Path) -> Result<PathBuf> {
    for _ in 0..32 {
        let candidate = output_root.join(format!(
            "derived-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            random_hex(4)
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to create {}", candidate.display()))
            }
        }
    }
    bail!("Failed to allocate unique finalize output directory")
}

pub(crate) fn create_temp_subdir(root: &Path, prefix: &str) -> Result<PathBuf> {
    for _ in 0..32 {
        let candidate = root.join(format!("{}-{}", prefix, random_hex(8)));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to create {}", candidate.display()))
            }
        }
    }
    bail!(
        "Failed to allocate temporary directory in {}",
        root.display()
    )
}

pub(crate) fn digest_dir_name(digest: &str) -> Result<String> {
    let normalized = digest
        .trim()
        .trim_start_matches("blake3:")
        .trim_start_matches("sha256:")
        .to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("Digest label is empty");
    }
    Ok(normalized)
}

pub(crate) fn fetches_root() -> Result<PathBuf> {
    Ok(capsule_core::common::paths::ato_path_or_workspace_tmp(
        "fetches",
    ))
}

pub(crate) fn derived_apps_root(scoped_id: &str, parent_digest: &str) -> Result<PathBuf> {
    let mut root = capsule_core::common::paths::ato_path_or_workspace_tmp("apps");
    for segment in scoped_id.split('/') {
        root.push(segment.trim());
    }
    root.push(digest_dir_name(parent_digest)?);
    Ok(root)
}

pub(crate) fn default_native_artifact_path(
    manifest_dir: &Path,
    name: &str,
    version: &str,
) -> PathBuf {
    manifest_dir
        .join("dist")
        .join(format!("{}-{}.capsule", name, version))
}
pub(crate) fn host_supports_finalize() -> bool {
    cfg!(target_os = "macos") || cfg!(windows)
}

pub(crate) fn random_hex(len_bytes: usize) -> String {
    let mut bytes = vec![0u8; len_bytes];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    hex::encode(bytes)
}

#[cfg(unix)]
pub(crate) fn mode_bits(metadata: &fs::Metadata) -> u32 {
    metadata.permissions().mode()
}

#[cfg(not(unix))]
pub(crate) fn mode_bits(_metadata: &fs::Metadata) -> u32 {
    0
}
