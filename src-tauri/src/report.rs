//! Aggregates scanned entries into the compact report consumed by the dashboard.
//! Days and hours use the system-local timezone.
use crate::model::HarnessData;
use crate::providers::{kind_info, ProviderConfig};
use crate::pricing::Pricing;
use chrono::{Local, TimeZone, Timelike};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Serialize)]
pub struct HarnessInfo {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub icon: String,
    pub color: String,
    pub found: bool,
    pub roots: Vec<String>,
    pub files: usize,
    pub responses: usize,
    pub prompts: usize,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub harness: u8,
    pub provider: Option<String>,
    /// Matched pricing table key, if any.
    pub price_key: Option<String>,
    /// USD per million tokens: input, output, cache write, cache read.
    pub price: Option<[f64; 4]>,
    /// Long-context rates (input, output) and their threshold, when tiered.
    pub long: Option<(f64, f64, u64)>,
    /// True when part of the cost came from the harness's own recorded cost.
    pub stored_cost: bool,
}

/// Per (day, harness, model): [input, output, cache read, cache write, reasoning], cost, responses.
#[derive(Serialize)]
pub struct DayRow(pub u32, pub u8, pub u32, pub [u64; 5], pub f64, pub u32);

/// Per (day, harness): hourly message counts (responses + prompts) and the prompt count.
#[derive(Serialize)]
pub struct ActivityRow(pub u32, pub u8, pub [u32; 24], pub u32);

/// Per session: harness, project, start ms, end ms, messages, tokens, cost, active day indexes.
#[derive(Serialize)]
pub struct SessionRow(pub u8, pub String, pub i64, pub i64, pub u32, pub u64, pub f64, pub Vec<u32>);

#[derive(Serialize)]
pub struct Report {
    pub generated_at: i64,
    pub scan_ms: u128,
    pub parsed_files: usize,
    pub cached_files: usize,
    pub pricing_updated: String,
    /// Day index 0 is this date (local); rows reference days by index.
    pub epoch_day: String,
    pub harnesses: Vec<HarnessInfo>,
    pub models: Vec<ModelInfo>,
    pub days: Vec<DayRow>,
    pub activity: Vec<ActivityRow>,
    pub sessions: Vec<SessionRow>,
}

fn local_day_hour(ts: i64) -> Option<(chrono::NaiveDate, u32)> {
    let dt = Local.timestamp_millis_opt(ts).single()?;
    Some((dt.date_naive(), dt.hour()))
}

pub fn build(data: &[(ProviderConfig, HarnessData)], pricing: &Pricing, scan_ms: u128, parsed: usize, cached: usize) -> Report {
    // Earliest day across all data (ignoring obviously invalid epoch timestamps).
    let min_ts = data
        .iter()
        .flat_map(|(_, d)| d.entries.iter().map(|e| e.ts).chain(d.prompts.iter().map(|p| p.ts)))
        .filter(|&t| t > 946_684_800_000)
        .min()
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let epoch = local_day_hour(min_ts).map(|x| x.0).unwrap_or_else(|| Local::now().date_naive());
    let day_ix = |d: chrono::NaiveDate| (d - epoch).num_days().max(0) as u32;

    let mut models: Vec<ModelInfo> = Vec::new();
    let mut model_ix: HashMap<(u8, String, Option<String>), u32> = HashMap::new();
    let mut days: BTreeMap<(u32, u8, u32), ([u64; 5], f64, u32)> = BTreeMap::new();
    let mut activity: BTreeMap<(u32, u8), ([u32; 24], u32)> = BTreeMap::new();
    struct Sess {
        project: String,
        start: i64,
        end: i64,
        messages: u32,
        tokens: u64,
        cost: f64,
        days: std::collections::BTreeSet<u32>,
    }
    let mut sessions: HashMap<(u8, String), Sess> = HashMap::new();
    let mut harnesses = Vec::new();

    for (hk, (cfg, d)) in data.iter().enumerate() {
        let hk = hk as u8;
        let info = kind_info(&cfg.kind);
        harnesses.push(HarnessInfo {
            id: cfg.id.clone(),
            kind: cfg.kind.clone(),
            label: cfg.name.clone(),
            icon: info.map(|k| k.icon).unwrap_or("custom").to_string(),
            color: cfg.color.clone().unwrap_or_else(|| info.map(|k| k.color).unwrap_or("#888888").to_string()),
            found: !d.roots.is_empty(),
            roots: d.roots.clone(),
            files: d.files,
            responses: d.entries.len(),
            prompts: d.prompts.len(),
            errors: d.errors.iter().take(20).cloned().collect(),
        });
        for e in &d.entries {
            let Some((day, hour)) = local_day_hour(e.ts).filter(|_| e.ts > 946_684_800_000) else { continue };
            let di = day_ix(day);
            let key = (hk, e.model.clone(), e.provider.clone());
            let mi = *model_ix.entry(key).or_insert_with(|| {
                let found = pricing.find(&e.model, e.provider.as_deref());
                models.push(ModelInfo {
                    name: e.model.clone(),
                    harness: hk,
                    provider: e.provider.clone(),
                    price_key: found.as_ref().map(|f| f.0.clone()),
                    price: found.as_ref().map(|(_, p)| [p.input, p.output, p.cache_write, p.cache_read]),
                    long: found.as_ref().and_then(|(_, p)| match (p.input_above, p.output_above) {
                        (Some(i), Some(o)) => Some((i, o, p.threshold.unwrap_or(200_000))),
                        _ => None,
                    }),
                    stored_cost: false,
                });
                (models.len() - 1) as u32
            });
            let cost = match e.stored_cost.filter(|c| *c > 0.0) {
                Some(c) => {
                    models[mi as usize].stored_cost = true;
                    c
                }
                None => pricing
                    .find(&e.model, e.provider.as_deref())
                    .map(|(_, p)| Pricing::cost(&p, &e.usage))
                    .unwrap_or(0.0),
            };
            let u = &e.usage;
            let row = days.entry((di, hk, mi)).or_insert(([0; 5], 0.0, 0));
            row.0[0] += u.input;
            row.0[1] += u.output;
            row.0[2] += u.cache_read;
            row.0[3] += u.cache_write + u.cache_write_1h;
            row.0[4] += u.reasoning;
            row.1 += cost;
            row.2 += 1;
            activity.entry((di, hk)).or_insert(([0; 24], 0)).0[hour as usize] += 1;
            let s = sessions.entry((hk, e.session.clone())).or_insert_with(|| Sess {
                project: e.project.clone(),
                start: e.ts,
                end: e.ts,
                messages: 0,
                tokens: 0,
                cost: 0.0,
                days: Default::default(),
            });
            s.start = s.start.min(e.ts);
            s.end = s.end.max(e.ts);
            s.messages += 1;
            s.tokens += u.total();
            s.cost += cost;
            s.days.insert(di);
        }
        for p in &d.prompts {
            let Some((day, hour)) = local_day_hour(p.ts).filter(|_| p.ts > 946_684_800_000) else { continue };
            let di = day_ix(day);
            let a = activity.entry((di, hk)).or_insert(([0; 24], 0));
            a.0[hour as usize] += 1;
            a.1 += 1;
            if let Some(s) = sessions.get_mut(&(hk, p.session.clone())) {
                s.start = s.start.min(p.ts);
                s.end = s.end.max(p.ts);
                s.messages += 1;
                s.days.insert(di);
            }
        }
    }

    Report {
        generated_at: chrono::Utc::now().timestamp_millis(),
        scan_ms,
        parsed_files: parsed,
        cached_files: cached,
        pricing_updated: pricing.updated.clone(),
        epoch_day: epoch.format("%Y-%m-%d").to_string(),
        harnesses,
        models,
        days: days.into_iter().map(|((d, h, m), (t, c, n))| DayRow(d, h, m, t, (c * 1e6).round() / 1e6, n)).collect(),
        activity: activity.into_iter().map(|((d, h), (hrs, p))| ActivityRow(d, h, hrs, p)).collect(),
        sessions: sessions
            .into_iter()
            .map(|((h, _), s)| SessionRow(h, s.project, s.start, s.end, s.messages, s.tokens, (s.cost * 1e6).round() / 1e6, s.days.into_iter().collect()))
            .collect(),
    }
}
