//! Discovery, incremental per-file cache and parallel parsing.
//!
//! Each transcript file is parsed once and cached (postcard) keyed by path, size and mtime;
//! a refresh re-parses only new or changed files. Cross-file deduplication runs on the
//! cached per-file results, which keeps it exact while staying incremental.
use crate::model::HarnessData;
use crate::providers::{self, ProviderConfig};
use crate::{claude, codex, dsh, opencode};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const CACHE_VERSION: u32 = 4;

#[derive(Clone, Serialize, Deserialize)]
enum Parsed {
    Claude(claude::FileData),
    Codex(codex::FileData),
    Dsh(dsh::FileData),
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedFile {
    size: u64,
    mtime: i64,
    data: Parsed,
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    files: HashMap<String, CachedFile>,
}

pub struct Scanner {
    cache_path: Option<PathBuf>,
    cache: CacheFile,
    dirty: bool,
}

pub struct ScanResult {
    pub data: Vec<HarnessData>,
    pub parsed_files: usize,
    pub cached_files: usize,
    pub millis: u128,
}

pub fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            walk(&p, f);
        } else if ft.is_file() {
            f(&p);
        }
    }
}

pub fn mtime_ns(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

enum Job {
    Claude(PathBuf),
    Codex(PathBuf, PathBuf),
    Dsh(PathBuf),
}

impl Job {
    fn path(&self) -> &Path {
        match self {
            Job::Claude(p) | Job::Codex(p, _) | Job::Dsh(p) => p,
        }
    }
    fn run(&self) -> std::io::Result<Parsed> {
        Ok(match self {
            Job::Claude(p) => Parsed::Claude(claude::parse_file(p)?),
            Job::Codex(p, src) => Parsed::Codex(codex::parse_file(p, src)?),
            Job::Dsh(p) => Parsed::Dsh(dsh::parse_file(p)?),
        })
    }
}

impl Scanner {
    pub fn new(cache_path: Option<PathBuf>) -> Scanner {
        let cache = cache_path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| postcard::from_bytes::<CacheFile>(&b).ok())
            .filter(|c| c.version == CACHE_VERSION)
            .unwrap_or(CacheFile { version: CACHE_VERSION, files: HashMap::new() });
        Scanner { cache_path, cache, dirty: false }
    }

    pub fn clear(&mut self) {
        self.cache.files.clear();
        self.dirty = true;
    }

    /// Scans the enabled providers; `data[i]` belongs to `providers[i]` (disabled ones stay empty).
    pub fn scan(&mut self, providers: &[ProviderConfig]) -> ScanResult {
        let started = std::time::Instant::now();
        let mut out: Vec<HarnessData> = providers.iter().map(|_| HarnessData::default()).collect();
        // (provider index, job) for every cacheable transcript file.
        let mut jobs: Vec<(usize, Job)> = Vec::new();
        for (pi, cfg) in providers.iter().enumerate() {
            if !cfg.enabled {
                continue;
            }
            let roots = providers::roots_for(cfg);
            let d = &mut out[pi];
            match cfg.kind.as_str() {
                "claude" => {
                    for r in &roots {
                        let projects = if r.file_name().is_some_and(|n| n == "projects") { r.clone() } else { r.join("projects") };
                        if projects.is_dir() {
                            d.roots.push(projects.display().to_string());
                            jobs.extend(claude::discover_projects(&projects).into_iter().map(|p| (pi, Job::Claude(p))));
                        }
                    }
                }
                "codex" => {
                    for h in &roots {
                        if h.is_dir() {
                            d.roots.push(h.display().to_string());
                            jobs.extend(codex::discover(h).into_iter().map(|(p, s)| (pi, Job::Codex(p, s))));
                        }
                    }
                }
                "dsh" => {
                    for h in &roots {
                        let sessions = if h.join("sessions").is_dir() { h.join("sessions") } else { h.clone() };
                        if sessions.is_dir() {
                            d.roots.push(sessions.display().to_string());
                            jobs.extend(dsh::discover_sessions(&sessions).into_iter().map(|p| (pi, Job::Dsh(p))));
                        }
                    }
                }
                "opencode" => {
                    let mut seen = HashSet::new();
                    for dir in &roots {
                        let dir = if dir.is_file() { dir.parent().map(Path::to_path_buf).unwrap_or_default() } else { dir.clone() };
                        if !dir.is_dir() {
                            continue;
                        }
                        match opencode::db_path(&dir) {
                            Some(db) => d.roots.push(db.display().to_string()),
                            None => d.roots.push(dir.display().to_string()),
                        }
                        d.files += 1;
                        if let Err(e) = opencode::scan_dir(&dir, d, &mut seen) {
                            d.errors.push(e);
                        }
                    }
                }
                kind => {
                    let existing: Vec<PathBuf> = roots.into_iter().filter(|r| r.exists()).collect();
                    for r in &existing {
                        d.roots.push(r.display().to_string());
                    }
                    if !existing.is_empty() {
                        providers::scan_simple(kind, &existing, d);
                    }
                }
            }
        }

        // Stat every file; reuse cached parses whose size and mtime are unchanged.
        let stats: Vec<(u64, i64)> = jobs
            .par_iter()
            .map(|(_, j)| std::fs::metadata(j.path()).map(|m| (m.len(), mtime_ns(&m))).unwrap_or((0, 0)))
            .collect();
        let todo: Vec<usize> = (0..jobs.len())
            .filter(|&i| {
                let key = jobs[i].1.path().to_string_lossy();
                !matches!(self.cache.files.get(key.as_ref()), Some(c) if (c.size, c.mtime) == stats[i])
            })
            .collect();
        let parsed: Vec<(usize, std::io::Result<Parsed>)> = todo.par_iter().map(|&i| (i, jobs[i].1.run())).collect();
        for (i, res) in parsed {
            match res {
                Ok(data) => {
                    let (size, mtime) = stats[i];
                    self.cache.files.insert(jobs[i].1.path().to_string_lossy().into_owned(), CachedFile { size, mtime, data });
                    self.dirty = true;
                }
                Err(e) => out[jobs[i].0].errors.push(format!("{}: {e}", jobs[i].1.path().display())),
            }
        }
        // Forget files that no longer exist (or belong to removed providers).
        let live: HashSet<String> = jobs.iter().map(|(_, j)| j.path().to_string_lossy().into_owned()).collect();
        let before = self.cache.files.len();
        self.cache.files.retain(|k, _| live.contains(k));
        if self.cache.files.len() != before {
            self.dirty = true;
        }

        for (pi, cfg) in providers.iter().enumerate() {
            let paths: Vec<&Path> = jobs.iter().filter(|(p, _)| *p == pi).map(|(_, j)| j.path()).collect();
            let d = &mut out[pi];
            let cached = |p: &Path| self.cache.files.get(p.to_string_lossy().as_ref()).map(|c| &c.data);
            match cfg.kind.as_str() {
                "claude" => {
                    let files: Vec<(PathBuf, claude::FileData)> = paths
                        .iter()
                        .filter_map(|p| match cached(p) {
                            Some(Parsed::Claude(f)) => Some((p.to_path_buf(), f.clone())),
                            _ => None,
                        })
                        .collect();
                    d.files += files.len();
                    claude::merge(&files, d);
                }
                "codex" => {
                    let files: Vec<(PathBuf, codex::FileData)> = paths
                        .iter()
                        .filter_map(|p| match cached(p) {
                            Some(Parsed::Codex(f)) => Some((p.to_path_buf(), f.clone())),
                            _ => None,
                        })
                        .collect();
                    d.files += files.len();
                    codex::merge(&files, d);
                }
                "dsh" => {
                    let files: Vec<(PathBuf, dsh::FileData)> = paths
                        .iter()
                        .filter_map(|p| match cached(p) {
                            Some(Parsed::Dsh(f)) => Some((p.to_path_buf(), f.clone())),
                            _ => None,
                        })
                        .collect();
                    d.files += files.len();
                    dsh::merge(&files, d);
                }
                _ => {}
            }
        }
        self.save();
        ScanResult { data: out, parsed_files: todo.len(), cached_files: jobs.len() - todo.len(), millis: started.elapsed().as_millis() }
    }

    fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(p) = &self.cache_path {
            if let Ok(bytes) = postcard::to_allocvec(&self.cache) {
                if let Some(dir) = p.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let tmp = p.with_extension("tmp");
                if std::fs::write(&tmp, bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, p);
                }
            }
        }
        self.dirty = false;
    }
}
