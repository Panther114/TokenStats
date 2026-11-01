//! Headless scanner for verification: prints per-provider totals, or the full report with `--json`.
//! `--all` scans every catalog kind instead of the default configuration.
use tokenstats_lib::{pricing::Pricing, providers, report, scan::Scanner};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cache = args.iter().position(|a| a == "--cache").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
    let list: Vec<providers::ProviderConfig> = if args.iter().any(|a| a == "--all") {
        providers::KINDS
            .iter()
            .map(|k| providers::ProviderConfig { id: k.key.into(), kind: k.key.into(), name: k.name.into(), paths: vec![], enabled: true, color: None })
            .collect()
    } else {
        providers::default_config()
    };
    if args.iter().any(|a| a == "--providers-json") {
        println!("{}", serde_json::json!({ "providers": list, "catalog": providers::catalog() }));
        return;
    }
    let mut scanner = Scanner::new(cache);
    let res = scanner.scan(&list);
    let data: Vec<_> = list.into_iter().zip(res.data).collect();
    let pricing = Pricing::embedded();
    let rep = report::build(&data, &pricing, res.millis, res.parsed_files, res.cached_files);
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string(&rep).unwrap());
        return;
    }
    eprintln!("scan {} ms, parsed {} files, cached {}", res.millis, res.parsed_files, res.cached_files);
    for (i, info) in rep.harnesses.iter().enumerate() {
        let (mut tok, mut cost, mut n) = ([0u64; 5], 0.0, 0u32);
        for d in rep.days.iter().filter(|d| d.1 as usize == i) {
            for k in 0..5 {
                tok[k] += d.3[k];
            }
            cost += d.4;
            n += d.5;
        }
        println!(
            "{:<20} responses {:>6} prompts {:>5} | in {:>11} out {:>10} cr {:>13} cw {:>11} | total {:>13} | ${:>9.2} | found {} roots {:?} errors {:?}",
            info.label, n, info.prompts, tok[0], tok[1], tok[2], tok[3], tok[0] + tok[1] + tok[2] + tok[3], cost, info.found, info.roots, info.errors.iter().take(2).collect::<Vec<_>>()
        );
    }
    let unpriced: Vec<String> = rep.models.iter().filter(|m| m.price.is_none() && !m.stored_cost).map(|m| format!("{}/{:?}", m.name, m.provider)).collect();
    println!("unpriced models: {unpriced:?}");
}
