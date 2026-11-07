//! CBOR (RFC 8949): what structured values cross between the host and an extension as, where nobody reads them - a
//! binary encoding of serde's data model that describes itself, so that a TOML table reads back as one
//! (`deserialize_any`), and gives the same bytes for the same values.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Result, anyhow};

/// A value as CBOR.
///
/// # Errors
/// It does not serialize.
pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).map_err(|e| anyhow!("it does not encode: {e}"))?;
    Ok(bytes)
}

/// A value read back from CBOR.
///
/// # Errors
/// It is no CBOR, or not a `T`.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ciborium::from_reader(bytes).map_err(|e| anyhow!("it does not decode: {e}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_table_reads_back_as_it_was() {
        let table: crate::Table =
            toml::from_str("a = 1\nb = [1.5, \"x\"]\n[c]\nd = true\ne = { f = -3 }\n").expect("a table");
        assert_eq!(super::decode::<crate::Table>(&super::encode(&table).unwrap()).unwrap(), table);
    }
}
