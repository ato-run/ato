use std::collections::BTreeMap;

use ato_computation::{ComputationRef, ContentRef, PortId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CompositeResidual, Connection, ConnectionError, Endpoint, NodeId, NodeIdError};

/// Maximum canonical byte length of one compose residual.
pub const MAX_COMPOSITE_RESIDUAL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum CompositeResidualCodecError {
    #[error("compose residual is {actual} bytes; maximum is {maximum}")]
    ObjectTooLarge { actual: u64, maximum: u64 },
    #[error("compose residual JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("compose residual identifier failed: {0}")]
    CoreIdentifier(#[from] ato_computation::IdentifierError),
    #[error(transparent)]
    NodeIdentifier(#[from] NodeIdError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error("compose residual is not in its canonical JCS representation")]
    NonCanonical,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositeResidualWire {
    nodes: BTreeMap<String, String>,
    connections: Vec<ConnectionWire>,
    exports: BTreeMap<String, EndpointWire>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionWire {
    first: EndpointWire,
    second: EndpointWire,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointWire {
    node: String,
    port: String,
}

pub fn encode_composite_residual(
    residual: &CompositeResidual,
) -> Result<Vec<u8>, CompositeResidualCodecError> {
    let nodes = residual
        .nodes
        .iter()
        .map(|(node, computation)| (node.as_str().to_owned(), computation.as_str().to_owned()))
        .collect();
    let mut connections: Vec<_> = residual
        .connections
        .iter()
        .map(|connection| ConnectionWire {
            first: encode_endpoint(connection.first()),
            second: encode_endpoint(connection.second()),
        })
        .collect();
    connections.sort_by(|left, right| {
        endpoint_wire_key(&left.first)
            .cmp(&endpoint_wire_key(&right.first))
            .then_with(|| endpoint_wire_key(&left.second).cmp(&endpoint_wire_key(&right.second)))
    });
    let exports = residual
        .exports
        .iter()
        .map(|(port, endpoint)| (port.as_str().to_owned(), encode_endpoint(endpoint)))
        .collect();
    let bytes = serde_jcs::to_vec(&CompositeResidualWire {
        nodes,
        connections,
        exports,
    })?;
    ensure_size(&bytes)?;
    Ok(bytes)
}

pub fn decode_composite_residual(
    bytes: &[u8],
) -> Result<CompositeResidual, CompositeResidualCodecError> {
    ensure_size(bytes)?;
    let wire: CompositeResidualWire = serde_json::from_slice(bytes)?;
    let residual = CompositeResidual {
        nodes: wire
            .nodes
            .into_iter()
            .map(|(node, computation)| {
                Ok((NodeId::parse(node)?, ComputationRef::parse(computation)?))
            })
            .collect::<Result<_, CompositeResidualCodecError>>()?,
        connections: wire
            .connections
            .into_iter()
            .map(|connection| {
                Ok(Connection::new(
                    decode_endpoint(connection.first)?,
                    decode_endpoint(connection.second)?,
                )?)
            })
            .collect::<Result<_, CompositeResidualCodecError>>()?,
        exports: wire
            .exports
            .into_iter()
            .map(|(port, endpoint)| Ok((PortId::parse(port)?, decode_endpoint(endpoint)?)))
            .collect::<Result<_, CompositeResidualCodecError>>()?,
    };
    if encode_composite_residual(&residual)? != bytes {
        return Err(CompositeResidualCodecError::NonCanonical);
    }
    Ok(residual)
}

pub fn composite_residual_ref(
    residual: &CompositeResidual,
) -> Result<ContentRef, CompositeResidualCodecError> {
    let bytes = encode_composite_residual(residual)?;
    Ok(ContentRef::parse(format!(
        "blake3:{}",
        blake3::hash(&bytes).to_hex()
    ))?)
}

fn encode_endpoint(endpoint: &Endpoint) -> EndpointWire {
    EndpointWire {
        node: endpoint.node.as_str().to_owned(),
        port: endpoint.port.as_str().to_owned(),
    }
}

fn decode_endpoint(endpoint: EndpointWire) -> Result<Endpoint, CompositeResidualCodecError> {
    Ok(Endpoint {
        node: NodeId::parse(endpoint.node)?,
        port: PortId::parse(endpoint.port)?,
    })
}

fn endpoint_wire_key(endpoint: &EndpointWire) -> (&str, &str) {
    (&endpoint.node, &endpoint.port)
}

fn ensure_size(bytes: &[u8]) -> Result<(), CompositeResidualCodecError> {
    let actual = bytes.len() as u64;
    if actual > MAX_COMPOSITE_RESIDUAL_BYTES {
        return Err(CompositeResidualCodecError::ObjectTooLarge {
            actual,
            maximum: MAX_COMPOSITE_RESIDUAL_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(byte: &str) -> ComputationRef {
        ComputationRef::parse(format!("blake3:{}", byte.repeat(64))).unwrap()
    }

    fn endpoint(node: &str, port: &str) -> Endpoint {
        Endpoint {
            node: NodeId::parse(node).unwrap(),
            port: PortId::parse(port).unwrap(),
        }
    }

    fn fixture() -> CompositeResidual {
        CompositeResidual {
            nodes: BTreeMap::from([
                (NodeId::parse("greeter").unwrap(), reference("a")),
                (NodeId::parse("name-provider").unwrap(), reference("b")),
            ]),
            connections: vec![
                Connection::new(
                    endpoint("name-provider", "name"),
                    endpoint("greeter", "name"),
                )
                .unwrap(),
            ],
            exports: BTreeMap::from([(
                PortId::parse("greeting").unwrap(),
                endpoint("greeter", "greeting"),
            )]),
        }
    }

    #[test]
    fn canonical_encoding_and_reference_match_golden_vectors() {
        let bytes = encode_composite_residual(&fixture()).unwrap();
        let expected =
            hex::decode(include_str!("../tests/vectors/composite_residual_v1.jcs.hex").trim())
                .unwrap();
        let reference = composite_residual_ref(&fixture()).unwrap();

        assert_eq!(bytes, expected);
        assert_eq!(
            reference.as_str(),
            include_str!("../tests/vectors/composite_residual_v1.ref").trim()
        );
        assert_eq!(decode_composite_residual(&bytes).unwrap(), fixture());
    }

    #[test]
    fn decoder_rejects_noncanonical_connection_order() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&encode_composite_residual(&fixture()).unwrap()).unwrap();
        let connection = &mut value["connections"][0];
        let first = connection["first"].take();
        connection["first"] = connection["second"].take();
        connection["second"] = first;
        let bytes = serde_jcs::to_vec(&value).unwrap();

        assert!(matches!(
            decode_composite_residual(&bytes),
            Err(CompositeResidualCodecError::NonCanonical)
        ));
    }

    #[test]
    fn codec_rejects_residual_above_limit() {
        let bytes = vec![b' '; MAX_COMPOSITE_RESIDUAL_BYTES as usize + 1];

        assert!(matches!(
            decode_composite_residual(&bytes),
            Err(CompositeResidualCodecError::ObjectTooLarge { .. })
        ));
    }
}
