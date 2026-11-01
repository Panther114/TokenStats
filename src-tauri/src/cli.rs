//! Headless scanner for verification: prints per-harness totals, or the full report with `--json`.
use tokenstats_lib::{model::Harness, pricing::Pricing, report, scan::Scanner};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cache = args.iter().position(|a| a == "--cache").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
    let mut scanner = Scanner::new(cache);
    let res = scanner.scan();
    let pricing = Pricing::embedded();
    let rep = report::build(&res.data, &pricing, res.millis, res.parsed_files, res.cached_files);
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string(&rep).unwrap());
        return;
    }
    eprintln!("scan {} ms, parsed {} files, cached {}", res.millis, res.parsed_files, res.cached_files);
    for h in Harness::ALL {
        let (mut tok, mut cost, mut n) = ([0u64; 5], 0.0, 0u32);
        for d in rep.days.iter().filter(|d| d.1 == h as u8) {
            for i in 0..5 {
                tok[i] += d.3[i];
            }
            cost += d.4;
            n += d.5;
        }
        let info = &rep.harnesses[h as usize];
        println!(
            "{:<17} responses {:>6} prompts {:>5} | in {:>11} out {:>10} cr {:>13} cw {:>11} reas {:>9} | total {:>13} | ${:>10.2} | roots {:?} errors {}",
            h.label(), n, info.prompts, tok[0], tok[1], tok[2], tok[3], tok[4], tok[0] + tok[1] + tok[2] + tok[3], cost, info.roots, info.errors.len()
        );
    }
    let unpriced: Vec<String> = rep.models.iter().filter(|m| m.price.is_none() && !m.stored_cost).map(|m| format!("{}/{:?}", m.name, m.provider)).collect();
    println!("unpriced models: {unpriced:?}");
    for m in &rep.models {
        println!("  model h{} {:<32} {:<18} -> {:?}", m.harness, m.name, m.provider.clone().unwrap_or_default(), m.price_key);
    }
}
