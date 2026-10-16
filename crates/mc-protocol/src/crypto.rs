//! Protocol encryption pieces: the AES/CFB8 stream cipher used after the key
//! exchange, and Minecraft's signed hex SHA-1 used for the session server hash.
//! RSA (the key exchange itself) lives in `mc-server`.

use aes::Aes128;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use sha1::{Digest, Sha1};

/// AES-128 in CFB8 mode. Minecraft uses the shared secret as both key and IV,
/// and the cipher runs continuously over the whole connection (never reset
/// between packets). One instance per direction.
pub struct Cfb8 {
    cipher: Aes128,
    register: [u8; 16],
}

impl std::fmt::Debug for Cfb8 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cfb8 { .. }")
    }
}

impl Cfb8 {
    /// The connection cipher: `secret` is key and IV.
    pub fn new(secret: &[u8; 16]) -> Self {
        Self::with_iv(secret, secret)
    }

    pub fn with_iv(key: &[u8; 16], iv: &[u8; 16]) -> Self {
        Self {
            cipher: Aes128::new(&(*key).into()),
            register: *iv,
        }
    }

    fn keystream_byte(&self) -> u8 {
        let mut block = self.register.into();
        self.cipher.encrypt_block(&mut block);
        block[0]
    }

    fn push(&mut self, ciphertext: u8) {
        self.register.copy_within(1.., 0);
        self.register[15] = ciphertext;
    }

    pub fn encrypt(&mut self, data: &mut [u8]) {
        for byte in data {
            *byte ^= self.keystream_byte();
            self.push(*byte);
        }
    }

    pub fn decrypt(&mut self, data: &mut [u8]) {
        for byte in data {
            let ciphertext = *byte;
            *byte ^= self.keystream_byte();
            self.push(ciphertext);
        }
    }
}

/// SHA-1 digest printed like Java's `new BigInteger(digest).toString(16)`:
/// the bytes as one signed two's-complement number, so it may start with `-`
/// and has no leading zeros.
pub fn minecraft_hex_digest(digest: [u8; 20]) -> String {
    let negative = digest[0] & 0x80 != 0;
    let mut magnitude = digest;
    if negative {
        let mut carry = true;
        for byte in magnitude.iter_mut().rev() {
            *byte = !*byte;
            if carry {
                (*byte, carry) = byte.overflowing_add(1);
            }
        }
    }
    let hex: String = magnitude.iter().map(|b| format!("{b:02x}")).collect();
    let hex = match hex.trim_start_matches('0') {
        "" => "0",
        trimmed => trimmed,
    };
    if negative {
        format!("-{hex}")
    } else {
        hex.to_owned()
    }
}

/// The hash both sides send to the session server for a login.
pub fn server_hash(server_id: &str, shared_secret: &[u8], public_key_der: &[u8]) -> String {
    let mut sha = Sha1::new();
    sha.update(server_id.as_bytes());
    sha.update(shared_secret);
    sha.update(public_key_der);
    minecraft_hex_digest(sha.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn digests_match_documented_examples() {
        let digest = |name: &str| minecraft_hex_digest(Sha1::digest(name.as_bytes()).into());
        assert_eq!(digest("Notch"), "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48");
        assert_eq!(digest("jeb_"), "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1");
        assert_eq!(digest("simon"), "88e16a1019277b15d58faf0541e11910eb756f6");
    }

    #[test]
    fn cfb8_matches_nist_vector() {
        // NIST SP 800-38A, F.3.7 CFB8-AES128.Encrypt.
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let plaintext = hex("6bc1bee22e409f96e93d7e117393172aae2d");
        let ciphertext = hex("3b79424c9c0dd436bace9e0ed4586a4f32b9");

        let mut data = plaintext.clone();
        Cfb8::with_iv(&key, &iv).encrypt(&mut data);
        assert_eq!(data, ciphertext);
        Cfb8::with_iv(&key, &iv).decrypt(&mut data);
        assert_eq!(data, plaintext);
    }

    #[test]
    fn cfb8_is_a_stream() {
        // Splitting the input anywhere must not change the output.
        let secret = [7u8; 16];
        let message: Vec<u8> = (0..=255).collect();
        let mut whole = message.clone();
        Cfb8::new(&secret).encrypt(&mut whole);
        let mut pieces = message.clone();
        let mut cipher = Cfb8::new(&secret);
        for chunk in pieces.chunks_mut(37) {
            cipher.encrypt(chunk);
        }
        assert_eq!(pieces, whole);
    }
}
