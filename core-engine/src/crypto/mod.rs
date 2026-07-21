//! Cifrado end-to-end de la sesion.
//!
//! Proximo paso: handshake tipo Noise Protocol para intercambio de
//! claves + cifrado simetrico (ChaCha20-Poly1305) para el stream
//! de video/input.

pub struct SessionCrypto;
