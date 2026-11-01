//! Discovery, incremental per-file cache and parallel parsing.
//!
//! Each transcript file is parsed once and cached (postcard) keyed by path, size and mtime;
//! a refresh re-parses only new or changed files. Cross-file deduplication runs on the
//! cached per-file results, which keeps it exact while staying incremental.
use crate::model::{Harness, HarnessData};
use crate::{claude, codex, dsh, opencode};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const CACHE_VERSION: u32 = 3;

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
    pub data: Vec<(Harness, HarnessData)>,
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
    fn harness(&self) -> Harness {
        match self {
            Job::Claude(_) => Harness::Claude,
            Job::Codex(..) => Harness::Codex,
            Job::Dsh(_) => Harness::Dsh,
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

    pub fn scan(&mut self) -> ScanResult {
        let started = std::time::Instant::now();
        let mut roots: HashMap<Harness, Vec<String>> = HashMap::new();
        let mut jobs: Vec<Job> = Vec::new();

        for r in claude::roots() {
            roots.entry(Harness::Claude).or_default().push(r.join("projects").display().to_string());
            jobs.extend(claude::discover(&r).into_iter().map(Job::Claude));
        }
        for h in codex::homes() {
            roots.entry(Harness::Codex).or_default().push(h.display().to_string());
            jobs.extend(codex::discover(&h).into_iter().map(|(p, s)| Job::Codex(p, s)));
        }
        if let Some(h) = dsh::home() {
            roots.entry(Harness::Dsh).or_default().push(h.join("sessions").display().to_string());
            jobs.extend(dsh::discover(&h).into_iter().map(Job::Dsh));
        }

        // Stat every file; reuse cached parses whose size and mtime are unchanged.
        let stats: Vec<(u64, i64)> = jobs
            .par_iter()
            .map(|j| std::fs::metadata(j.path()).map(|m| (m.len(), mtime_ns(&m))).unwrap_or((0, 0)))
            .collect();
        let todo: Vec<usize> = (0..jobs.len())
            .filter(|&i| {
                let key = jobs[i].path().to_string_lossy();
                !matches!(self.cache.files.get(key.as_ref()), Some(c) if (c.size, c.mtime) == stats[i])
            })
            .collect();
        let parsed: Vec<(usize, std::io::Result<Parsed>)> = todo.par_iter().map(|&i| (i, jobs[i].run())).collect();
        let mut errors: HashMap<Harness, Vec<String>> = HashMap::new();
        for (i, res) in parsed {
            match res {
                Ok(data) => {
                    let (size, mtime) = stats[i];
                    self.cache.files.insert(jobs[i].path().to_string_lossy().into_owned(), CachedFile { size, mtime, data });
                    self.dirty = true;
                }
                Err(e) => errors.entry(jobs[i].harness()).or_default().push(format!("{}: {e}", jobs[i].path().display())),
            }
        }
        // Forget files that no longer exist.
        let live: HashSet<String> = jobs.iter().map(|j| j.path().to_string_lossy().into_owned()).collect();
        let before = self.cache.files.len();
        self.cache.files.retain(|k, _| live.contains(k));
        if self.cache.files.len() != before {
            self.dirty = true;
        }

        let mut cl: Vec<(PathBuf, claude::FileData)> = Vec::new();
        let mut cx: Vec<(PathBuf, codex::FileData)> = Vec::new();
        let mut ds: Vec<(PathBuf, dsh::FileData)> = Vec::new();
        for j in &jobs {
            let key = j.path().to_string_lossy();
            match self.cache.files.get(key.as_ref()).map(|c| &c.data) {
                Some(Parsed::Claude(d)) => cl.push((j.path().to_path_buf(), d.clone())),
                Some(Parsed::Codex(d)) => cx.push((j.path().to_path_buf(), d.clone())),
                Some(Parsed::Dsh(d)) => ds.push((j.path().to_path_buf(), d.clone())),
                None => {}
            }
        }

        let mut out: Vec<(Harness, HarnessData)> = Vec::new();
        let mut hd = HarnessData { files: cl.len(), ..Default::default() };
        claude::merge(&cl, &mut hd);
        out.push((Harness::Claude, hd));
        let mut hd = HarnessData { files: cx.len(), ..Default::default() };
        codex::merge(&cx, &mut hd);
        out.push((Harness::Codex, hd));
        let mut hd = HarnessData::default();
        let mut seen = HashSet::new();
        for dir in opencode::data_dirs() {
            if let Some(db) = opencode::db_path(&dir) {
                hd.roots.push(db.display().to_string());
                hd.files += 1;
            } else {
                hd.roots.push(dir.display().to_string());
            }
            if let Err(e) = opencode::scan_dir(&dir, &mut hd, &mut seen) {
                hd.errors.push(e);
            }
        }
        out.push((Harness::OpenCode, hd));
        let mut hd = HarnessData { files: ds.len(), ..Default::default() };
        dsh::merge(&ds, &mut hd);
        out.push((Harness::Dsh, hd));

        for (h, d) in out.iter_mut() {
            if let Some(r) = roots.remove(h) {
                d.roots.extend(r);
            }
            if let Some(e) = errors.remove(h) {
                d.errors.extend(e);
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
