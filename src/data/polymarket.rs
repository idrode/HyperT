//! Cliente REST mínimo de Polymarket — probabilidades de la próxima decisión
//! del FOMC, como CONTEXTO informativo de la Vista 6.
//!
//! IMPORTANTE, honestidad del dato: esto es el precio de un mercado de
//! predicción de POLYMARKET, no la herramienta FedWatch del CME (que se deriva
//! de los futuros de fondos federales). No son lo mismo y no tienen por qué
//! coincidir. La etiqueta "Polymarket" acompaña siempre al dato en la UI.
//!
//! DESACOPLADO A PROPÓSITO: este dato NO entra en el score compuesto de la
//! Vista 6 ni en ninguna otra señal. Es contexto macro aparte, y se mantiene
//! así — si algún día se quisiera ponderar, sería una decisión de diseño
//! explícita, no un efecto colateral de tener el dato a mano.
//!
//! Solo lectura: no toca `src/wallet/` ni `trader.rs`, no firma nada.
//!
//! # Endpoint — verificado en vivo (2026-09-14), no asumido
//!
//! Se usa la **Gamma API** (`gamma-api.polymarket.com`), no la CLOB API, y la
//! razón está comprobada:
//!
//! - `GET clob.polymarket.com/markets` es un volcado paginado por cursor de
//!   1000 en 1000 que empieza en mercados de 2021 — descubrir "la próxima
//!   reunión del FOMC" ahí costaría cientos de peticiones.
//! - `GET clob.polymarket.com/midpoint?token_id=…` sí sirve, pero exige un
//!   `token_id` que solo se obtiene… de la Gamma API.
//! - Se comprobó que el precio de Gamma ES el mid de la CLOB: para el token de
//!   "No change" de septiembre, `outcomePrices` daba `0.205` y `/midpoint`
//!   devolvía exactamente `{"mid":"0.205"}`. Misma cifra, una sola petición.
//!
//! Petición usada (una por ciclo):
//! ```text
//! GET https://gamma-api.polymarket.com/events?closed=false&limit=200&tag_slug=fed-rates
//! ```
//! Respuesta: array de eventos. Los de reunión concreta se titulan
//! "Fed Decision in <Mes>?" y su `endDate` es la fecha de la decisión.
//!
//! GOTCHA verificado: dentro de cada market, `outcomes` y `outcomePrices` NO
//! son arrays JSON — son *strings* que contienen un array JSON codificado
//! (`"[\"Yes\", \"No\"]"`, `"[\"0.205\", \"0.795\"]"`). Hay que parsear dos
//! veces. Asumir que son arrays es el error fácil aquí.
//!
//! GOTCHA verificado: los slugs llevan sufijo aleatorio inestable
//! (`fed-decision-in-september-762`, `fed-decision-in-october-20260617190323537`),
//! así que NO se puede hardcodear un slug ni construirlo a partir del mes. El
//! mercado se descubre por tag + patrón de título + `endDate` más próxima.

use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use super::now_ms;
use super::types::DataMsg;

const GAMMA_URL: &str =
    "https://gamma-api.polymarket.com/events?closed=false&limit=200&tag_slug=fed-rates";

/// Prefijo de título de los eventos de una reunión concreta del FOMC.
const EVENT_TITLE_PREFIX: &str = "Fed Decision in";

/// Cadencia de refresco. Un mercado de predicción sobre una reunión que es
/// cada varias semanas no se mueve en segundos: pedirlo más a menudo sería
/// ruido y carga gratuita sobre una API pública de terceros.
const REFRESH: Duration = Duration::from_secs(300);
/// Reintento corto tras un fallo, para no quedarse 5 minutos sin nada si el
/// primer intento cae justo al arrancar.
const RETRY: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQ_TIMEOUT: Duration = Duration::from_secs(10);

/// Sentido de un tramo de la decisión.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FedKind {
    /// Bajada de tipos.
    Cut,
    /// Sin cambio.
    NoChange,
    /// Subida de tipos.
    Hike,
}

/// Un tramo del mercado ("25 bps decrease" al 0.35%).
#[derive(Clone, Debug, PartialEq)]
pub struct FedBucket {
    pub kind: FedKind,
    /// Magnitud en puntos básicos, si el tramo la expresa (`None` en "No change").
    /// `bps_plus` marca los tramos abiertos ("50+ bps").
    pub bps: Option<u32>,
    pub bps_plus: bool,
    /// Probabilidad implícita en tanto por uno (0.0–1.0): precio del outcome "Yes".
    pub prob: f64,
}

/// Instantánea de las probabilidades de la próxima reunión del FOMC.
#[derive(Clone, Debug, PartialEq)]
pub struct FedOdds {
    /// Título del evento tal cual lo publica Polymarket ("Fed Decision in September?").
    pub title: String,
    /// Fecha de resolución del evento (= fecha de la decisión), en ms epoch.
    pub meeting_ms: u64,
    /// Tramos, ordenados de bajada más fuerte a subida más fuerte.
    pub buckets: Vec<FedBucket>,
    /// Cuándo se trajo este dato (ms epoch), para poder mostrar su antigüedad.
    pub fetched_ms: u64,
}

impl FedOdds {
    /// Probabilidad agregada de bajada / sin cambio / subida.
    pub fn totals(&self) -> (f64, f64, f64) {
        let sum = |k: FedKind| {
            self.buckets
                .iter()
                .filter(|b| b.kind == k)
                .map(|b| b.prob)
                .sum::<f64>()
        };
        (sum(FedKind::Cut), sum(FedKind::NoChange), sum(FedKind::Hike))
    }

    /// Fecha de la reunión en UTC como (año, mes, día), para pintarla sin
    /// arrastrar una dependencia de fechas.
    pub fn meeting_ymd(&self) -> (i64, u32, u32) {
        civil_from_days((self.meeting_ms / 1000) as i64 / 86_400)
    }

    /// Días naturales que faltan para la reunión (0 si ya es hoy o pasó).
    pub fn days_until(&self, now: u64) -> i64 {
        let d = (self.meeting_ms / 1000) as i64 / 86_400 - (now / 1000) as i64 / 86_400;
        d.max(0)
    }

    /// Tramo con mayor probabilidad — el titular del widget.
    pub fn top(&self) -> Option<&FedBucket> {
        self.buckets
            .iter()
            .max_by(|a, b| a.prob.total_cmp(&b.prob))
    }
}

/// Lanza la tarea de fondo. No bloquea el arranque y nunca hace fallar la app:
/// si Polymarket no responde (o está bloqueado por DNS, que es un caso real en
/// algunas redes), simplemente no se emite nada y el widget muestra su estado
/// de "sin datos".
pub fn spawn(tx: UnboundedSender<DataMsg>) {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQ_TIMEOUT)
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        loop {
            let wait = match fetch(&client).await {
                Some(odds) => {
                    // el receptor cerrado significa que la app está saliendo
                    if tx.send(DataMsg::FedOdds(odds)).is_err() {
                        return;
                    }
                    REFRESH
                }
                None => RETRY,
            };
            tokio::time::sleep(wait).await;
        }
    });
}

async fn fetch(client: &reqwest::Client) -> Option<FedOdds> {
    let body: Value = client.get(GAMMA_URL).send().await.ok()?.json().await.ok()?;
    parse_events(&body, now_ms())
}

/// Elige el evento de la reunión más próxima aún por resolver y lo parsea.
///
/// Separado de la red a propósito: es la parte con lógica y la que se testea
/// contra una respuesta real capturada de la API.
pub fn parse_events(body: &Value, now: u64) -> Option<FedOdds> {
    let events = body.as_array()?;
    events
        .iter()
        .filter(|e| {
            e["title"]
                .as_str()
                .is_some_and(|t| t.starts_with(EVENT_TITLE_PREFIX))
                && e["closed"] != Value::Bool(true)
        })
        .filter_map(|e| {
            let end = parse_iso_ms(e["endDate"].as_str()?)?;
            // ya resuelta: no es "la próxima"
            if end <= now {
                return None;
            }
            let buckets = parse_buckets(e["markets"].as_array()?);
            if buckets.is_empty() {
                return None;
            }
            Some(FedOdds {
                title: e["title"].as_str()?.to_string(),
                meeting_ms: end,
                buckets,
                fetched_ms: now,
            })
        })
        // la reunión más próxima en el tiempo
        .min_by_key(|o| o.meeting_ms)
}

fn parse_buckets(markets: &[Value]) -> Vec<FedBucket> {
    let mut out: Vec<FedBucket> = markets
        .iter()
        .filter(|m| m["closed"] != Value::Bool(true))
        .filter_map(parse_bucket)
        .collect();
    // de bajada más fuerte a subida más fuerte, para que el widget se lea como
    // una escala continua en vez de en el orden arbitrario de la API
    out.sort_by_key(|b| match b.kind {
        FedKind::Cut => -(b.bps.unwrap_or(0) as i64),
        FedKind::NoChange => 0,
        FedKind::Hike => b.bps.unwrap_or(0) as i64,
    });
    out
}

fn parse_bucket(m: &Value) -> Option<FedBucket> {
    let label = m["groupItemTitle"].as_str()?;
    let (kind, bps, bps_plus) = classify(label)?;
    let prob = yes_price(m)?;
    Some(FedBucket {
        kind,
        bps,
        bps_plus,
        prob,
    })
}

/// Clasifica un `groupItemTitle` ("25 bps decrease", "50+ bps increase",
/// "No change") en sentido + magnitud.
fn classify(label: &str) -> Option<(FedKind, Option<u32>, bool)> {
    let low = label.to_ascii_lowercase();
    let kind = if low.contains("decrease") || low.contains("cut") {
        FedKind::Cut
    } else if low.contains("increase") || low.contains("hike") {
        FedKind::Hike
    } else if low.contains("no change") {
        return Some((FedKind::NoChange, None, false));
    } else {
        // tramo con una redacción que no reconocemos: mejor descartarlo que
        // clasificarlo mal y enseñar un desglose que no suma lo que dice
        return None;
    };
    let digits: String = low.chars().take_while(|c| c.is_ascii_digit()).collect();
    let bps = digits.parse::<u32>().ok();
    let plus = low[digits.len()..].starts_with('+');
    Some((kind, bps, plus))
}

/// Precio del outcome "Yes" = probabilidad implícita.
///
/// `outcomes` y `outcomePrices` llegan como STRINGS con un array JSON dentro
/// (ver gotcha en la cabecera del módulo), así que hay un segundo parseo.
fn yes_price(m: &Value) -> Option<f64> {
    let outcomes: Vec<String> = serde_json::from_str(m["outcomes"].as_str()?).ok()?;
    let prices: Vec<String> = serde_json::from_str(m["outcomePrices"].as_str()?).ok()?;
    let i = outcomes
        .iter()
        .position(|o| o.eq_ignore_ascii_case("yes"))?;
    let p = prices.get(i)?.parse::<f64>().ok()?;
    // un precio fuera de [0,1] significa que el formato cambió: no inventamos
    (0.0..=1.0).contains(&p).then_some(p)
}

/// Parsea las fechas ISO-8601 UTC que devuelve Gamma, en sus dos formatos
/// reales: `2026-09-16T00:00:00Z` y `2026-05-13T21:23:09.737806Z`.
///
/// A mano y no con `chrono` porque el proyecto no lo tiene como dependencia y
/// añadir una entera por un solo campo no compensa.
fn parse_iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo as u32, d as u32);
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec;
    u64::try_from(secs * 1000).ok()
}

/// Días desde 1970-01-01 (algoritmo civil_from_days de Howard Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverso de `days_from_civil` (Howard Hinnant), para poder pintar la fecha
/// de la reunión.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Respuesta REAL de la Gamma API capturada el 2026-09-14 (recortada a los
    /// eventos "Fed Decision in …" y a los campos que consume el parser).
    const FIXTURE: &str = include_str!("../../tests/fixtures/polymarket_fed_events.json");

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("fixture válido")
    }

    /// 2026-09-14T00:00:00Z, el día en que se capturó el fixture.
    const NOW: u64 = 1_789_344_000_000;

    #[test]
    fn fechas_iso_en_los_dos_formatos_reales() {
        // 2026-09-16T00:00:00Z
        assert_eq!(parse_iso_ms("2026-09-16T00:00:00Z"), Some(1_789_516_800_000));
        // con fracción de segundo, como los startDate
        assert_eq!(
            parse_iso_ms("2026-05-13T21:23:09.737806Z"),
            parse_iso_ms("2026-05-13T21:23:09Z")
        );
        // ancla conocida
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ms("2000-03-01T00:00:00Z"), Some(951_868_800_000));
        // basura -> None, nunca un timestamp inventado
        assert_eq!(parse_iso_ms("no es una fecha"), None);
        assert_eq!(parse_iso_ms("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn elige_la_reunion_mas_proxima_no_la_primera_de_la_lista() {
        let o = parse_events(&fixture(), NOW).expect("hay evento vivo");
        assert_eq!(o.title, "Fed Decision in September?");
        assert_eq!(o.meeting_ms, 1_789_516_800_000); // 2026-09-16
    }

    /// El GOTCHA central: outcomes/outcomePrices son strings con JSON dentro.
    #[test]
    fn probabilidades_reales_del_fixture() {
        let o = parse_events(&fixture(), NOW).unwrap();
        let probs: Vec<f64> = o.buckets.iter().map(|b| b.prob).collect();
        // orden: 50+ baja, 25 baja, sin cambio, 25 sube, 50+ sube
        assert_eq!(probs, vec![0.0005, 0.0035, 0.205, 0.785, 0.0065]);
        let kinds: Vec<FedKind> = o.buckets.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                FedKind::Cut,
                FedKind::Cut,
                FedKind::NoChange,
                FedKind::Hike,
                FedKind::Hike
            ]
        );
        assert_eq!(o.buckets[0].bps, Some(50));
        assert!(o.buckets[0].bps_plus);
        assert_eq!(o.buckets[1].bps, Some(25));
        assert!(!o.buckets[1].bps_plus);
        assert_eq!(o.buckets[2].bps, None);
    }

    #[test]
    fn totales_y_titular() {
        let o = parse_events(&fixture(), NOW).unwrap();
        let (cut, same, hike) = o.totals();
        assert!((cut - 0.004).abs() < 1e-9);
        assert!((same - 0.205).abs() < 1e-9);
        assert!((hike - 0.7915).abs() < 1e-9);
        // OJO: los tramos NO suman exactamente 1. Aquí suman 1.0005, porque
        // son precios de mercado reales (con su spread), no una distribución
        // normalizada. Por eso el widget muestra cada tramo tal cual y no
        // intenta "arreglar" la suma: normalizarla daría una falsa precisión.
        let total = cut + same + hike;
        assert!((total - 1.0).abs() < 0.02, "suma inesperada: {total}");
        assert!((total - 1.0005).abs() < 1e-9);
        let top = o.top().unwrap();
        assert_eq!(top.kind, FedKind::Hike);
        assert_eq!(top.bps, Some(25));
    }

    /// Si la reunión de septiembre ya pasó, debe pasar a la de octubre en vez
    /// de seguir enseñando una fecha vencida.
    #[test]
    fn reunion_pasada_se_descarta() {
        // 2026-09-20, después del 16 y antes del 28 de octubre
        let o = parse_events(&fixture(), 1_789_862_400_000).unwrap();
        assert_eq!(o.title, "Fed Decision in October?");
    }

    #[test]
    fn sin_eventos_vivos_devuelve_none() {
        // muy en el futuro: ninguna reunión del fixture sigue pendiente
        assert_eq!(parse_events(&fixture(), 4_000_000_000_000), None);
        // y una respuesta que no es lo que esperamos tampoco revienta
        assert_eq!(parse_events(&Value::Null, NOW), None);
        assert_eq!(parse_events(&serde_json::json!([]), NOW), None);
        assert_eq!(parse_events(&serde_json::json!([{"title": 3}]), NOW), None);
    }

    #[test]
    fn fecha_de_reunion_y_cuenta_atras() {
        let o = parse_events(&fixture(), NOW).unwrap();
        assert_eq!(o.meeting_ymd(), (2026, 9, 16));
        assert_eq!(o.days_until(NOW), 2);
        // ya pasada: nunca negativo
        assert_eq!(o.days_until(4_000_000_000_000), 0);
    }

    /// `civil_from_days` debe ser exactamente el inverso de `days_from_civil`.
    #[test]
    fn ida_y_vuelta_de_fechas() {
        for &(y, m, d) in &[
            (1970, 1, 1),
            (2000, 2, 29),
            (2026, 9, 16),
            (2026, 12, 31),
            (2027, 3, 1),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn clasificacion_de_tramos() {
        assert_eq!(classify("No change"), Some((FedKind::NoChange, None, false)));
        assert_eq!(
            classify("25 bps decrease"),
            Some((FedKind::Cut, Some(25), false))
        );
        assert_eq!(
            classify("50+ bps increase"),
            Some((FedKind::Hike, Some(50), true))
        );
        // redacción desconocida: se descarta, no se adivina
        assert_eq!(classify("something else"), None);
    }

    #[test]
    fn precio_fuera_de_rango_se_descarta() {
        let m = serde_json::json!({
            "groupItemTitle": "No change",
            "outcomes": "[\"Yes\", \"No\"]",
            "outcomePrices": "[\"12.0\", \"0.0\"]",
        });
        assert_eq!(parse_bucket(&m), None);
        // y si outcomes/prices llegaran como arrays de verdad (cambio de API),
        // tampoco se inventa nada
        let m2 = serde_json::json!({
            "groupItemTitle": "No change",
            "outcomes": ["Yes", "No"],
            "outcomePrices": ["0.5", "0.5"],
        });
        assert_eq!(parse_bucket(&m2), None);
    }
}
