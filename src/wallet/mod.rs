//! Fase 2 — wallet real (cuenta maestra vía WalletConnect + MetaMask).
//! Nada aquí toca fondos todavía: solo pairing y estado de sesión (Vista 8).
//! Sin relación con la wallet watch-only de Fase 1 (esa es solo lectura).

pub mod agent;
pub mod walletconnect;
mod wc_crypto;
