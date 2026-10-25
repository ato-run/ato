//! Step-owned OCI Adapter input selectors. Missing is the legacy no-data lane.
use std::collections::BTreeSet;

pub const OCI_INPUT_IDS_RUNTIME: &str = "oci.input_ids";
pub const OCI_COMPONENT_DATA_PLANE_RUNTIME_FEATURE: &str =
    "runtime_feature=oci_component_data_plane_v1";
pub const MAX_COMPONENT_INPUT_IDS: usize = 3;
pub const ASSETS_INPUT_ID: &str = "assets";

pub fn is_input_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(b))
        && id != "workspace"
}

/// Missing remains None; an explicit null is not an assignment.
pub(crate) fn deserialize_input_ids<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <Vec<String> as serde::Deserialize>::deserialize(deserializer).map(Some)
}

/// Sorted and unique as authored, never silently normalized during binding.
pub fn validate_input_ids(ids: &[String]) -> Result<(), &'static str> {
    if ids.len() > MAX_COMPONENT_INPUT_IDS
        || ids.iter().any(|id| !is_input_id(id))
        || ids.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err("component input IDs must be safe, sorted and unique");
    }
    let mut names = BTreeSet::new();
    if ids.iter().any(|id| !names.insert(input_env_name(id))) {
        return Err("component input IDs collide after environment projection");
    }
    Ok(())
}

pub fn parse_input_ids(value: &str) -> Result<Vec<String>, &'static str> {
    if value.len() > 1024 {
        return Err("component input selector exceeds bound");
    }
    let ids: Vec<String> =
        serde_json::from_str(value).map_err(|_| "component input selector is not an ID array")?;
    validate_input_ids(&ids)?;
    if serde_json::to_string(&ids).map_err(|_| "component input selector encoding failed")? != value
    {
        return Err("component input selector must use canonical JSON bytes");
    }
    Ok(ids)
}

/// Existing data-plane environment projection, shared without reverse inference.
pub fn input_env_name(id: &str) -> String {
    if id == ASSETS_INPUT_ID {
        return "ATO_INPUT_ASSETS_DIR".into();
    }
    format!(
        "ATO_INPUT_PATH_{}",
        id.chars()
            .map(|c| if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_empty_and_readonly_selectors_are_canonical() {
        assert_eq!(parse_input_ids("[]").unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_input_ids("[\"assets\",\"models\"]").unwrap(),
            ["assets", "models"]
        );
        for invalid in [
            "null",
            "{}",
            "[1]",
            "[\"models\",\"assets\"]",
            "[\"models\",\"models\"]",
            "[\"workspace\"]",
            "[\"../secret\"]",
            "[\"MODELS\"]",
            "[ \"models\" ]",
            "[\"a-b\",\"a.b\"]",
        ] {
            assert!(parse_input_ids(invalid).is_err(), "{invalid}");
        }
    }
}
