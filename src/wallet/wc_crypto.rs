//! Criptografía mínima de WalletConnect v2, adaptada de walletconnect-sdk-rs
//! (zemse, MIT OR Apache-2.0) que a su vez replica el monorepo JS oficial:
//! JWT EdDSA para autenticarse con el relay, envelopes ChaCha20-Poly1305 para
//! los mensajes del protocolo Sign, y derivación x25519+HKDF de la clave de
//! sesión. Validado end-to-end contra MetaMask en spike/wc_sign (2026-07-13).

use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::hex;
use anyhow::{anyhow, bail, Result};
use base64ct::{Base64, Base64UrlUnpadded, Encoding};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signer, SigningKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

const JWT_TTL_SECS: u64 = 86_400;
/// Header multicodec de ed25519 en base58 ("K36" = bytes [0xed, 0x01]).
const MULTICODEC_ED25519_HEADER: &str = "K36";

const IV_LEN: usize = 12;
const KEY_LEN: usize = 32;

pub fn random_bytes32() -> [u8; 32] {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    b
}

pub fn sha256_32(data: [u8; 32]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn x25519_public(secret: [u8; 32]) -> [u8; 32] {
    let secret = x25519_dalek::StaticSecret::from(secret);
    x25519_dalek::PublicKey::from(&secret).to_bytes()
}

/// Clave simétrica de sesión: HKDF-SHA256 (sin salt ni info) del secreto ECDH.
pub fn derive_sym_key(private: [u8; 32], public: [u8; 32]) -> [u8; 32] {
    let private = x25519_dalek::StaticSecret::from(private);
    let shared = private.diffie_hellman(&x25519_dalek::PublicKey::from(public));
    let hk = Hkdf::<Sha256>::new(None, shared.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(&[], &mut okm).expect("hkdf expand");
    okm
}

/// JWT "iridium" EdDSA con el que el relay autentica clientes. `aud` debe ser
/// la URL exacta a la que se conecta (wss://relay.walletconnect.org).
pub fn sign_relay_jwt(seed: [u8; 32], aud: &str) -> String {
    let signing_key = SigningKey::from_bytes(&seed);
    let public_key = signing_key.verifying_key().to_bytes();

    // iss = did:key:z<base58(header_multicodec + pubkey)>
    let header_bytes = bs58::decode(MULTICODEC_ED25519_HEADER)
        .into_vec()
        .expect("header base58 constante");
    let iss = format!(
        "did:key:z{}",
        bs58::encode([header_bytes, public_key.to_vec()].concat()).into_string()
    );

    let iat = unix_ts();
    let header = r#"{"alg":"EdDSA","typ":"JWT"}"#;
    let payload = format!(
        r#"{{"iss":"{iss}","sub":"{}","aud":"{aud}","iat":{iat},"exp":{}}}"#,
        hex::encode(random_bytes32()),
        iat + JWT_TTL_SECS
    );
    let head_payload = format!(
        "{}.{}",
        Base64UrlUnpadded::encode_string(header.as_bytes()),
        Base64UrlUnpadded::encode_string(payload.as_bytes())
    );
    let signature = signing_key.sign(head_payload.as_bytes()).to_bytes();
    format!(
        "{head_payload}.{}",
        Base64UrlUnpadded::encode_string(&signature)
    )
}

/// Envelope tipo 0 (`[0x00 | iv(12) | sellado]` en base64): cifra el JSON de
/// un mensaje del protocolo Sign con la clave del topic.
pub fn encrypt_type0(sym_key: [u8; 32], plaintext: &str) -> Result<String> {
    let mut iv = [0u8; IV_LEN];
    OsRng.fill_bytes(&mut iv);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&sym_key));
    let sealed = cipher
        .encrypt(Nonce::from_slice(&iv), plaintext.as_bytes())
        .map_err(|e| anyhow!("cifrado del envelope: {e}"))?;
    let mut bytes = vec![0u8];
    bytes.extend_from_slice(&iv);
    bytes.extend_from_slice(&sealed);
    Ok(Base64::encode_string(&bytes))
}

/// Descifra un envelope entrante. Acepta tipo 0 y tipo 1 (este último lleva
/// la pubkey del emisor antepuesta; aquí solo se salta — la clave ya se conoce).
pub fn decrypt(sealed_b64: &str, sym_key: [u8; 32]) -> Result<String> {
    let bytes =
        Base64::decode_vec(sealed_b64).map_err(|e| anyhow!("envelope no es base64: {e}"))?;
    let (iv, sealed) = match bytes.first() {
        Some(0) if bytes.len() > 1 + IV_LEN => (&bytes[1..1 + IV_LEN], &bytes[1 + IV_LEN..]),
        Some(1) if bytes.len() > 1 + KEY_LEN + IV_LEN => (
            &bytes[1 + KEY_LEN..1 + KEY_LEN + IV_LEN],
            &bytes[1 + KEY_LEN + IV_LEN..],
        ),
        Some(t) => bail!("envelope tipo {t} no soportado o truncado"),
        None => bail!("envelope vacío"),
    };
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&sym_key));
    let plain = cipher
        .decrypt(Nonce::from_slice(iv), sealed)
        .map_err(|e| anyhow!("descifrado del envelope: {e}"))?;
    String::from_utf8(plain).map_err(|e| anyhow!("payload no es UTF-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_type0() {
        let key = random_bytes32();
        let msg = r#"{"jsonrpc":"2.0","id":1,"result":true}"#;
        let sealed = encrypt_type0(key, msg).unwrap();
        assert_eq!(decrypt(&sealed, key).unwrap(), msg);
    }

    #[test]
    fn sym_key_conmutativa() {
        // ambos lados del ECDH derivan la misma clave de sesión
        let a = random_bytes32();
        let b = random_bytes32();
        assert_eq!(
            derive_sym_key(a, x25519_public(b)),
            derive_sym_key(b, x25519_public(a))
        );
    }

    #[test]
    fn did_key_conocido() {
        // vector del SDK original: seed de ceros → did:key determinista
        let signing_key = SigningKey::from_bytes(&[0u8; 32]);
        let public_key = signing_key.verifying_key().to_bytes();
        let header = bs58::decode(MULTICODEC_ED25519_HEADER).into_vec().unwrap();
        let iss = format!(
            "did:key:z{}",
            bs58::encode([header, public_key.to_vec()].concat()).into_string()
        );
        assert_eq!(
            iss,
            "did:key:z6MkiTBz1ymuepAQ4HEHYSF1H8quG5GLVVQR3djdX3mDooWp"
        );
    }
}
