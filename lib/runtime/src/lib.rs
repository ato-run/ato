//! Product-neutral lifecycle for a portable Capsule session.
//!
//! The runtime host supplies physical realization. This crate only owns the
//! temporary repository boundary and delegates live operations through one
//! reusable handle.

#![forbid(unsafe_code)]

use std::path::Path;

use ato_computation::ComputationRef;
use ato_objects::{
    BranchOrigin, BundleMaterialization, LocalCapsuleRepository, ReferenceRegistry, decode_bundle,
    import_bundle,
};
use thiserror::Error;

pub const PORTABLE_SESSION_BRANCH: &str = "session";

#[derive(Clone)]
pub struct PortableSessionContext {
    repository: LocalCapsuleRepository,
    parent_root: ComputationRef,
    materializations: Vec<BundleMaterialization>,
}

impl PortableSessionContext {
    pub fn repository(&self) -> &LocalCapsuleRepository {
        &self.repository
    }

    pub fn parent_root(&self) -> &ComputationRef {
        &self.parent_root
    }

    pub fn branch(&self) -> &str {
        PORTABLE_SESSION_BRANCH
    }

    pub fn materializations(&self) -> &[BundleMaterialization] {
        &self.materializations
    }
}

pub trait PortableRuntimeFactory: Send + Sync {
    fn create(
        &self,
        context: &PortableSessionContext,
    ) -> Result<Box<dyn PortableSessionRuntime>, PortableSessionError>;
}

pub trait PortableSessionRuntime: Send {
    fn start(&mut self) -> Result<(), PortableSessionError>;
    fn wait(&mut self) -> Result<(), PortableSessionError>;
    fn current_head(&self) -> Result<ComputationRef, PortableSessionError>;
    fn encap_current(&mut self, output: &Path) -> Result<ComputationRef, PortableSessionError>;
    fn stop(&mut self) -> Result<(), PortableSessionError>;
}

pub struct PortableSession {
    context: PortableSessionContext,
    runtime: Option<Box<dyn PortableSessionRuntime>>,
}

impl PortableSession {
    pub fn import(
        bundle_bytes: &[u8],
        temp_repository: impl AsRef<Path>,
        references: &ReferenceRegistry,
    ) -> Result<Self, PortableSessionError> {
        let bundle = decode_bundle(bundle_bytes)?;
        let repository = LocalCapsuleRepository::open(temp_repository.as_ref())?;
        let parent_root = import_bundle(&bundle, repository.objects(), references)?;
        repository.create_branch(
            PORTABLE_SESSION_BRANCH,
            &parent_root,
            Some(&BranchOrigin {
                computation: parent_root.clone(),
                parent_record: None,
            }),
        )?;
        Ok(Self {
            context: PortableSessionContext {
                repository,
                parent_root,
                materializations: bundle.index.materializations,
            },
            runtime: None,
        })
    }

    pub fn context(&self) -> &PortableSessionContext {
        &self.context
    }

    pub fn start(
        &mut self,
        factory: &dyn PortableRuntimeFactory,
    ) -> Result<(), PortableSessionError> {
        if self.runtime.is_some() {
            return Err(PortableSessionError::AlreadyStarted);
        }
        let mut runtime = factory.create(&self.context)?;
        runtime.start()?;
        self.runtime = Some(runtime);
        Ok(())
    }

    pub fn current_head(&self) -> Result<ComputationRef, PortableSessionError> {
        match &self.runtime {
            Some(runtime) => runtime.current_head(),
            None => self
                .context
                .repository
                .head(PORTABLE_SESSION_BRANCH)?
                .ok_or(PortableSessionError::MissingBranch),
        }
    }

    pub fn encap_current(&mut self, output: &Path) -> Result<ComputationRef, PortableSessionError> {
        self.runtime
            .as_mut()
            .ok_or(PortableSessionError::NotStarted)?
            .encap_current(output)
    }

    pub fn wait(&mut self) -> Result<(), PortableSessionError> {
        self.runtime
            .as_mut()
            .ok_or(PortableSessionError::NotStarted)?
            .wait()
    }

    pub fn stop(&mut self) -> Result<(), PortableSessionError> {
        let Some(mut runtime) = self.runtime.take() else {
            return Ok(());
        };
        runtime.stop()
    }
}

#[derive(Debug, Error)]
pub enum PortableSessionError {
    #[error(transparent)]
    Bundle(#[from] ato_objects::BundleError),
    #[error(transparent)]
    Repository(#[from] ato_objects::RepositoryError),
    #[error("portable session is already started")]
    AlreadyStarted,
    #[error("portable session has not started")]
    NotStarted,
    #[error("portable session branch is missing")]
    MissingBranch,
    #[error("portable runtime failed: {0}")]
    Runtime(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use ato_computation::{
        Boundary, ComputationObject, ResolvedComputation, SemanticsId, computation_ref,
        encode_computation_object,
    };
    use ato_objects::{
        ComputationReferences, MemoryObjectStore, ObjectLink, ObjectResolver, ObjectStore,
        ReferenceRegistry, encode_bundle, export_bundle,
    };

    use super::*;

    struct LeafReferences(SemanticsId);

    impl ComputationReferences for LeafReferences {
        fn semantics(&self) -> &SemanticsId {
            &self.0
        }

        fn outgoing(
            &self,
            _computation: &ResolvedComputation,
            _objects: &dyn ObjectResolver,
        ) -> Result<Vec<ObjectLink>, ato_objects::BundleError> {
            Ok(Vec::new())
        }
    }

    struct FakeFactory {
        head: Arc<Mutex<ComputationRef>>,
    }

    impl PortableRuntimeFactory for FakeFactory {
        fn create(
            &self,
            _context: &PortableSessionContext,
        ) -> Result<Box<dyn PortableSessionRuntime>, PortableSessionError> {
            Ok(Box::new(FakeRuntime {
                head: Arc::clone(&self.head),
            }))
        }
    }

    struct FakeRuntime {
        head: Arc<Mutex<ComputationRef>>,
    }

    impl PortableSessionRuntime for FakeRuntime {
        fn start(&mut self) -> Result<(), PortableSessionError> {
            Ok(())
        }
        fn wait(&mut self) -> Result<(), PortableSessionError> {
            Ok(())
        }
        fn current_head(&self) -> Result<ComputationRef, PortableSessionError> {
            Ok(self.head.lock().unwrap().clone())
        }
        fn encap_current(&mut self, output: &Path) -> Result<ComputationRef, PortableSessionError> {
            std::fs::write(output, b"child")
                .map_err(|error| PortableSessionError::Runtime(error.to_string()))?;
            self.current_head()
        }
        fn stop(&mut self) -> Result<(), PortableSessionError> {
            Ok(())
        }
    }

    #[test]
    fn imported_parent_stays_immutable_while_session_advances_and_encapsulates() {
        let source = MemoryObjectStore::default();
        let semantics = SemanticsId::parse("example.portable@1").unwrap();
        let residual = source.put(b"parent").unwrap();
        let object = ComputationObject {
            semantics: semantics.clone(),
            boundary: Boundary::new(),
            residual,
        };
        let parent = computation_ref(&object).unwrap();
        source
            .insert(
                parent.content_ref(),
                &encode_computation_object(&object).unwrap(),
            )
            .unwrap();
        let mut references = ReferenceRegistry::default();
        references
            .register(Arc::new(LeafReferences(semantics.clone())))
            .unwrap();
        let bundle = encode_bundle(&export_bundle(&parent, &source, &references).unwrap()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut session = PortableSession::import(&bundle, directory.path(), &references).unwrap();
        assert_eq!(session.current_head().unwrap(), parent);

        let child_residual = session
            .context()
            .repository()
            .objects()
            .put(b"child")
            .unwrap();
        let child_object = ComputationObject {
            semantics,
            boundary: BTreeMap::new(),
            residual: child_residual,
        };
        let child = computation_ref(&child_object).unwrap();
        session
            .context()
            .repository()
            .objects()
            .insert(
                child.content_ref(),
                &encode_computation_object(&child_object).unwrap(),
            )
            .unwrap();
        let head = Arc::new(Mutex::new(child.clone()));
        session
            .start(&FakeFactory {
                head: Arc::clone(&head),
            })
            .unwrap();
        assert_eq!(session.current_head().unwrap(), child);
        let output = directory.path().join("child.capsule");
        assert_eq!(session.encap_current(&output).unwrap(), child);
        assert_eq!(std::fs::read(output).unwrap(), b"child");
        assert_eq!(session.context().parent_root(), &parent);
        assert_eq!(
            session
                .context()
                .repository()
                .branch_origin(PORTABLE_SESSION_BRANCH)
                .unwrap()
                .unwrap()
                .computation,
            parent
        );
        session.stop().unwrap();
    }
}
