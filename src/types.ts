/** Report produced by the Rust backend (see src-tauri/src/report.rs). */
export interface HarnessInfo {
  id: string;
  kind: string;
  label: string;
  icon: string;
  color: string;
  found: boolean;
  roots: string[];
  files: number;
  responses: number;
  prompts: number;
  errors: string[];
}


export interface ModelInfo {
  name: string;
  harness: number;
  provider: string | null;
  price_key: string | null;
  /** USD per million tokens: input, output, cache write, cache read. */
  price: [number, number, number, number] | null;
  long: [number, number, number] | null;
  stored_cost: boolean;
}

/** [day, harness, model, [input, output, cacheRead, cacheWrite, reasoning], cost, responses] */
export type DayRow = [number, number, number, [number, number, number, number, number], number, number];
/** [day, harness, hourly message counts (24), prompts] */
export type ActivityRow = [number, number, number[], number];
/** [harness, project, startMs, endMs, messages, tokens, cost, activeDays] */
export type SessionRow = [number, string, number, number, number, number, number, number[]];

export interface Report {
  generated_at: number;
  scan_ms: number;
  parsed_files: number;
  cached_files: number;
  pricing_updated: string;
  epoch_day: string;
  harnesses: HarnessInfo[];
  models: ModelInfo[];
  days: DayRow[];
  activity: ActivityRow[];
  sessions: SessionRow[];
}

/** A configured provider instance (see src-tauri/src/providers/mod.rs). */
export interface ProviderConfig {
  id: string;
  kind: string;
  name: string;
  paths: string[];
  enabled: boolean;
  color: string | null;
}

/** A supported harness kind. */
export interface CatalogEntry {
  key: string;
  name: string;
  icon: string;
  color: string;
  location: string;
  env: string;
  detected: boolean;
  default_paths: string[];
}

export interface ProvidersState {
  providers: ProviderConfig[];
  catalog: CatalogEntry[];
}
