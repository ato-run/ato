//! Model Sets: the immutable, content-addressed list of model objects a route
//! consumes.
//!
//! A Model Set is referenced from a Derivation as a `BoundInput` with protocol
//! [`MODEL_SET_PROTOCOL`]. `BoundInput` is an immutable *logical* input
//! reference whose resolution depends on its protocol: for a Model Set the
//! manifest (this type, a few KiB) travels with the bundle, while the objects
//! it lists are resolved by digest through the data plane and are never part
//! of a bundle. Names, versions and upstream sources are operator metadata
//! outside the manifest, so relabelling a Model Set never changes its digest.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MODEL_SET_PROTOCOL: &str = "ato.model-set@1";
pub const MODEL_SET_SCHEMA: &str = "ato.model-set/1";
pub const MAX_MODEL_SET_OBJECTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSetManifest {
    pub schema: String,
    pub objects: Vec<ModelSetEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSetEntry {
    /// Relative, `/`-separated, no `.`/`..` segments.
    pub path: String,
    /// `sha256:<64 lowercase hex>` of the object's bytes.
    pub digest: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ModelSetError(pub &'static str);

fn is_sha256_ref(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

impl ModelSetManifest {
    /// Structure only; canonical order is checked by [`Self::reference`].
    pub fn validate(&self) -> Result<(), ModelSetError> {
        if self.schema != MODEL_SET_SCHEMA {
            return Err(ModelSetError("model_set_schema_unsupported"));
        }
        if self.objects.is_empty() || self.objects.len() > MAX_MODEL_SET_OBJECTS {
            return Err(ModelSetError("model_set_bounds"));
        }
        let mut paths = BTreeSet::new();
        for entry in &self.objects {
            if !is_safe_path(&entry.path) || !is_sha256_ref(&entry.digest) || entry.bytes == 0 {
                return Err(ModelSetError("model_set_entry_invalid"));
            }
            if !paths.insert(entry.path.as_str()) {
                return Err(ModelSetError("model_set_path_duplicate"));
            }
            // A path that is a prefix directory of another would make the
            // materialized tree ambiguous.
            if paths.iter().any(|other| {
                other.starts_with(&format!("{}/", entry.path))
                    || entry.path.starts_with(&format!("{other}/"))
            }) {
                return Err(ModelSetError("model_set_path_conflict"));
            }
        }
        Ok(())
    }

    /// The same manifest with objects in canonical (path) order.
    pub fn canonicalized(&self) -> Result<Self, ModelSetError> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical.objects.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(canonical)
    }

    /// The canonical bytes: JCS of the canonical manifest.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ModelSetError> {
        serde_jcs::to_vec(&self.canonicalized()?)
            .map_err(|_| ModelSetError("model_set_canonicalization"))
    }

    /// `sha256:` of the canonical bytes: the Model Set's identity. Refuses a
    /// manifest that is not already in canonical order, so the digest a
    /// Derivation names is the digest of the bytes it carries.
    pub fn reference(&self) -> Result<String, ModelSetError> {
        if self.canonicalized()? != *self {
            return Err(ModelSetError("model_set_not_canonical"));
        }
        let bytes = self.canonical_bytes()?;
        Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
    }

    /// Parse canonical bytes and check they are exactly the canonical form.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<(Self, String), ModelSetError> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|_| ModelSetError("model_set_malformed"))?;
        if manifest.canonical_bytes()? != bytes {
            return Err(ModelSetError("model_set_not_canonical"));
        }
        let reference = manifest.reference()?;
        Ok((manifest, reference))
    }

    pub fn total_bytes(&self) -> u64 {
        self.objects.iter().map(|o| o.bytes).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, fill: char, bytes: u64) -> ModelSetEntry {
        ModelSetEntry {
            path: path.to_owned(),
            digest: format!("sha256:{}", fill.to_string().repeat(64)),
            bytes,
        }
    }

    fn manifest(objects: Vec<ModelSetEntry>) -> ModelSetManifest {
        ModelSetManifest {
            schema: MODEL_SET_SCHEMA.to_owned(),
            objects,
        }
    }

    #[test]
    fn identity_is_the_digest_of_canonical_bytes() {
        let m = manifest(vec![
            entry("a/vae.safetensors", 'a', 10),
            entry("b/model.safetensors", 'b', 20),
        ]);
        let reference = m.reference().unwrap();
        let bytes = m.canonical_bytes().unwrap();
        assert_eq!(
            ModelSetManifest::from_canonical_bytes(&bytes).unwrap(),
            (m.clone(), reference.clone())
        );
        assert_eq!(reference, format!("sha256:{:x}", Sha256::digest(&bytes)));
        assert_eq!(m.total_bytes(), 30);
    }

    #[test]
    fn non_canonical_or_invalid_manifests_are_refused() {
        let unordered = manifest(vec![entry("b", 'b', 1), entry("a", 'a', 1)]);
        assert_eq!(
            unordered.reference().unwrap_err().0,
            "model_set_not_canonical"
        );
        assert!(unordered.canonicalized().unwrap().reference().is_ok());
        for bad in [
            manifest(vec![]),
            manifest(vec![entry("../x", 'a', 1)]),
            manifest(vec![entry("/x", 'a', 1)]),
            manifest(vec![entry("x", 'a', 0)]),
            manifest(vec![entry("x", 'A', 1)]),
            manifest(vec![entry("x", 'a', 1), entry("x", 'b', 1)]),
            manifest(vec![entry("x", 'a', 1), entry("x/y", 'b', 1)]),
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert!(
            serde_json::from_str::<ModelSetManifest>(
                r#"{"schema":"ato.model-set/1","objects":[],"name":"wan"}"#
            )
            .is_err()
        );
        // Pretty-printed bytes of a valid manifest are not its canonical bytes.
        let m = manifest(vec![entry("a", 'a', 1)]);
        let pretty = serde_json::to_vec_pretty(&m).unwrap();
        assert_eq!(
            ModelSetManifest::from_canonical_bytes(&pretty)
                .unwrap_err()
                .0,
            "model_set_not_canonical"
        );
    }
}
