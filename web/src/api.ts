// Kenji Web API クライアント & 共有型定義 (docs/api.md 準拠)

export const TOKEN_KEY = 'kenji_token';

// ---------- 共通型 ----------

export type Role = 'viewer' | 'operator' | 'admin';
export type JobStatus = 'running' | 'stopped' | 'failed' | 'stopping';
export type RestartPolicy = 'never' | 'on_failure' | 'always';
export type FileKind = 'file' | 'dir' | 'drive';
export type FileScope = 'workspace' | 'system';

export interface ApiErrorBody {
  error: { code: string; message: string };
}

// ---------- Auth ----------

export interface LoginResponse {
  token: string;
  username: string;
  role: Role;
  expires_at: string;
}

export interface Me {
  username: string;
  role: Role;
}

export interface KeyInfo {
  id: string;
  name: string;
  role: Role;
  created_at: string;
  last_used_at: string | null;
}

/** GET /api/users — SQLite のユーザー情報 */
export interface UserInfo {
  id: number;
  username: string;
  role: Role;
  created_at: string;
  updated_at: string;
  disabled: boolean;
}

/** POST /api/auth/keys の応答（key はこの時だけ返る） */
export interface CreatedKey extends KeyInfo {
  key: string;
}

// ---------- System ----------

export interface CpuInfo {
  cores: number;
  usage_percent: number;
}

export interface MemoryInfo {
  total_bytes: number;
  used_bytes: number;
}

export interface DiskInfo {
  label: string;
  mount_point: string;
  total_bytes: number;
  available_bytes: number;
}

export interface GpuInfo {
  name: string;
  utilization_percent: number;
  memory_total_bytes: number;
  memory_used_bytes: number;
}

export interface JobsSummary {
  running: number;
  total: number;
  failed: number;
}

/** tunnel.status: disabled | starting | running | stopped | unavailable | error */
export interface TunnelInfo {
  available: boolean;
  enabled: boolean;
  url: string | null;
  status: string;
}

/** Native Windows ngrok service status (independent of cloudflared). */
export interface NgrokServiceInfo {
  installed: boolean;
  running: boolean;
  status: 'running' | 'stopped' | 'starting' | 'stopping' | 'pending' | 'not_installed' | 'unavailable' | 'unsupported';
}

export interface Overview {
  hostname: string;
  os: string;
  version: string;
  uptime_secs: number;
  cpu: CpuInfo;
  memory: MemoryInfo;
  disks: DiskInfo[];
  gpus: GpuInfo[];
  jobs_summary: JobsSummary;
  tunnel: TunnelInfo;
  resources: ResourceStatus;
}

// ---------- Jobs ----------

export interface Job {
  id: string;
  name: string;
  description: string | null;
  command: string;
  args: string[];
  cwd: string | null;
  env: Record<string, string> | null;
  restart_policy: RestartPolicy;
  schedule: string | null;
  tags: string[];
  owner: string;
  status: JobStatus;
  pid: number | null;
  exit_code: number | null;
  last_error: string | null;
  started_at: string | null;
  stopped_at: string | null;
  created_at: string;
  schedule_next_at: string | null;
}

export interface CreateJobRequest {
  name: string;
  command: string;
  args: string[];
  cwd: string | null;
  env: Record<string, string>;
  restart_policy: RestartPolicy;
  schedule: string | null;
  tags: string[];
  description: string;
}

// ---------- Settings / Audit ----------

export interface UpdateConfigInfo {
  auto_check: boolean;
  auto_install: boolean;
  check_interval_hours: number;
  repo: string;
}

export interface ResourceStatus {
  enabled: boolean;
  supported: boolean;
  cpu_limit_percent: number;
  memory_limit_bytes: number;
  scope: string;
  error: string | null;
}

export interface RepairConfig {
  enabled: boolean;
  provider: string;
  model: string;
  base_url: string;
  api_key_env: string;
  harness_path: string;
}

export interface RepairStatus {
  state: 'idle' | 'running' | 'completed' | 'failed';
  id?: string;
  error?: string;
  output_file?: string;
}

export interface Settings {
  data_dir: string;
  workspace_root: string;
  bind: string;
  port: number;
  version: string;
  tunnel: TunnelInfo;
  ngrok?: NgrokServiceInfo;
  update: UpdateConfigInfo;
  resources: ResourceStatus;
  repair: RepairConfig;
  repair_status: RepairStatus;
}

// ---------- Update ----------

/** update.status: idle | checking | downloading | verifying | installing | failed */
export type UpdateStatusValue =
  | 'idle'
  | 'checking'
  | 'downloading'
  | 'verifying'
  | 'installing'
  | 'failed';

export interface UpdateStatusInfo {
  current_version: string;
  available: boolean;
  latest: { tag: string; version: string; notes_url: string } | null;
  status: UpdateStatusValue;
  last_check_at: string | null;
  error: string | null;
  settings: UpdateConfigInfo;
}

export interface AuditEntry {
  ts: string;
  user: string;
  role: Role;
  action: string;
  detail: string | null;
}

// ---------- Terminal ----------

/** Persistent server-side terminal tab. */
export interface TerminalSessionInfo {
  id: string;
  title: string;
  created_at: string;
  cwd: string;
  attached: boolean;
  exited: boolean;
}

export interface CreateTerminalRequest {
  title?: string;
  cols?: number;
  rows?: number;
}

// ---------- Files ----------

export interface FileEntry {
  name: string;
  kind: FileKind;
  size: number | null;
  modified: string | null;
}

export interface FileList {
  path: string;
  scope?: FileScope;
  entries: FileEntry[];
}

// ---------- トークン ----------

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearToken(): void {
  localStorage.removeItem(TOKEN_KEY);
}

// ---------- API エラー ----------

export class ApiRequestError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'ApiRequestError';
    this.status = status;
    this.code = code;
  }
}

export function errorMessage(err: unknown): string {
  if (err instanceof ApiRequestError) return err.message;
  if (err instanceof Error) return err.message;
  return String(err);
}

// ---------- fetch ラッパー ----------

/**
 * Authorization ヘッダーを付与して JSON API を呼ぶ。
 * 401 の場合はトークンを破棄して /login へリダイレクトする。
 * エラーボディ {"error":{"code","message"}} の message を例外で返す。
 */
export async function api<T = unknown>(path: string, init?: RequestInit): Promise<T> {
  const token = getToken();
  const headers = new Headers(init?.headers);
  if (token) headers.set('Authorization', `Bearer ${token}`);
  if (init?.body != null && !(init.body instanceof FormData) && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }

  const res = await fetch(path, { ...init, headers });

  if (!res.ok) {
    let code = `http_${res.status}`;
    let message = `リクエストに失敗しました (HTTP ${res.status})`;
    try {
      const text = await res.text();
      if (text) {
        const body = JSON.parse(text) as Partial<ApiErrorBody>;
        if (body.error) {
          code = body.error.code;
          message = body.error.message;
        }
      }
    } catch {
      // JSON 以外のボディは無視
    }
    if (res.status === 401) {
      clearToken();
      if (window.location.pathname !== '/login') {
        window.location.assign('/login');
      }
    }
    throw new ApiRequestError(res.status, code, message);
  }

  const text = await res.text();
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}

// ---------- WebSocket ----------

/** 開発時は Vite プロキシ経由（location.host を使用。絶対に 127.0.0.1:8787 を直書きしない） */
export function wsUrl(path: string): string {
  const token = getToken() ?? '';
  const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const separator = path.includes('?') ? '&' : '?';
  return `${proto}//${window.location.host}${path}${separator}token=${encodeURIComponent(token)}`;
}

// ---------- フォーマット ----------

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null) return '-';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB', 'PB'];
  let value = bytes;
  let i = -1;
  do {
    value /= 1024;
    i += 1;
  } while (value >= 1024 && i < units.length - 1);
  return `${value.toFixed(1)} ${units[i]}`;
}

export function formatDateTime(iso: string | null | undefined): string {
  if (!iso) return '-';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '-';
  return d.toLocaleString('ja-JP', { hour12: false });
}

export function formatUptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}日 ${h}時間`;
  if (h > 0) return `${h}時間 ${m}分`;
  return `${m}分`;
}
