//! Online mode: the RSA key exchange and the session server check.
//!
//! Note: decryption uses RSA PKCS#1 v1.5 as the protocol requires; the `rsa`
//! crate's implementation is not constant-time (RUSTSEC-2023-0071). The key is
//! regenerated on every start and only protects the shared secret exchange.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use mc_protocol::packets::{GameProfile, ProfileProperty};
use rsa::pkcs8::EncodePublicKey;
use rsa::rand_core::{OsRng, RngCore};
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};

/// Mojang's session server; tests point this at a local fake.
pub const MOJANG_SESSION_SERVER: &str = "https://sessionserver.mojang.com";

pub struct ServerKey {
    private: RsaPrivateKey,
    /// DER-encoded SubjectPublicKeyInfo, as sent in Encryption Request.
    pub public_der: Vec<u8>,
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerKey { .. }")
    }
}

impl ServerKey {
    /// A fresh 1024-bit key, like the vanilla server generates on startup.
    pub fn generate() -> Result<Self> {
        let private = RsaPrivateKey::new(&mut OsRng, 1024).context("generating the RSA key")?;
        let public_der = RsaPublicKey::from(&private)
            .to_public_key_der()
            .context("encoding the RSA public key")?
            .as_bytes()
            .to_vec();
        Ok(Self {
            private,
            public_der,
        })
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        self.private
            .decrypt(Pkcs1v15Encrypt, data)
            .context("RSA decryption failed")
    }
}

pub fn verify_token() -> [u8; 4] {
    let mut token = [0; 4];
    OsRng.fill_bytes(&mut token);
    token
}

/// Asks the session server whether `username` joined with `server_hash`.
/// Returns the authenticated profile (with skin textures), or `None` if the
/// session server does not know this login.
pub async fn has_joined(
    session_server: &str,
    username: &str,
    server_hash: &str,
) -> Result<Option<GameProfile>> {
    // Player names are [A-Za-z0-9_] and hashes are hex with an optional '-',
    // so neither needs URL encoding.
    let url = format!(
        "{session_server}/session/minecraft/hasJoined?username={username}&serverId={server_hash}"
    );
    let body = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let mut response = agent
            .get(&url)
            .call()
            .context("session server request failed")?;
        if response.status() == 204 {
            return Ok(None);
        }
        Ok(Some(response.body_mut().read_to_string()?))
    })
    .await??;
    let Some(body) = body else {
        return Ok(None);
    };
    parse_profile(&body).map(Some)
}

fn parse_profile(body: &str) -> Result<GameProfile> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("session server sent invalid JSON")?;
    let id = json["id"].as_str().context("profile without id")?;
    if id.len() != 32 {
        bail!("profile id {id:?} is not 32 hex digits");
    }
    let uuid = u128::from_str_radix(id, 16).context("profile id is not hex")?;
    let name = json["name"]
        .as_str()
        .context("profile without name")?
        .to_owned();
    let properties = json["properties"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(ProfileProperty {
                name: p["name"].as_str()?.to_owned(),
                value: p["value"].as_str()?.to_owned(),
                signature: p["signature"].as_str().map(str::to_owned),
            })
        })
        .collect();
    Ok(GameProfile {
        uuid,
        name,
        properties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_server_profile() {
        let body = r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch",
            "properties":[{"name":"textures","value":"e30=","signature":"c2ln"}]}"#;
        let profile = parse_profile(body).unwrap();
        assert_eq!(profile.uuid, 0x069a79f444e94726a5befca90e38aaf5);
        assert_eq!(profile.name, "Notch");
        assert_eq!(profile.properties[0].name, "textures");
        assert_eq!(profile.properties[0].signature.as_deref(), Some("c2ln"));
    }

    #[test]
    fn key_roundtrip() {
        let key = ServerKey::generate().unwrap();
        let public =
            <RsaPublicKey as rsa::pkcs8::DecodePublicKey>::from_public_key_der(&key.public_der)
                .unwrap();
        let secret = [42u8; 16];
        let encrypted = public
            .encrypt(&mut OsRng, Pkcs1v15Encrypt, &secret)
            .unwrap();
        assert_eq!(encrypted.len(), 128, "1024-bit key");
        assert_eq!(key.decrypt(&encrypted).unwrap(), secret);
    }
}
