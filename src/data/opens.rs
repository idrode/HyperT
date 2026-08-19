//! Reconstrucción de la fecha de apertura de las posiciones abiertas de una
//! dirección observada (Vista 9).
//!
//! El problema: `userFills` corta en 2000 registros, que en una cuenta activa
//! son minutos. Verificado contra la whale `0xb83d…6e36` (2026-08-18): sus 2000
//! fills cubrían 100 minutos, y paginando `userFillsByTime` hasta agotar TODO
//! el historial que la API conserva salían 18.854 fills… que cubren 23 horas.
//! Con posiciones abiertas desde mayo/julio, por esa vía no hay ningún cruce
//! por cero que encontrar: el dato no existe en el endpoint.
//!
//! La vía que sí llega: `userFunding` conserva meses (comprobado hasta abril) y
//! trae `usdc` por par. Como en este proyecto ya está verificado que
//! `cumFunding.sinceOpen = −Σ userFunding.usdc` del tramo abierto, se acumula
//! hacia atrás hasta AGOTAR ese `sinceOpen`: el evento donde el acumulado cruza
//! el total es el principio del tramo actual. Precisión: la del evento de
//! funding (horas), no la del fill.
//!
//! Se combinan las dos, de más firme a menos:
//!   1. `userFillsByTime` paginado hacia atrás → cruce por cero exacto
//!      (`OpenKind::Exact`). En cuentas normales resuelve casi todo.
//!   2. `userFunding` acumulado hacia atrás → `OpenKind::Funding` (≈).
//!   3. Lo que no se resuelve queda como `OpenKind::LowerBound` (≥) con el
//!      instante más antiguo barrido: honesto, y ya mucho mejor que la ventana
//!      de 2000 fills.
//!
//! Gotchas del endpoint, todos verificados con llamadas reales antes de asumir:
//! - `userFunding`/`userFillsByTime` devuelven ASCENDENTE por tiempo y cortan
//!   en 500/2000 entradas; el corte se lleva las MÁS RECIENTES de la ventana
//!   pedida, así que una ventana saturada deja un agujero silencioso (esto hizo
//!   parecer que 13 posiciones se habían abierto a la vez en la misma hora).
//!   Por eso una ventana saturada se descarta y se reintenta más estrecha.
//! - Los eventos de funding se AGREGAN (`nSamples`): puede haber 24h sin
//!   eventos con la posición abierta todo el rato, así que "hueco = cerrada" es
//!   una heurística falsa. De ahí el criterio contable de `sinceOpen`.

use std::collections::HashMap;

use super::types::{FillInfo, OpenEst, OpenKind};

/// Tope de peticiones por barrido de funding. A ~150ms cada una son ~25s en
/// segundo plano en el peor caso medido (whale con 13 posiciones abiertas desde
/// hace hasta 3 meses: resolvió 11 de 13). Lo que quede sin resolver se muestra
/// como cota inferior, no se insiste en bucle.
const FUNDING_BUDGET: usize = 150;
/// Tope de páginas de fills (2000 cada una) hacia atrás.
const FILLS_PAGES: usize = 10;
/// No se busca apertura más allá de esto (la cuenta puede llevar años).
const MAX_LOOKBACK_MS: u64 = 400 * 86_400_000;
/// Con un `sinceOpen` así de pequeño el criterio contable es puro ruido
/// (posiciones de polvo, o funding neto ≈0): no se intenta por esa vía.
const MIN_SINCE_OPEN: f64 = 1.0;
/// Si el evento del cruce dispara el acumulado muy por encima del objetivo, la
/// suma no es limpia (funding alternando de signo): no se da por buena.
const OVERSHOOT: f64 = 1.5;

/// Un evento de `userFunding` reducido a lo que importa aquí.
#[derive(Debug, Clone, Copy)]
pub struct FundEv {
    pub time_ms: u64,
    /// `usdc` tal cual lo reporta la API (>0 = la cuenta RECIBE).
    pub usdc: f64,
}

/// Instante en que empieza el tramo abierto, acumulando funding hacia atrás
/// hasta agotar `since_open`. `events` son los eventos del par ORDENADOS DE MÁS
/// RECIENTE A MÁS ANTIGUO. Devuelve el evento donde el acumulado cruza el
/// objetivo, o `None` si no llega a cruzar (hay que seguir barriendo hacia
/// atrás) o si el cruce se pasa tanto que la suma no es fiable.
///
/// Función pura: es el corazón del algoritmo y se testea sin red.
pub fn funding_open(events: &[FundEv], since_open: f64) -> Option<u64> {
    if since_open.abs() < MIN_SINCE_OPEN {
        return None;
    }
    let mut acc = 0.0;
    for e in events {
        acc += -e.usdc;
        // mismo signo y ya cubierto el total: aquí empieza el tramo
        if acc.abs() >= since_open.abs() * 0.999 && (acc > 0.0) == (since_open > 0.0) {
            return (acc.abs() <= since_open.abs() * OVERSHOOT).then_some(e.time_ms);
        }
    }
    None
}

/// Estado del barrido de una dirección, para poder mostrar avances parciales.
struct Scan {
    /// coin → (szi actual, sinceOpen)
    want: Vec<(String, f64, f64)>,
    out: HashMap<String, OpenEst>,
    /// Instante más antiguo alcanzado (suelo de la cota inferior).
    floor: u64,
}

impl Scan {
    fn pending(&self) -> Vec<(String, f64, f64)> {
        self.want
            .iter()
            .filter(|(c, _, _)| !self.out.contains_key(c))
            .cloned()
            .collect()
    }

    /// Rellena con cota inferior todo lo que quedó sin resolver.
    fn finish(mut self) -> HashMap<String, OpenEst> {
        for (coin, _, _) in &self.want {
            self.out.entry(coin.clone()).or_insert(OpenEst {
                ms: self.floor,
                kind: OpenKind::LowerBound,
            });
        }
        self.out
    }
}

/// Reconstruye la apertura de cada posición abierta de `user`.
///
/// `positions` = (par, szi actual, `cumFunding.sinceOpen`). `fills` son los que
/// la app ya tiene (userFills), para no repetir la primera página.
pub async fn resolve(
    client: &reqwest::Client,
    api: &str,
    user: &str,
    positions: &[(String, f64, f64)],
    now_ms: u64,
) -> HashMap<String, OpenEst> {
    let mut scan = Scan {
        want: positions.to_vec(),
        out: HashMap::new(),
        floor: now_ms,
    };

    // ── 1. fills hacia atrás: cruce por cero exacto ─────────────────────────
    let mut fills: Vec<FillInfo> = Vec::new();
    let mut cursor = now_ms.saturating_sub(MAX_LOOKBACK_MS);
    for _ in 0..FILLS_PAGES {
        let page = fetch_fills_by_time(client, api, user, cursor).await;
        let Ok(page) = page else { break };
        if page.is_empty() {
            break;
        }
        let full = page.len() >= 2000;
        let last = page.last().map(|f| f.time_ms).unwrap_or(cursor);
        fills.extend(page);
        if !full {
            break;
        }
        cursor = last + 1;
    }
    if let Some(oldest) = fills.iter().map(|f| f.time_ms).min() {
        scan.floor = oldest;
    }
    // `position_open_time` espera el orden de userFills: más reciente primero.
    fills.sort_by(|a, b| b.time_ms.cmp(&a.time_ms));
    for (coin, szi, _) in &scan.want.clone() {
        if let Some((ms, true)) = crate::ui::wallet::position_open_time(&fills, coin, *szi) {
            scan.out.insert(
                coin.clone(),
                OpenEst {
                    ms,
                    kind: OpenKind::Exact,
                },
            );
        }
    }

    // ── 2. funding hacia atrás para lo que quede ────────────────────────────
    if scan.pending().is_empty() {
        return scan.finish();
    }
    // Eventos por par, acumulados de más reciente a más antiguo conforme se
    // barre; se reevalúa el criterio contable tras cada ventana.
    let mut ev: HashMap<String, Vec<FundEv>> = HashMap::new();
    // Los extremos de ventana son inclusivos en ambos lados y el cursor de
    // paginación no puede saltar por ms (varios pares comparten timestamp), así
    // que el mismo evento puede llegar dos veces: sin deduplicar, la suma de
    // funding se infla y la apertura sale más antigua de lo que es (visto en
    // vivo: el mismo par daba 40d en una pasada y 74d en la siguiente).
    let mut seen: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
    let mut end = now_ms;
    let mut delta: u64 = 2 * 86_400_000;
    let mut used = 0usize;
    while used < FUNDING_BUDGET && !scan.pending().is_empty() {
        let start = end.saturating_sub(delta);
        if now_ms.saturating_sub(start) > MAX_LOOKBACK_MS {
            break;
        }
        // Una ventana con más de 500 eventos se devuelve TRUNCADA por el final
        // (se pierden los más recientes del rango), lo que abriría un agujero
        // silencioso en la contabilidad. Se pagina hacia adelante dentro de la
        // ventana hasta cubrirla entera: así no se tira ninguna petición.
        let mut page: Vec<(String, FundEv)> = Vec::new();
        let mut cursor = start;
        let mut saturated = false;
        while used < FUNDING_BUDGET {
            let Ok(chunk) = fetch_funding(client, api, user, cursor, end).await else {
                break;
            };
            used += 1;
            let full = chunk.len() >= 500;
            let last = chunk.last().map(|(_, e)| e.time_ms);
            page.extend(chunk);
            match (full, last) {
                (true, Some(t)) => {
                    saturated = true;
                    // no se salta el ms del último evento (otros pares pueden
                    // compartirlo); el dedupe de abajo cubre el solape, y si el
                    // cursor no avanzara se rompería el bucle
                    cursor = if t > cursor { t } else { t + 1 };
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
                _ => break,
            }
        }
        let sparse = page.len() < 250;
        // llegan ascendentes: hacia atrás es del final al principio
        page.sort_by_key(|(_, e)| std::cmp::Reverse(e.time_ms));
        for (coin, e) in page {
            if seen.insert((coin.clone(), e.time_ms)) {
                ev.entry(coin).or_default().push(e);
            }
        }
        for (coin, _, since_open) in scan.pending() {
            if let Some(evs) = ev.get(&coin) {
                if let Some(ms) = funding_open(evs, since_open) {
                    scan.out.insert(
                        coin,
                        OpenEst {
                            ms,
                            kind: OpenKind::Funding,
                        },
                    );
                }
            }
        }
        scan.floor = scan.floor.min(start);
        // el paso se adapta a la densidad real de eventos de esta cuenta
        if saturated {
            delta = (delta / 2).max(3 * 3_600_000);
        } else if sparse {
            delta = (delta * 2).min(20 * 86_400_000);
        }
        end = start;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    scan.finish()
}

/// Una página de `userFillsByTime` (ascendente, tope 2000).
async fn fetch_fills_by_time(
    client: &reqwest::Client,
    api: &str,
    user: &str,
    start_ms: u64,
) -> Result<Vec<FillInfo>, String> {
    let v = super::info_post(
        client,
        api,
        serde_json::json!({
            "type": "userFillsByTime",
            "user": user,
            "startTime": start_ms,
        }),
    )
    .await?;
    let arr = v
        .as_array()
        .ok_or_else(|| format!("respuesta inesperada: {v}"))?;
    Ok(arr.iter().filter_map(super::parse_fill_json).collect())
}

/// Una página de `userFunding` (ascendente, tope 500), ya reducida a
/// (par, evento).
async fn fetch_funding(
    client: &reqwest::Client,
    api: &str,
    user: &str,
    start_ms: u64,
    end_ms: u64,
) -> Result<Vec<(String, FundEv)>, String> {
    let v = super::info_post(
        client,
        api,
        serde_json::json!({
            "type": "userFunding",
            "user": user,
            "startTime": start_ms,
            "endTime": end_ms,
        }),
    )
    .await?;
    let arr = v
        .as_array()
        .ok_or_else(|| format!("respuesta inesperada: {v}"))?;
    Ok(arr
        .iter()
        .filter_map(|x| {
            let d = x.get("delta")?;
            Some((
                d.get("coin")?.as_str()?.to_string(),
                FundEv {
                    time_ms: x.get("time")?.as_u64()?,
                    usdc: d.get("usdc")?.as_str()?.parse().ok()?,
                },
            ))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: u64, usdc: f64) -> FundEv {
        FundEv { time_ms: t, usdc }
    }

    /// Caso nominal: la cuenta PAGÓ 30 de funding (sinceOpen>0 = pagado) en
    /// tres eventos de −10; el tercero hacia atrás agota el total y marca el
    /// principio del tramo. Los anteriores son de un tramo ya cerrado y no
    /// deben arrastrar la fecha más atrás.
    #[test]
    fn apertura_por_funding_agota_el_since_open() {
        let evs = vec![
            ev(900, -10.0),
            ev(800, -10.0),
            ev(700, -10.0),
            ev(600, -10.0),
            ev(500, -10.0),
        ];
        assert_eq!(funding_open(&evs, 30.0), Some(700));
    }

    /// Mientras el acumulado no alcanza el total, no hay respuesta: hay que
    /// seguir barriendo hacia atrás en vez de inventar una fecha.
    #[test]
    fn sin_cubrir_el_total_no_resuelve() {
        assert_eq!(funding_open(&[ev(900, -10.0)], 30.0), None);
    }

    /// Posición que COBRÓ funding (sinceOpen<0): mismo criterio con el signo
    /// contrario, y un evento del signo opuesto no descarrila la cuenta.
    #[test]
    fn funciona_con_funding_cobrado() {
        let evs = vec![ev(900, 5.0), ev(800, -1.0), ev(700, 8.0)];
        assert_eq!(funding_open(&evs, -12.0), Some(700));
    }

    /// Contra la API real (mainnet): la whale que motivó todo esto, cuyas 2000
    /// entradas de `userFills` cubren 100 minutos y NINGUNA de sus posiciones
    /// tiene ahí su apertura. Comprueba que la vía del funding sí las
    /// reconstruye. Tarda ~1 min y sale a la red: `--ignored` explícito.
    #[tokio::test]
    #[ignore]
    async fn aperturas_reales_de_whale_activa() {
        let user = "0xb83de012dba672c76a7dbbbf3e459cb59d7d6e36";
        let client = reqwest::Client::new();
        let api = super::super::MAINNET_API_URL;
        let st = super::super::info_post(
            &client,
            api,
            serde_json::json!({"type": "clearinghouseState", "user": user}),
        )
        .await
        .expect("clearinghouseState responde");
        let positions: Vec<(String, f64, f64)> = st["assetPositions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|ap| {
                let p = &ap["position"];
                (
                    p["coin"].as_str().unwrap().to_string(),
                    p["szi"].as_str().unwrap().parse().unwrap(),
                    p["cumFunding"]["sinceOpen"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap(),
                )
            })
            .collect();
        assert!(
            positions.len() > 5,
            "la whale debería tener varias posiciones"
        );
        let now = super::super::now_ms();
        let out = resolve(&client, api, user, &positions, now).await;

        let by_funding = out.values().filter(|o| o.kind == OpenKind::Funding).count();
        for (coin, o) in &out {
            let age_d = (now - o.ms) / 86_400_000;
            println!("{coin:10} {:?} hace {age_d}d", o.kind);
        }
        assert!(
            by_funding >= 5,
            "se esperaban aperturas reconstruidas por funding, hubo {by_funding}"
        );
    }

    /// Salvaguardas: `sinceOpen` de polvo no se intenta (sería ruido), y un
    /// cruce que se pasa muchísimo del objetivo (funding alternando de signo)
    /// se descarta en vez de dar una fecha inventada.
    #[test]
    fn descarta_ruido_y_cruces_sucios() {
        assert_eq!(funding_open(&[ev(900, -0.001)], 0.002), None);
        assert_eq!(funding_open(&[ev(900, -100.0)], 1.0), None);
    }
}
