use rlive_core::{DeviceId, PluginFormat};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceKind {
    Builtin {
        type_id: String,
        #[serde(default)]
        sample: Option<PathBuf>,
    },
    Plugin {
        format: PluginFormat,
        /// Format-specific id (CLAP id, VST3 class id, AU `type:subtype:manufacturer`).
        plugin_id: String,
        path: PathBuf,
        /// Opaque plugin state blob.
        #[serde(default, with = "base64_opt")]
        state: Option<Vec<u8>>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
    pub bypass: bool,
    /// Plain parameter values by id (built-ins; plugins also keep their own state).
    #[serde(default)]
    pub params: BTreeMap<u32, f64>,
    /// Whether the device card is expanded in the device view.
    #[serde(default = "yes")]
    pub expanded: bool,
}

fn yes() -> bool {
    true
}

impl Device {
    pub fn builtin(type_id: &str, name: &str) -> Self {
        Self {
            id: DeviceId::new(),
            name: name.into(),
            kind: DeviceKind::Builtin {
                type_id: type_id.into(),
                sample: None,
            },
            bypass: false,
            params: BTreeMap::new(),
            expanded: true,
        }
    }

    pub fn plugin(format: PluginFormat, plugin_id: &str, path: PathBuf, name: &str) -> Self {
        Self {
            id: DeviceId::new(),
            name: name.into(),
            kind: DeviceKind::Plugin {
                format,
                plugin_id: plugin_id.into(),
                path,
                state: None,
            },
            bypass: false,
            params: BTreeMap::new(),
            expanded: true,
        }
    }
}

mod base64_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(s: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        let vals: Vec<u8> = s
            .bytes()
            .filter(|c| !c.is_ascii_whitespace())
            .take_while(|&c| c != b'=')
            .map(|c| ALPHABET.iter().position(|&a| a == c).map(|p| p as u8))
            .collect::<Option<_>>()?;
        for chunk in vals.chunks(4) {
            let mut n = 0u32;
            for (i, v) in chunk.iter().enumerate() {
                n |= (*v as u32) << (18 - 6 * i);
            }
            let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
            out.extend_from_slice(&bytes[..chunk.len().saturating_sub(1)]);
        }
        Some(out)
    }

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => s.serialize_some(&encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        match s {
            Some(s) => decode(&s)
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("invalid base64 plugin state")),
            None => Ok(None),
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn roundtrip() {
            for len in 0..20 {
                let data: Vec<u8> = (0..len).map(|i| (i * 37 + 5) as u8).collect();
                assert_eq!(super::decode(&super::encode(&data)).unwrap(), data);
            }
            assert_eq!(super::encode(b"Man"), "TWFu");
            assert_eq!(super::encode(b"Ma"), "TWE=");
        }
    }
}
