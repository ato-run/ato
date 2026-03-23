use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::adapters::install::source::local::LocalArtifactSource;
use crate::adapters::install::target::test_temp::{publish_test_sandbox_spec, TestSandboxTarget};
use crate::application::ports::install::{
    InstalledEnvironment, SharedSourcePort, SharedTargetPort, SourceSpec, TargetSpec,
};

#[derive(Debug, Clone)]
pub struct InstallPhaseRequest {
    pub source_spec: SourceSpec,
    pub target_spec: TargetSpec,
}

pub struct InstallPhase {
    source: SharedSourcePort,
    target: SharedTargetPort,
}

impl InstallPhase {
    pub fn new(source: SharedSourcePort, target: SharedTargetPort) -> Self {
        Self { source, target }
    }

    pub async fn execute(&self, request: &InstallPhaseRequest) -> Result<InstalledEnvironment> {
        let artifact = self.source.fetch(&request.source_spec).await?;
        self.target.unpack(artifact, &request.target_spec).await
    }
}

pub async fn install_local_artifact_into_test_sandbox(
    artifact_path: PathBuf,
    scoped_id: &str,
    version: &str,
) -> Result<InstalledEnvironment> {
    let cwd = std::env::current_dir().context("failed to resolve current directory")?;
    let request = InstallPhaseRequest {
        source_spec: SourceSpec::LocalArtifact {
            path: artifact_path,
        },
        target_spec: publish_test_sandbox_spec(&cwd, scoped_id, version),
    };
    let phase = InstallPhase::new(Arc::new(LocalArtifactSource), Arc::new(TestSandboxTarget));
    phase.execute(&request).await
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::{InstallPhase, InstallPhaseRequest};
    use crate::application::ports::install::{
        InstalledEnvironment, SourceArtifact, SourcePort, SourceSpec, TargetPort, TargetSpec,
    };

    #[derive(Debug)]
    struct StubSource;

    #[async_trait]
    impl SourcePort for StubSource {
        async fn fetch(&self, spec: &SourceSpec) -> anyhow::Result<SourceArtifact> {
            Ok(SourceArtifact {
                bytes: b"capsule".to_vec(),
                source: spec.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct StubTarget;

    #[async_trait]
    impl TargetPort for StubTarget {
        async fn unpack(
            &self,
            artifact: SourceArtifact,
            spec: &TargetSpec,
        ) -> anyhow::Result<InstalledEnvironment> {
            std::fs::create_dir_all(spec.root_dir())?;
            std::fs::write(spec.root_dir().join("artifact.bin"), artifact.bytes)?;
            Ok(InstalledEnvironment {
                root_dir: spec.root_dir().to_path_buf(),
                source: artifact.source,
            })
        }
    }

    fn build_test_capsule() -> Vec<u8> {
        let mut payload = Vec::new();
        {
            let mut payload_builder = tar::Builder::new(&mut payload);
            let bytes = b"hello from payload";
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            payload_builder
                .append_data(
                    &mut header,
                    "source/hello.txt",
                    Cursor::new(bytes.as_slice()),
                )
                .expect("payload append");
            payload_builder.finish().expect("payload finish");
        }
        let payload_zst =
            zstd::stream::encode_all(Cursor::new(payload), 1).expect("encode payload");

        let mut capsule = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut capsule);
            let manifest = b"schema_version = \"0.2\"\nname = \"demo\"\nversion = \"0.1.0\"\n";
            let mut manifest_header = tar::Header::new_gnu();
            manifest_header.set_size(manifest.len() as u64);
            manifest_header.set_mode(0o644);
            manifest_header.set_cksum();
            builder
                .append_data(
                    &mut manifest_header,
                    "capsule.toml",
                    Cursor::new(manifest.as_slice()),
                )
                .expect("manifest append");

            let mut payload_header = tar::Header::new_gnu();
            payload_header.set_size(payload_zst.len() as u64);
            payload_header.set_mode(0o644);
            payload_header.set_cksum();
            builder
                .append_data(
                    &mut payload_header,
                    "payload.tar.zst",
                    Cursor::new(payload_zst),
                )
                .expect("payload append");
            builder.finish().expect("capsule finish");
        }
        capsule
    }

    #[tokio::test]
    async fn install_phase_fetches_then_unpacks_into_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let request = InstallPhaseRequest {
            source_spec: SourceSpec::LocalArtifact {
                path: dir.path().join("demo.capsule"),
            },
            target_spec: TargetSpec::TestSandbox {
                root_dir: dir.path().join("sandbox"),
            },
        };
        let phase = InstallPhase::new(Arc::new(StubSource), Arc::new(StubTarget));

        let env = phase.execute(&request).await.expect("execute");

        assert_eq!(env.root_dir, dir.path().join("sandbox"));
        assert_eq!(
            std::fs::read(env.root_dir.join("artifact.bin")).expect("artifact"),
            b"capsule"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn helper_unpacks_capsule_into_publish_test_sandbox() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact_path = dir.path().join("demo.capsule");
        std::fs::write(&artifact_path, build_test_capsule()).expect("artifact");
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("set cwd");

        let env = super::install_local_artifact_into_test_sandbox(
            artifact_path,
            "capsules/demo",
            "0.1.0",
        )
        .await
        .expect("install");

        std::env::set_current_dir(original_dir).expect("restore cwd");
        assert!(env
            .root_dir
            .starts_with(dir.path().join(".tmp/ato/publish/install")));
        assert_eq!(
            std::fs::read_to_string(Path::new(&env.root_dir).join("source/hello.txt"))
                .expect("payload file"),
            "hello from payload"
        );
    }
}
