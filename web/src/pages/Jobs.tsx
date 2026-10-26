import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { FileText, Play, Plus, RefreshCw, Square, Trash2, X } from 'lucide-react';
import {
  api,
  errorMessage,
  formatDateTime,
  wsUrl,
  type CreateJobRequest,
  type Job,
  type JobStatus,
  type RestartPolicy,
} from '@/api';
import { cn } from '@/lib/utils';
import { Badge, type BadgeProps } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { EmptyState, ErrorBanner, Field, PageHeader, useConfirm } from '@/components/kit';

const MAX_LOG_CHARS = 1_000_000;

type BadgeVariant = NonNullable<BadgeProps['variant']>;

const STATUS_META: Record<JobStatus, { label: string; variant: BadgeVariant; dot: boolean }> = {
  running: { label: '実行中', variant: 'success', dot: true },
  stopped: { label: '停止', variant: 'secondary', dot: false },
  failed: { label: '失敗', variant: 'danger', dot: false },
  stopping: { label: '停止中', variant: 'warning', dot: false },
};

const RESTART_LABEL: Record<RestartPolicy, string> = {
  never: '再起動しない',
  on_failure: '失敗時',
  always: '常時',
};

function commandLine(job: Job): string {
  return [job.command, ...job.args].join(' ');
}

function StatusBadge({ status }: { status: JobStatus }) {
  const meta = STATUS_META[status] ?? STATUS_META.stopped;
  return (
    <Badge variant={meta.variant} dot={meta.dot}>
      {meta.label}
    </Badge>
  );
}

// ============================================================
// ジョブ一覧
// ============================================================

export default function Jobs() {
  const confirm = useConfirm();
  const [jobs, setJobs] = useState<Job[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [busy, setBusy] = useState<{ id: string; action: 'start' | 'stop' | 'delete' } | null>(null);

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api<{ jobs: Job[] }>('/api/jobs');
      setJobs(res.jobs);
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const id = window.setInterval(() => void load(), 5000);
    return () => window.clearInterval(id);
  }, [load]);

  const onJobUpdate = useCallback((job: Job) => {
    setJobs((prev) => prev.map((j) => (j.id === job.id ? job : j)));
  }, []);

  async function act(job: Job, action: 'start' | 'stop' | 'delete'): Promise<void> {
    setBusy({ id: job.id, action });
    setError(null);
    try {
      if (action === 'delete') {
        await api(`/api/jobs/${job.id}`, { method: 'DELETE' });
      } else if (action === 'start') {
        await api(`/api/jobs/${job.id}/start`, { method: 'POST' });
      } else {
        await api(`/api/jobs/${job.id}/stop`, { method: 'POST' });
      }
      await load();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(null);
    }
  }

  function onDelete(job: Job): void {
    if (!confirm(`ジョブ「${job.name}」を削除しますか？`)) return;
    void act(job, 'delete');
  }

  const selected = selectedId !== null ? jobs.find((j) => j.id === selectedId) ?? null : null;

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="mx-auto max-w-[1600px] space-y-3 p-5">
        <PageHeader title="ジョブ">
          <Button variant="secondary" onClick={() => void load()}>
            <RefreshCw className="h-3.5 w-3.5" />
            更新
          </Button>
          <Button onClick={() => setCreateOpen(true)}>
            <Plus className="h-3.5 w-3.5" />
            新規ジョブ
          </Button>
        </PageHeader>

        {error && <ErrorBanner>{error}</ErrorBanner>}

        <Card className="overflow-hidden">
          <Table className="min-w-[980px]">
            <TableHeader>
              <TableRow>
                <TableHead>名前</TableHead>
                <TableHead>所有者</TableHead>
                <TableHead>コマンド</TableHead>
                <TableHead>ステータス</TableHead>
                <TableHead>PID</TableHead>
                <TableHead>終了コード</TableHead>
                <TableHead>開始時刻</TableHead>
                <TableHead>再起動</TableHead>
                <TableHead>次回実行</TableHead>
                <TableHead className="text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {jobs.map((job) => (
                <TableRow
                  key={job.id}
                  className="cursor-pointer"
                  onClick={() => setSelectedId(job.id)}
                >
                  <TableCell className="font-medium text-zinc-900">{job.name}</TableCell>
                  <TableCell className="text-muted-foreground">{job.owner || '共通'}</TableCell>
                  <TableCell>
                    <code
                      className="block max-w-[220px] truncate font-mono text-muted-foreground"
                      title={commandLine(job)}
                    >
                      {commandLine(job)}
                    </code>
                  </TableCell>
                  <TableCell>
                    <StatusBadge status={job.status} />
                  </TableCell>
                  <TableCell className="font-mono tabular-nums text-muted-foreground">
                    {job.pid ?? '-'}
                  </TableCell>
                  <TableCell className="font-mono tabular-nums text-muted-foreground">
                    {job.exit_code ?? '-'}
                  </TableCell>
                  <TableCell className="whitespace-nowrap font-mono text-muted-foreground">
                    {formatDateTime(job.started_at)}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {RESTART_LABEL[job.restart_policy] ?? job.restart_policy}
                  </TableCell>
                  <TableCell className="whitespace-nowrap font-mono text-muted-foreground">
                    {formatDateTime(job.schedule_next_at)}
                  </TableCell>
                  <TableCell onClick={(e) => e.stopPropagation()}>
                    <div className="flex items-center justify-end gap-0.5">
                      <Button
                        variant="ghost"
                        size="icon"
                        title="ログ"
                        onClick={() => setSelectedId(job.id)}
                      >
                        <FileText className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        title="開始"
                        disabled={job.status !== 'stopped' && job.status !== 'failed'}
                        loading={busy?.id === job.id && busy.action === 'start'}
                        onClick={() => void act(job, 'start')}
                      >
                        <Play className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        title="停止"
                        disabled={job.status !== 'running'}
                        loading={busy?.id === job.id && busy.action === 'stop'}
                        onClick={() => void act(job, 'stop')}
                      >
                        <Square className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="hover:bg-red-50 hover:text-red-600"
                        title="削除"
                        disabled={busy?.id === job.id && busy.action === 'delete'}
                        onClick={() => onDelete(job)}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          {loading && jobs.length === 0 && <EmptyState>読み込み中...</EmptyState>}
          {!loading && jobs.length === 0 && (
            <EmptyState>ジョブがありません。「新規ジョブ」から作成してください。</EmptyState>
          )}
        </Card>

        {selected && (
          <JobDrawer job={selected} onClose={() => setSelectedId(null)} onJobUpdate={onJobUpdate} />
        )}

        <CreateJobDialog open={createOpen} onClose={() => setCreateOpen(false)} onCreated={() => void load()} />
      </div>
    </div>
  );
}

// ============================================================
// ログドロワー
// ============================================================

function JobDrawer({
  job,
  onClose,
  onJobUpdate,
}: {
  job: Job;
  onClose: () => void;
  onJobUpdate: (job: Job) => void;
}) {
  const [logs, setLogs] = useState('');
  const [connected, setConnected] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const [logError, setLogError] = useState<string | null>(null);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    setLogs('');
    setLogError(null);

    // 初期ログ（末尾最大64KB）
    void api<{ content: string }>(`/api/jobs/${job.id}/logs?max_bytes=65536`)
      .then((res) => {
        if (!cancelled) setLogs(res.content);
      })
      .catch((err) => {
        if (!cancelled) setLogError(errorMessage(err));
      });

    // ライブログ WS
    const ws = new WebSocket(wsUrl(`/api/jobs/${job.id}/logs/ws`));
    ws.onopen = () => {
      if (!cancelled) setConnected(true);
    };
    ws.onmessage = (ev: MessageEvent) => {
      if (typeof ev.data !== 'string') return;
      try {
        const msg = JSON.parse(ev.data) as { type?: string; data?: unknown; job?: Job };
        const data = msg.data;
        if (msg.type === 'log' && typeof data === 'string') {
          setLogs((prev) => (prev + data).slice(-MAX_LOG_CHARS));
        } else if (msg.type === 'status' && msg.job) {
          onJobUpdate(msg.job);
        }
      } catch {
        // 不正なフレームは無視
      }
    };
    ws.onclose = () => {
      if (!cancelled) setConnected(false);
    };
    return () => {
      cancelled = true;
      ws.close();
    };
  }, [job.id, onJobUpdate]);

  // 自動スクロール
  useEffect(() => {
    if (autoScroll && logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [logs, autoScroll]);

  return (
    <div className="fixed inset-0 z-40">
      <div className="absolute inset-0 bg-black/40" onClick={onClose} />
      <div className="absolute bottom-0 right-0 top-0 flex w-[720px] max-w-[90vw] flex-col border-l border-zinc-200 bg-white shadow-2xl">
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-zinc-200 px-4">
          <h2 className="truncate text-sm font-semibold text-zinc-900">{job.name}</h2>
          <StatusBadge status={job.status} />
          <span className="font-mono text-[11px] text-zinc-400">{job.id.slice(0, 8)}</span>
          <div className="ml-auto flex items-center gap-3">
            <label className="flex cursor-pointer items-center gap-1.5 text-[11px] text-zinc-600">
              <input
                type="checkbox"
                className="accent-blue-600"
                checked={autoScroll}
                onChange={(e) => setAutoScroll(e.target.checked)}
              />
              自動スクロール
            </label>
            <Badge variant={connected ? 'success' : 'secondary'} dot>
              {connected ? 'ライブ' : '未接続'}
            </Badge>
            <button
              type="button"
              className="rounded p-1 text-zinc-400 transition-colors hover:bg-zinc-100 hover:text-zinc-700"
              onClick={onClose}
              aria-label="閉じる"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>

        <div className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-0 border-b border-zinc-200 px-4 py-2 text-[11px] text-muted-foreground">
          <code className="truncate font-mono text-zinc-500">{commandLine(job)}</code>
          {job.pid != null && <span>PID {job.pid}</span>}
          {job.exit_code != null && <span>終了コード {job.exit_code}</span>}
          <span>開始 {formatDateTime(job.started_at)}</span>
          <span>{RESTART_LABEL[job.restart_policy] ?? job.restart_policy}</span>
          <span>所有者 {job.owner || '共通'}</span>
        </div>

        <div ref={logRef} className="flex-1 overflow-auto bg-zinc-50 p-3">
          {logError ? (
            <div className="text-xs text-red-600">{logError}</div>
          ) : logs.length === 0 ? (
            <div className="text-xs text-zinc-400">ログはまだありません</div>
          ) : (
            <pre className="whitespace-pre-wrap break-all font-mono text-[12px] leading-relaxed text-zinc-700">
              {logs}
            </pre>
          )}
        </div>
      </div>
    </div>
  );
}

// ============================================================
// 新規ジョブダイアログ
// ============================================================

function CreateJobDialog({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: () => void;
}) {
  const [name, setName] = useState('');
  const [command, setCommand] = useState('');
  const [argsText, setArgsText] = useState('');
  const [cwd, setCwd] = useState('');
  const [envText, setEnvText] = useState('');
  const [restartPolicy, setRestartPolicy] = useState<RestartPolicy>('never');
  const [scheduleText, setScheduleText] = useState('');
  const [descriptionText, setDescriptionText] = useState('');
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [serverError, setServerError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setName('');
    setCommand('');
    setArgsText('');
    setCwd('');
    setEnvText('');
    setRestartPolicy('never');
    setScheduleText('');
    setDescriptionText('');
    setErrors({});
    setServerError(null);
  }, [open]);

  async function submit(e: FormEvent): Promise<void> {
    e.preventDefault();
    const errs: Record<string, string> = {};
    if (!name.trim()) errs.name = '名前を入力してください';
    if (!command.trim()) errs.command = 'コマンドを入力してください';

    let env: Record<string, string> = {};
    if (envText.trim()) {
      try {
        const parsed: unknown = JSON.parse(envText);
        if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
          throw new Error('not an object');
        }
        env = {};
        for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
          env[k] = String(v);
        }
      } catch {
        errs.env = 'JSONオブジェクトとして入力してください（例: {"KEY": "value"}）';
      }
    }

    if (scheduleText.trim() && scheduleText.trim().split(/\s+/).length !== 5) {
      errs.schedule = '5フィールド（分 時 日 月 曜）のcron形式で入力してください';
    }

    setErrors(errs);
    if (Object.keys(errs).length > 0) return;

    const body = {
      name: name.trim(),
      command: command.trim(),
      args: argsText.split(/\s+/).filter((s) => s.length > 0),
      cwd: cwd.trim() || null,
      env,
      restart_policy: restartPolicy,
      schedule: scheduleText.trim() || null,
      tags: [],
      description: descriptionText.trim(),
    } satisfies CreateJobRequest;

    setBusy(true);
    setServerError(null);
    try {
      await api('/api/jobs', { method: 'POST', body: JSON.stringify(body) });
      onCreated();
      onClose();
    } catch (err) {
      setServerError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>新規ジョブ</DialogTitle>
        </DialogHeader>
        <form className="space-y-3" onSubmit={(e) => void submit(e)}>
          <div className="grid grid-cols-2 gap-3">
            <Field label="名前" error={errors.name}>
              <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="train" autoFocus />
            </Field>
            <Field label="コマンド" error={errors.command}>
              <Input value={command} onChange={(e) => setCommand(e.target.value)} placeholder="python" />
            </Field>
            <Field label="引数（スペース区切り）" className="col-span-2">
              <Input
                value={argsText}
                onChange={(e) => setArgsText(e.target.value)}
                placeholder="例: train.py --epochs 10"
              />
            </Field>
            <Field label="作業ディレクトリ（任意）" className="col-span-2">
              <Input
                value={cwd}
                onChange={(e) => setCwd(e.target.value)}
                placeholder="例: proj（省略時は自分の作業フォルダ）"
              />
            </Field>
            <Field label="環境変数（JSON、任意）" error={errors.env} className="col-span-2">
              <Input
                value={envText}
                onChange={(e) => setEnvText(e.target.value)}
                placeholder='{"CUDA_VISIBLE_DEVICES": "0"}'
                spellCheck={false}
              />
            </Field>
            <Field label="再起動ポリシー">
              <Select value={restartPolicy} onValueChange={(v) => setRestartPolicy(v as RestartPolicy)}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="never">再起動しない</SelectItem>
                  <SelectItem value="on_failure">失敗時</SelectItem>
                  <SelectItem value="always">常時</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field label="スケジュール（cron、任意）" error={errors.schedule}>
              <Input
                value={scheduleText}
                onChange={(e) => setScheduleText(e.target.value)}
                placeholder="*/5 * * * *"
                spellCheck={false}
              />
            </Field>
            <Field label="説明（任意）" className="col-span-2">
              <Input value={descriptionText} onChange={(e) => setDescriptionText(e.target.value)} />
            </Field>
          </div>
          {serverError && <ErrorBanner>{serverError}</ErrorBanner>}
          <p className="text-[11px] text-muted-foreground">ジョブは作成後にすぐに開始されます。</p>
          <div className={cn('flex justify-end gap-2 pt-1')}>
            <Button type="button" variant="ghost" onClick={onClose}>
              キャンセル
            </Button>
            <Button type="submit" loading={busy}>
              作成
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
