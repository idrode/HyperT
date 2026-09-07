//! Agent wallet (paso 6 de Fase 2): generación local de la clave de trading
//! y su almacenamiento en disco, FUERA de git (`/secrets/` está en el
//! .gitignore desde antes de que exista ningún archivo).
//!
//! La agent key firma las órdenes del día a día como `LocalWallet` del SDK
//! (paso 7); la autoriza UNA firma EIP-712 de la cuenta maestra vía
//! WalletConnect (`walletconnect::request_agent`). Hyperliquid concede a los
//! agents permiso de TRADING solamente — un agent no puede retirar ni
//! transferir fondos (por diseño del protocolo, no de esta app).
//!
//! Orden de persistencia pensado para no perder nunca una clave autorizada:
//! 1. `save_pending` escribe la clave nueva en `<ruta>.pending` ANTES de
//!    pedir la firma — si la app muere con la aprobación ya enviada, la clave
//!    sigue en disco y se puede promover a mano.
//! 2. `promote` renombra a la ruta final SOLO cuando /exchange respondió ok
//!    (la clave anterior, ya invalidada por el servidor, se sobreescribe).
//! 3. `discard_pending` limpia si la firma falla o se rechaza.
//!
//! NUNCA loguear ni renderizar `priv_hex` — solo la dirección pública.

use std::fs;
use std::path::PathBuf;

/// Expiración por defecto que Hyperliquid aplica a un agent aprobado SIN
/// `valid_until` en el nombre: 90 días desde la aprobación. Solo se usa como
/// fallback para claves guardadas antes de que esta app fijara la expiración.
pub const DEFAULT_AGENT_TTL_MS: u64 = 90 * 24 * 60 * 60 * 1000;

/// Máximo que el protocolo acepta como `valid_until`: 180 días en el futuro.
/// Es lo que fija esta app al autorizar (el riesgo de la key está acotado —
/// sin permiso de retiro — y así se reautoriza la mitad de veces).
pub const MAX_AGENT_TTL_MS: u64 = 180 * 24 * 60 * 60 * 1000;

/// Desde cuándo se enseña la cuenta atrás en el panel (día −20). Antes de
/// esto la línea solo dice la fecha, sin urgencia: avisar demasiado pronto
/// entrena a ignorar el aviso.
pub const COUNTDOWN_WINDOW_MS: u64 = 20 * 24 * 60 * 60 * 1000;

/// Umbral del color ámbar (día −10): el relevo ya debería estar planificado.
pub const AMBER_MS: u64 = 10 * 24 * 60 * 60 * 1000;

/// Desde el día −5 sin relevo se BLOQUEAN las entradas nuevas. Cerrar,
/// cancelar y editar SL/TP siguen sin restricción hasta el último momento de
/// validez real: quedarse sin poder proteger una posición abierta sería peor
/// que abrir una entrada tarde.
pub const BLOCK_ENTRIES_MS: u64 = 5 * 24 * 60 * 60 * 1000;

use alloy_primitives::{keccak256, Address};
use anyhow::{Context, Result};
use k256::ecdsa::SigningKey;

/// Tramo de vida de la autorización, con el color/urgencia que le toca en la
/// Vista 8. Es una función pura del tiempo restante — testeada abajo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Life {
    /// Más de 20 días: sin cuenta atrás, solo la fecha.
    Fresh,
    /// −20 a −10 días: cuenta atrás en verde, informativa.
    Countdown,
    /// −10 a −5 días: ámbar, conviene activar el relevo ya.
    Amber,
    /// −5 días a la expiración: urgente. Entradas nuevas BLOQUEADAS.
    Urgent,
    /// Ya caducada: las firmas fallan del lado del servidor.
    Expired,
}

impl Life {
    /// Tramo correspondiente al tiempo que queda (ms, negativo si caducó).
    pub fn from_left_ms(left_ms: i64) -> Life {
        if left_ms <= 0 {
            Life::Expired
        } else if (left_ms as u64) < BLOCK_ENTRIES_MS {
            Life::Urgent
        } else if (left_ms as u64) < AMBER_MS {
            Life::Amber
        } else if (left_ms as u64) < COUNTDOWN_WINDOW_MS {
            Life::Countdown
        } else {
            Life::Fresh
        }
    }

    /// ¿Se dibuja la cuenta atrás (día −20 en adelante)?
    pub fn shows_countdown(self) -> bool {
        !matches!(self, Life::Fresh)
    }

    /// ¿Se bloquean las ENTRADAS nuevas? Solo entradas: cerrar/cancelar/
    /// editar SL-TP nunca se bloquean por expiración.
    pub fn blocks_entries(self) -> bool {
        matches!(self, Life::Urgent | Life::Expired)
    }
}

/// Días completos que quedan (redondeando hacia abajo); 0 si ya caducó.
pub fn days_left(left_ms: i64) -> u64 {
    if left_ms <= 0 {
        0
    } else {
        (left_ms as u64) / 86_400_000
    }
}

/// Ahora en epoch ms (0 si el reloj del sistema está antes de 1970).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Clave recién generada, aún sin autorizar. La clave privada vive solo en
/// memoria (y en el `.pending` una vez pedida la firma).
pub struct FreshAgent {
    /// Dirección pública, checksummed EIP-55 — la que autoriza la maestra.
    pub address: String,
    /// Clave privada en hex (0x + 64). No es `pub` fuera de `wallet`.
    pub(crate) priv_hex: String,
}

/// Genera una clave secp256k1 nueva con el RNG del sistema operativo.
pub fn generate() -> FreshAgent {
    let sk = SigningKey::random(&mut rand::rngs::OsRng);
    let address = derive_address(&sk);
    FreshAgent {
        address: format!("{address}"),
        priv_hex: format!("0x{}", alloy_primitives::hex::encode(sk.to_bytes())),
    }
}

/// Dirección Ethereum de una clave: keccak256 del punto público sin comprimir
/// (sin el byte 0x04 de prefijo), últimos 20 bytes.
fn derive_address(sk: &SigningKey) -> Address {
    let pubkey = sk.verifying_key().to_encoded_point(false);
    let hash = keccak256(&pubkey.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

/// Ruta del archivo de la agent key: `secrets/` relativo al directorio de
/// trabajo (la app se lanza desde la raíz del repo; el .gitignore cubre esa
/// ruta), con override por `HYPERT_SECRETS_DIR`. Una clave por red:
/// autorizar en testnet no debe pisar la clave de mainnet ni al revés.
pub fn key_path(hl_chain: &str) -> PathBuf {
    let net = if hl_chain == "Mainnet" {
        "mainnet"
    } else {
        "testnet"
    };
    let dir = std::env::var("HYPERT_SECRETS_DIR").unwrap_or_else(|_| "secrets".to_string());
    PathBuf::from(dir).join(format!("agent_{net}.json"))
}

fn pending_path(hl_chain: &str) -> PathBuf {
    let mut p = key_path(hl_chain).into_os_string();
    p.push(".pending");
    PathBuf::from(p)
}

/// Dirección del agent ya autorizado para esta red, si hay clave guardada
/// (solo lee la dirección — la clave privada no entra al estado de la app).
pub fn existing_agent(hl_chain: &str) -> Option<String> {
    let raw = fs::read_to_string(key_path(hl_chain)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v["agent_address"].as_str().map(str::to_string)
}

/// Agent autorizado cargado COMPLETO (con clave privada) para firmar órdenes
/// del panel de ejecución real (paso 7). La clave solo debe viajar a la tarea
/// del trader — nunca al estado de la App ni a ningún log/render.
pub struct LoadedAgent {
    /// Cuenta maestra dueña de las posiciones/órdenes (checksummed).
    pub master: Address,
    /// Dirección pública del agent (solo informativa).
    pub address: String,
    /// Clave privada hex (0x + 64). No es `pub` fuera de `wallet`/crate.
    pub(crate) priv_hex: String,
    /// Cuándo caduca la autorización (epoch ms). Con `valid_until_ms` en el
    /// archivo es exacto; sin él (claves autorizadas antes de fijar expiración
    /// explícita) es el default del protocolo: aprobación + 90 días. None solo
    /// si el archivo tampoco trae `approved_nonce_ms` (no debería ocurrir).
    pub expires_ms: Option<u64>,
    /// ¿La expiración es EXPLÍCITA (`valid_until_ms` en el archivo, es decir
    /// autorizada por esta app con el flujo nombrado "hypert")? Los agents
    /// antiguos, sin nombre, no aparecen necesariamente en `extraAgents`, así
    /// que su ausencia allí NO se puede tratar como una revocación.
    pub explicit_expiry: bool,
}

/// Expiración a partir del JSON de la clave: `valid_until_ms` explícito, o
/// el default del protocolo (aprobación + 90d) para archivos antiguos.
fn expiry_from_json(v: &serde_json::Value) -> Option<u64> {
    v["valid_until_ms"].as_u64().or_else(|| {
        v["approved_nonce_ms"]
            .as_u64()
            .map(|n| n + DEFAULT_AGENT_TTL_MS)
    })
}

/// Carga la clave del agent de esta red, verificando que el archivo es de la
/// red pedida (autorizar en testnet no debe firmar jamás contra mainnet).
/// None = sin clave autorizada aún, o archivo ilegible/incoherente.
pub fn load(hl_chain: &str) -> Option<LoadedAgent> {
    let raw = fs::read_to_string(key_path(hl_chain)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    if v["hyperliquid_chain"].as_str() != Some(hl_chain) {
        return None;
    }
    let master: Address = v["master"].as_str()?.parse().ok()?;
    let priv_hex = v["private_key"].as_str()?.to_string();
    // coherencia clave↔dirección: una clave corrupta no debe firmar nada
    let bytes = alloy_primitives::hex::decode(&priv_hex).ok()?;
    let sk = SigningKey::from_slice(&bytes).ok()?;
    let derived = format!("{}", derive_address(&sk));
    let address = v["agent_address"].as_str()?.to_string();
    if derived != address {
        return None;
    }
    Some(LoadedAgent {
        master,
        address,
        priv_hex,
        expires_ms: expiry_from_json(&v),
        explicit_expiry: v["valid_until_ms"].as_u64().is_some(),
    })
}

/// Qué dice EL SERVIDOR sobre el agent que el panel tiene armado. El cálculo
/// local de la expiración sale del archivo en disco, que puede estar
/// desactualizado (agent revocado desde la web, reemplazado por otro cliente,
/// o una fecha mal supuesta): esta comprobación es la fuente autoritativa.
#[derive(Debug, Clone, PartialEq)]
pub enum Registration {
    /// Consulta en vuelo (o aún sin lanzar).
    Checking,
    /// `extraAgents` lo lista. `valid_until_ms` es la expiración QUE DICE EL
    /// SERVIDOR cuando la trae — manda sobre el cálculo local.
    Listed { valid_until_ms: Option<u64> },
    /// El servidor respondió bien y el agent NO está en la lista: la clave de
    /// disco ya no vale para firmar, por mucho que la fecha local diga que sí.
    NotListed,
    /// No se pudo comprobar (red/servidor). NO es lo mismo que NotListed: no
    /// se bloquea nada por esto, solo se dice que no se pudo verificar.
    Unknown { error: String },
}

/// Consulta `extraAgents` del Info API y dice si `agent` sigue registrado
/// para `master`. Mismo endpoint que ya usa la verificación posterior a una
/// autorización — aquí se reutiliza para el agent YA guardado.
pub async fn check_registration(api: &str, master: &str, agent: &str) -> Registration {
    let body = serde_json::json!({"type": "extraAgents", "user": master});
    let resp = reqwest::Client::new()
        .post(format!("{api}/info"))
        .json(&body)
        .send()
        .await;
    let json: serde_json::Value = match resp {
        Ok(r) => match r.json().await {
            Ok(j) => j,
            Err(e) => {
                return Registration::Unknown {
                    error: format!("respuesta ilegible de extraAgents: {e}"),
                }
            }
        },
        Err(e) => {
            return Registration::Unknown {
                error: format!("no se pudo consultar extraAgents: {e}"),
            }
        }
    };
    let Some(list) = json.as_array() else {
        return Registration::Unknown {
            error: "extraAgents no devolvió una lista".into(),
        };
    };
    match find_agent(list, agent) {
        Some(valid_until_ms) => Registration::Listed { valid_until_ms },
        None => Registration::NotListed,
    }
}

/// Busca el agent en la respuesta de `extraAgents` (comparación de direcciones
/// insensible a mayúsculas: el servidor no garantiza el checksum EIP-55) y
/// devuelve su `validUntil` si lo trae. Separado para poder testearlo sin red.
fn find_agent(list: &[serde_json::Value], agent: &str) -> Option<Option<u64>> {
    let want = agent.to_lowercase();
    let e = list.iter().find(|a| {
        a["address"]
            .as_str()
            .is_some_and(|x| x.to_lowercase() == want)
    })?;
    // el campo llega como número o como string según la versión del API
    let vu = e["validUntil"]
        .as_u64()
        .or_else(|| e["validUntil"].as_str().and_then(|s| s.parse().ok()));
    Some(vu)
}

/// Escribe la clave nueva en `<ruta>.pending` con permisos 0600 (y el
/// directorio a 0700), ANTES de pedir la firma a la maestra.
pub fn save_pending(
    hl_chain: &str,
    master: &str,
    agent_address: &str,
    priv_hex: &str,
    nonce: u64,
    valid_until_ms: u64,
) -> Result<()> {
    let path = pending_path(hl_chain);
    let dir = path.parent().context("ruta sin directorio")?;
    fs::create_dir_all(dir).with_context(|| format!("creando {}", dir.display()))?;
    let body = serde_json::json!({
        "agent_address": agent_address,
        "master": master,
        "hyperliquid_chain": hl_chain,
        "approved_nonce_ms": nonce,
        "valid_until_ms": valid_until_ms,
        "private_key": priv_hex,
    });
    fs::write(&path, format!("{body:#}\n"))
        .with_context(|| format!("escribiendo {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("permisos de {}", path.display()))?;
    }
    Ok(())
}

/// /exchange respondió ok: la clave pending pasa a ser LA clave del agent de
/// esta red (el rename sobreescribe la anterior, que el servidor ya invalidó).
pub fn promote(hl_chain: &str) -> Result<PathBuf> {
    let from = pending_path(hl_chain);
    let to = key_path(hl_chain);
    fs::rename(&from, &to)
        .with_context(|| format!("promoviendo {} → {}", from.display(), to.display()))?;
    Ok(to)
}

/// La firma falló o se rechazó: la clave pending nunca se autorizó y se borra.
pub fn discard_pending(hl_chain: &str) {
    let _ = fs::remove_file(pending_path(hl_chain));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vector conocido: la clave privada 0x…01 tiene la dirección
    /// 0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf (verificable en cualquier
    /// herramienta Ethereum) — valida la derivación completa.
    #[test]
    fn derivacion_de_direccion_conocida() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let sk = SigningKey::from_bytes(&sk.into()).unwrap();
        assert_eq!(
            format!("{}", derive_address(&sk)),
            "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
        );
    }

    #[test]
    fn generar_da_clave_y_direccion_coherentes() {
        let a = generate();
        assert!(a.address.starts_with("0x") && a.address.len() == 42);
        assert!(a.priv_hex.starts_with("0x") && a.priv_hex.len() == 66);
        // la dirección publicada corresponde a la clave privada devuelta
        let bytes = alloy_primitives::hex::decode(&a.priv_hex).unwrap();
        let sk = SigningKey::from_slice(&bytes).unwrap();
        assert_eq!(format!("{}", derive_address(&sk)), a.address);
        // dos generaciones nunca coinciden (RNG del sistema)
        assert_ne!(generate().priv_hex, a.priv_hex);
    }

    /// Los tramos de vida y el bloqueo de entradas caen donde dice el diseño:
    /// cuenta atrás desde −20d, ámbar desde −10d, urgente + entradas
    /// bloqueadas desde −5d, y caducada al pasar la fecha. En NINGÚN tramo se
    /// bloquea cerrar/cancelar/SL-TP (eso no se decide aquí, pero el único
    /// predicado que existe se llama `blocks_entries` a propósito).
    #[test]
    fn tramos_de_vida_y_bloqueo_de_entradas() {
        let d = |n: f64| (n * 86_400_000.0) as i64;
        assert_eq!(Life::from_left_ms(d(45.0)), Life::Fresh);
        assert_eq!(Life::from_left_ms(d(20.1)), Life::Fresh);
        assert_eq!(Life::from_left_ms(d(19.9)), Life::Countdown);
        assert_eq!(Life::from_left_ms(d(10.1)), Life::Countdown);
        assert_eq!(Life::from_left_ms(d(9.9)), Life::Amber);
        assert_eq!(Life::from_left_ms(d(5.1)), Life::Amber);
        assert_eq!(Life::from_left_ms(d(4.9)), Life::Urgent);
        assert_eq!(Life::from_left_ms(d(0.01)), Life::Urgent);
        assert_eq!(Life::from_left_ms(0), Life::Expired);
        assert_eq!(Life::from_left_ms(d(-3.0)), Life::Expired);

        // la cuenta atrás aparece exactamente a partir del día −20
        assert!(!Life::Fresh.shows_countdown());
        for l in [Life::Countdown, Life::Amber, Life::Urgent, Life::Expired] {
            assert!(l.shows_countdown());
        }
        // y el bloqueo de entradas SOLO en los dos últimos tramos
        for l in [Life::Fresh, Life::Countdown, Life::Amber] {
            assert!(!l.blocks_entries(), "{l:?} no debe bloquear entradas");
        }
        for l in [Life::Urgent, Life::Expired] {
            assert!(l.blocks_entries(), "{l:?} debe bloquear entradas");
        }

        assert_eq!(days_left(d(19.9)), 19);
        assert_eq!(days_left(d(0.5)), 0);
        assert_eq!(days_left(-1), 0);
    }

    /// Lectura de extraAgents: dirección insensible a mayúsculas, validUntil
    /// aceptado como número o como string, y "no está en la lista" distinguido
    /// de "está pero sin validUntil".
    #[test]
    fn lectura_de_extra_agents() {
        let list: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
                {"address":"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"hypert","validUntil":1790000000000},
                {"address":"0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","name":"otro","validUntil":"1795000000000"},
                {"address":"0xcccccccccccccccccccccccccccccccccccccccc","name":"viejo"}
            ]"#,
        )
        .unwrap();
        // checksum distinto al del servidor: debe encontrarlo igualmente
        assert_eq!(
            find_agent(&list, "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            Some(Some(1_790_000_000_000))
        );
        assert_eq!(
            find_agent(&list, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            Some(Some(1_795_000_000_000))
        );
        // listado pero sin validUntil ≠ no listado
        assert_eq!(
            find_agent(&list, "0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"),
            Some(None)
        );
        assert_eq!(
            find_agent(&list, "0xdddddddddddddddddddddddddddddddddddddddd"),
            None
        );
    }

    /// Ciclo completo pending → promote con lectura de la dirección, en un
    /// directorio temporal vía HYPERT_SECRETS_DIR (env global al proceso —
    /// por eso todo el ciclo va en un único test, no repartido en varios).
    #[test]
    fn ciclo_pending_promote_y_permisos() {
        let tmp = std::env::temp_dir().join(format!("hypert_agent_test_{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("HYPERT_SECRETS_DIR", &tmp);

        let out = (|| -> Result<()> {
            save_pending(
                "Testnet",
                "0xMASTER",
                "0xAGENT",
                "0xkey",
                123,
                123 + MAX_AGENT_TTL_MS,
            )?;
            // aún no promovida: no cuenta como agent existente
            assert_eq!(existing_agent("Testnet"), None);
            let path = promote("Testnet")?;
            assert_eq!(path, key_path("Testnet"));
            assert_eq!(existing_agent("Testnet"), Some("0xAGENT".to_string()));
            // mainnet y testnet no comparten archivo
            assert_eq!(existing_agent("Mainnet"), None);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600, "la clave debe quedar a 0600");
            }
            // descartar sin pending no revienta
            discard_pending("Testnet");

            // load (paso 7): una clave real recién generada se carga con la
            // maestra parseada y la coherencia clave↔dirección verificada
            let a = generate();
            save_pending(
                "Mainnet",
                "0x000000000000000000000000000000000000dEaD",
                &a.address,
                &a.priv_hex,
                1,
                1 + MAX_AGENT_TTL_MS,
            )?;
            promote("Mainnet")?;
            let l = load("Mainnet").expect("clave coherente debe cargar");
            assert_eq!(l.address, a.address);
            assert_eq!(l.priv_hex, a.priv_hex);
            // valid_until_ms explícito manda sobre el fallback de 90d
            assert_eq!(l.expires_ms, Some(1 + MAX_AGENT_TTL_MS));
            assert_eq!(
                format!("{}", l.master),
                "0x000000000000000000000000000000000000dEaD"
            );
            // archivo antiguo SIN valid_until_ms (clave autorizada antes de
            // fijar expiración explícita): fallback = aprobación + 90 días
            let legacy = serde_json::json!({
                "agent_address": a.address,
                "master": "0x000000000000000000000000000000000000dEaD",
                "hyperliquid_chain": "Mainnet",
                "approved_nonce_ms": 1000,
                "private_key": a.priv_hex,
            });
            fs::write(key_path("Mainnet"), legacy.to_string())?;
            assert_eq!(
                load("Mainnet").unwrap().expires_ms,
                Some(1000 + DEFAULT_AGENT_TTL_MS)
            );
            // la red del archivo manda: pedir Testnet no carga el de Mainnet
            // (el archivo Testnet del ciclo anterior tiene clave basura y
            //  tampoco carga — la coherencia lo rechaza)
            assert!(load("Testnet").is_none());
            Ok(())
        })();

        std::env::remove_var("HYPERT_SECRETS_DIR");
        let _ = fs::remove_dir_all(&tmp);
        out.unwrap();
    }
}
