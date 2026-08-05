//! Buscador incremental de par (estilo `/` de nvim), compartido entre las
//! vistas con tabla de pares (Ranking y Flujo): filtra en vivo por substring
//! case-insensitive y prioriza los tickers que EMPIEZAN por el texto sobre
//! los que solo lo contienen en medio.

/// Estado del overlay de búsqueda. Vive en App; solo la vista activa lo usa,
/// así que un único estado basta para Ranking y Flujo.
#[derive(Default)]
pub struct SearchState {
    pub active: bool,
    pub query: String,
    /// Selección dentro de los resultados filtrados (0 = primer resultado).
    pub sel: usize,
}

impl SearchState {
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.sel = 0;
    }

    pub fn close(&mut self) {
        self.active = false;
        self.query.clear();
        self.sel = 0;
    }
}

/// Filtra `coins` a los que contienen `query` (case-insensitive): primero los
/// que empiezan por el texto, después los que lo contienen en medio, orden
/// original estable dentro de cada grupo. Query vacía = lista intacta.
pub fn filter_rank(coins: &[String], query: &str) -> Vec<String> {
    if query.is_empty() {
        return coins.to_vec();
    }
    let q = query.to_ascii_uppercase();
    let (mut starts, mut contains) = (Vec::new(), Vec::new());
    for c in coins {
        let cu = c.to_ascii_uppercase();
        if cu.starts_with(&q) {
            starts.push(c.clone());
        } else if cu.contains(&q) {
            contains.push(c.clone());
        }
    }
    starts.append(&mut contains);
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coins(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_query_returns_all() {
        let c = coins(&["BTC", "ETH"]);
        assert_eq!(filter_rank(&c, ""), c);
    }

    #[test]
    fn prefix_before_contains_stable() {
        // WETH contiene ETH pero no empieza por él: va detrás de ETH y ETHFI
        let c = coins(&["WETH", "ETH", "BTC", "ETHFI"]);
        assert_eq!(filter_rank(&c, "ETH"), coins(&["ETH", "ETHFI", "WETH"]));
    }

    #[test]
    fn case_insensitive_both_sides() {
        let c = coins(&["kPEPE", "PEOPLE"]);
        assert_eq!(filter_rank(&c, "pe"), coins(&["PEOPLE", "kPEPE"]));
        assert_eq!(filter_rank(&c, "K"), coins(&["kPEPE"]));
    }

    #[test]
    fn no_match_is_empty() {
        let c = coins(&["BTC", "ETH"]);
        assert!(filter_rank(&c, "ZZZ").is_empty());
    }
}
