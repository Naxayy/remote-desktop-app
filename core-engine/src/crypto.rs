//! Cifrado end-to-end de la sesion.
//!
//! Handshake: cada punta genera un par de claves X25519 efimero
//! (nuevo por sesion, se descarta al terminar), se mandan las claves
//! publicas entre si (via el relay, sin cifrar - es Diffie-Hellman,
//! la clave publica no necesita ser secreta), y cada una calcula el
//! mismo secreto compartido de forma independiente. Ese secreto pasa
//! por HKDF-SHA256 para derivar la clave simetrica final.
//!
//! De ahi en mas, todo el trafico de aplicacion (video, input,
//! archivos, control) se cifra con ChaCha20-Poly1305 antes de
//! mandarse por el relay. El signaling-server sigue viendo estos
//! bytes (los tiene que reenviar), pero no puede leer el contenido.
//!
//! Limite importante de este modelo: protege contra un operador de
//! relay pasivo/curioso (o alguien que consiga logs del servidor).
//! NO protege contra un relay activamente malicioso que decida
//! sustituir las claves publicas de ambas puntas en el momento del
//! intercambio (un MITM activo) - eso requeriria verificar las claves
//! por un canal separado, que queda fuera del alcance de esta
//! version.

use anyhow::Result;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use std::sync::atomic::{AtomicU64, Ordering};
use x25519_dalek::{EphemeralSecret, PublicKey};

/// Info fijo para HKDF - "personaliza" la derivacion de clave para
/// esta app en particular (buena practica, evita colisiones si el
/// mismo secreto DH se reusara en otro contexto - aunque aca no pasa
/// porque las claves son efimeras).
const HKDF_INFO: &[u8] = b"remote-desktop-app-v1-session-key";

/// Genera un par de claves X25519 efimero. `EphemeralSecret` no se
/// puede clonar ni reusar a proposito (fuerza a generar uno nuevo por
/// sesion, y a consumirlo en el momento del handshake).
pub fn generate_keypair() -> (EphemeralSecret, [u8; 32]) {
    let secret = EphemeralSecret::random();
    let public = PublicKey::from(&secret);
    (secret, public.to_bytes())
}

/// Consume nuestra clave privada efimera + la clave publica del peer,
/// calcula el secreto Diffie-Hellman, y lo pasa por HKDF-SHA256 para
/// derivar la clave simetrica de 32 bytes que usa ChaCha20-Poly1305.
pub fn derive_session_key(my_secret: EphemeralSecret, peer_public_bytes: [u8; 32]) -> [u8; 32] {
    let peer_public = PublicKey::from(peer_public_bytes);
    let shared_secret = my_secret.diffie_hellman(&peer_public);

    let hk = Hkdf::<Sha256>::new(None, shared_secret.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(HKDF_INFO, &mut key)
        .expect("32 bytes es un largo valido para HKDF-SHA256");
    key
}

/// Cifra/descifra mensajes de aplicacion con la clave de sesion ya
/// derivada. Cada mensaje saliente usa un nonce distinto (contador
/// que nunca se repite dentro de la misma sesion - la clave es
/// efimera, asi que no hace falta que sea aleatorio, alcanza con que
/// no se repita).
pub struct SessionCipher {
    cipher: ChaCha20Poly1305,
    send_counter: AtomicU64,
}

impl SessionCipher {
    pub fn new(key_bytes: [u8; 32]) -> Self {
        let key = Key::from_slice(&key_bytes);
        Self {
            cipher: ChaCha20Poly1305::new(key),
            send_counter: AtomicU64::new(0),
        }
    }

    /// Cifra `plaintext` y devuelve (nonce, ciphertext) - el llamador
    /// los empaqueta con `netproto::encode_encrypted`.
    pub fn encrypt(&self, plaintext: &[u8]) -> ([u8; 12], Vec<u8>) {
        let counter = self.send_counter.fetch_add(1, Ordering::Relaxed);
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[4..].copy_from_slice(&counter.to_be_bytes());
        let nonce = Nonce::from_slice(&nonce_bytes);

        // El unwrap es seguro: ChaCha20-Poly1305 con una clave de 32
        // bytes valida y un nonce de 12 bytes no puede fallar al cifrar.
        let ciphertext = self.cipher.encrypt(nonce, plaintext).expect("encrypt no deberia fallar");
        (nonce_bytes, ciphertext)
    }

    /// Descifra un mensaje recibido. Falla si el nonce/ciphertext estan
    /// corruptos o si alguien intento alterar el mensaje (la etiqueta
    /// de autenticacion de ChaCha20-Poly1305 no matchea).
    pub fn decrypt(&self, nonce_bytes: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
        if nonce_bytes.len() != 12 {
            anyhow::bail!("nonce de largo invalido: {} bytes", nonce_bytes.len());
        }
        let nonce = Nonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!("no se pudo descifrar el mensaje (corrupto o alterado)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dh_handshake_both_sides_derive_same_key() {
        let (secret_a, public_a) = generate_keypair();
        let (secret_b, public_b) = generate_keypair();

        let key_a = derive_session_key(secret_a, public_b);
        let key_b = derive_session_key(secret_b, public_a);

        assert_eq!(key_a, key_b, "ambas puntas deben derivar exactamente la misma clave");
    }

    #[test]
    fn different_sessions_get_different_keys() {
        let (secret_a1, public_a1) = generate_keypair();
        let (_secret_b1, public_b1) = generate_keypair();
        let key_1 = derive_session_key(secret_a1, public_b1);

        let (secret_a2, public_a2) = generate_keypair();
        let (_secret_b2, public_b2) = generate_keypair();
        let key_2 = derive_session_key(secret_a2, public_b2);

        let _ = public_a1;
        let _ = public_a2;
        assert_ne!(key_1, key_2, "sesiones distintas no deberian compartir clave");
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let cipher = SessionCipher::new(key);

        let plaintext = b"hola, esto es un frame de video (simulado)";
        let (nonce, ciphertext) = cipher.encrypt(plaintext);
        let decrypted = cipher.decrypt(&nonce, &ciphertext).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_fails_with_wrong_key() {
        let cipher_a = SessionCipher::new([1u8; 32]);
        let cipher_b = SessionCipher::new([2u8; 32]);

        let (nonce, ciphertext) = cipher_a.encrypt(b"secreto");
        assert!(cipher_b.decrypt(&nonce, &ciphertext).is_err());
    }

    #[test]
    fn decrypt_fails_if_ciphertext_tampered() {
        let cipher = SessionCipher::new([5u8; 32]);
        let (nonce, mut ciphertext) = cipher.encrypt(b"mensaje original");
        // Alteramos un byte del ciphertext - la etiqueta de
        // autenticacion deberia detectarlo.
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0xFF;
        assert!(cipher.decrypt(&nonce, &ciphertext).is_err());
    }

    #[test]
    fn successive_messages_use_different_nonces() {
        let cipher = SessionCipher::new([9u8; 32]);
        let (nonce1, _) = cipher.encrypt(b"primero");
        let (nonce2, _) = cipher.encrypt(b"segundo");
        assert_ne!(nonce1, nonce2);
    }
}
