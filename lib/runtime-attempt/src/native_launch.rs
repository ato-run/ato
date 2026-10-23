//! Physical launch projection of the selected Derivation's native Adapter.
//! This is neither a new Capsule identity nor a claim that Contract K passed.

use anyhow::{Context, Result, ensure};
use ato_formation::native_surface::{NativePresentation, NativeSurfaceAdapter};
use ato_ipc::runtime_launch::MAX_NATIVE_LAUNCH_DECLARATION_BYTES;
use ato_ipc::session_surface::{SessionSurfaceDescriptor, SurfaceCapabilityValue};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLaunchDeclaration {
    pub derivation_ref: String,
    pub adapter: NativeSurfaceAdapter,
}

impl NativeLaunchDeclaration {
    pub fn new(derivation_ref: String, adapter: NativeSurfaceAdapter) -> Result<Self> {
        let declaration = Self {
            derivation_ref,
            adapter,
        };
        declaration.validate()?;
        Ok(declaration)
    }

    /// Decode only after the IPC launch envelope's bounds have been checked.
    pub fn decode(value: &serde_json::Value) -> Result<Self> {
        ensure!(
            serde_json::to_vec(value)?.len() <= MAX_NATIVE_LAUNCH_DECLARATION_BYTES,
            "native declaration exceeds byte bound"
        );
        let declaration: Self =
            serde_json::from_value(value.clone()).context("invalid native launch declaration")?;
        declaration.validate()?;
        Ok(declaration)
    }

    fn validate(&self) -> Result<()> {
        let digest = self
            .derivation_ref
            .strip_prefix("sha256:")
            .context("native declaration requires selected DerivationRef")?;
        ensure!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "native declaration requires canonical selected DerivationRef"
        );
        self.adapter.validate_catalog().map_err(anyhow::Error::msg)
    }

    /// Bind presentation metadata to the catalog inside the digested spec.
    /// The caller must still refuse execution until it owns the native lifecycle.
    pub fn validate_selection(
        &self,
        selected_ref: &str,
        surface: &SessionSurfaceDescriptor,
    ) -> Result<()> {
        ensure!(
            self.derivation_ref == selected_ref,
            "native selected DerivationRef mismatch"
        );
        surface
            .validate()
            .context("invalid native session Surface")?;
        let matches = match (&self.adapter.surface, surface) {
            (
                NativePresentation::PixelStream { width, height, .. },
                SessionSurfaceDescriptor::PixelStream {
                    profile, viewport, ..
                },
            ) => {
                profile == self.adapter.surface.profile()
                    && viewport.width == u32::from(*width)
                    && viewport.height == u32::from(*height)
            }
            (
                NativePresentation::Terminal { cols, rows, .. },
                SessionSurfaceDescriptor::Terminal {
                    profile,
                    capabilities,
                    ..
                },
            ) => {
                profile == self.adapter.surface.profile()
                    && capabilities.get("cols")
                        == Some(&SurfaceCapabilityValue::Integer(i64::from(*cols)))
                    && capabilities.get("rows")
                        == Some(&SurfaceCapabilityValue::Integer(i64::from(*rows)))
            }
            _ => false,
        };
        ensure!(
            matches,
            "native session Surface does not match selected Adapter kind/profile/size"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declaration() -> serde_json::Value {
        json!({"derivation_ref":format!("sha256:{}", "ab".repeat(32)), "adapter":{
            "surface":{"kind":"terminal","port":"app.native","cols":80,"rows":24},
            "probe_port":"app.probe","operations":[{"kind":"terminal_exchange","path":"/result",
                "input":[50,43,50,13],"output_delimiter":[69,78,68],"max_output_bytes":1024,"timeout_ms":1000}]
        }})
    }

    #[test]
    fn ipc_native_golden_contains_the_same_strict_validated_catalog() {
        let spec = ato_ipc::runtime_launch::RuntimeLaunchSpecV1::parse(include_str!(
            "../../ipc/tests/fixtures/runtime-launch-spec-v1/native-terminal-oci.json"
        ))
        .unwrap();
        let ato_ipc::runtime_launch::LaunchRealizationV1::Oci(oci) = spec.realization else {
            unreachable!()
        };
        let decoded =
            NativeLaunchDeclaration::decode(oci.native_surface.as_ref().unwrap()).unwrap();
        assert_eq!(
            decoded,
            NativeLaunchDeclaration::decode(&declaration()).unwrap()
        );
    }

    #[test]
    fn maximum_catalog_input_fits_the_shared_opaque_bound() {
        let mut value = declaration();
        let template = value["adapter"]["operations"][0].clone();
        value["adapter"]["operations"] = json!(
            (0..8)
                .map(|index| {
                    let mut operation = template.clone();
                    operation["path"] = json!(format!("/result/{index}"));
                    operation["input"] = json!(vec![126_u8; 4096]);
                    operation["max_output_bytes"] = json!(8192);
                    operation
                })
                .collect::<Vec<_>>()
        );
        assert!(serde_json::to_vec(&value).unwrap().len() > 65_536);
        NativeLaunchDeclaration::decode(&value).unwrap();
    }

    #[test]
    fn strict_native_decode_reuses_catalog_bounds() {
        NativeLaunchDeclaration::decode(&declaration()).unwrap();
        for path in ["outer", "adapter", "operation", "ref", "bounds"] {
            let mut value = declaration();
            match path {
                "outer" => value["expected_answer"] = json!("success"),
                "adapter" => value["adapter"]["expected_answer"] = json!("success"),
                "operation" => {
                    value["adapter"]["operations"][0]["expected_digest"] = json!("answer")
                }
                "ref" => value["derivation_ref"] = json!(format!("sha256:{}", "AB".repeat(32))),
                _ => value["adapter"]["operations"][0]["timeout_ms"] = json!(30_001),
            }
            assert!(NativeLaunchDeclaration::decode(&value).is_err(), "{path}");
        }
    }

    #[test]
    fn pixel_descriptor_requires_exact_catalog_viewport() {
        let mut value = declaration();
        value["adapter"]["surface"] =
            json!({"kind":"pixel_stream","port":"app.native","width":640,"height":480});
        value["adapter"]["operations"] = json!([{"kind":"pixel_key_frame","path":"/result","keysyms":[65293],"max_frame_bytes":1228800,"timeout_ms":1000}]);
        let declaration = NativeLaunchDeclaration::decode(&value).unwrap();
        let mut descriptor = json!({"kind":"pixel_stream","profile":"ato.pixel-stream.v1","surface_id":"surface_native",
            "transport":"rfb_websocket","viewport":{"width":640,"height":480},"capabilities":{}});
        let surface = serde_json::from_value(descriptor.clone()).unwrap();
        declaration
            .validate_selection(&declaration.derivation_ref, &surface)
            .unwrap();
        descriptor["viewport"]["width"] = json!(641);
        let surface = serde_json::from_value(descriptor).unwrap();
        assert!(
            declaration
                .validate_selection(&declaration.derivation_ref, &surface)
                .is_err()
        );
    }

    #[test]
    fn descriptor_requires_exact_selection_kind_profile_and_size() {
        let declaration = NativeLaunchDeclaration::decode(&declaration()).unwrap();
        let valid = json!({"kind":"terminal","profile":"ato.terminal-surface.v1","surface_id":"surface_native",
            "transport":"terminal_websocket","capabilities":{"cols":80,"rows":24}});
        let surface: SessionSurfaceDescriptor = serde_json::from_value(valid.clone()).unwrap();
        declaration
            .validate_selection(&declaration.derivation_ref, &surface)
            .unwrap();
        assert!(
            declaration
                .validate_selection(&format!("sha256:{}", "cd".repeat(32)), &surface)
                .is_err()
        );
        for value in [
            json!({"kind":"web","profile":"ato.web-surface.v1","surface_id":"surface_native","embed_policy":"sandboxed"}),
            {
                let mut v = valid.clone();
                v["capabilities"]["cols"] = json!(81);
                v
            },
            {
                let mut v = valid.clone();
                v["capabilities"] = json!({});
                v
            },
            {
                let mut v = valid.clone();
                v["profile"] = json!("legacy");
                v
            },
        ] {
            let surface: SessionSurfaceDescriptor = serde_json::from_value(value).unwrap();
            assert!(
                declaration
                    .validate_selection(&declaration.derivation_ref, &surface)
                    .is_err()
            );
        }
    }
}
