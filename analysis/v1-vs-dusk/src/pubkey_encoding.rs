//! Exact public-key parsing at canonical history boundaries.
//!
//! Current decoded history stores schema-declared public keys as canonical
//! base58 strings. Early provisional extracts stringified the generated
//! decoder's `[u8; 32]` representation into a JSON array of decimal strings.
//! Only those two schema-known encodings are accepted here.

use std::str::FromStr;

use solana_pubkey::Pubkey;

/// Parses either a canonical base58 public key or the exact legacy
/// `["0", ..., "255"]` 32-byte representation.
///
/// Decimal byte strings must themselves be canonical: signs, whitespace, and
/// leading zeroes are rejected. JSON numeric arrays and every other shape are
/// rejected so unrelated 32-byte values cannot silently become public keys.
pub(crate) fn parse_schema_pubkey(value: &str) -> Option<Pubkey> {
    if let Ok(pubkey) = Pubkey::from_str(value) {
        if pubkey.to_string() == value {
            return Some(pubkey);
        }
    }

    let encoded: Vec<String> = serde_json::from_str(value).ok()?;
    let mut bytes = [0_u8; 32];
    if encoded.len() != bytes.len() {
        return None;
    }
    for (output, text) in bytes.iter_mut().zip(encoded) {
        let byte = text.parse::<u8>().ok()?;
        if byte.to_string() != text {
            return None;
        }
        *output = byte;
    }
    Some(Pubkey::new_from_array(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(bytes: &[u8]) -> String {
        serde_json::to_string(&bytes.iter().map(u8::to_string).collect::<Vec<String>>()).unwrap()
    }

    #[test]
    fn accepts_only_exact_schema_known_pubkey_encodings() {
        let bytes = [137_u8; 32];
        let pubkey = Pubkey::new_from_array(bytes);
        assert_eq!(parse_schema_pubkey(&pubkey.to_string()), Some(pubkey));
        assert_eq!(parse_schema_pubkey(&legacy(&bytes)), Some(pubkey));

        assert_eq!(parse_schema_pubkey(&legacy(&bytes[..31])), None);
        assert_eq!(parse_schema_pubkey(&legacy(&[137_u8; 33])), None);
        assert_eq!(
            parse_schema_pubkey(&serde_json::to_string(&bytes).unwrap()),
            None
        );
        assert_eq!(
            parse_schema_pubkey(&legacy(&bytes).replacen("\"137\"", "\"0137\"", 1)),
            None
        );
        assert_eq!(
            parse_schema_pubkey(&legacy(&bytes).replacen("\"137\"", "\"256\"", 1)),
            None
        );
        assert_eq!(parse_schema_pubkey("not-a-public-key"), None);
    }
}
