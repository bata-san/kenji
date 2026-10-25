import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from 'react';
import {
  Check,
  Copy,
  ExternalLink,
  KeyRound,
  Plus,
  RefreshCw,
  ScrollText,
  ShieldAlert,
  Trash2,
} from 'lucide-react';
import {
  api,
  ApiRequestError,
  errorMessage,
  formatDateTime,
  type AuditEntry,
  type CreatedKey,
  type KeyInfo,
  type Role,
  type Settings,
} from '@/api';
import { Badge, type BadgeProps } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardHeader, CardTitle } from '@/components/ui/card';
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import {
  EmptyState,
  ErrorBanner,
  Field,
  PageHeader,
  Spinner,
  useConfirm,
} from '@/components/kit';

type BadgeVariant = NonNullable<BadgeProps['variant']>;

const TUNNEL_STATUS: Record<string, { label: string; variant: BadgeVariant }> = {
  disabled: { label: '無効', variant: 'secondary' },
  starting: { label: '起動中', variant: 'warning' },
  running: { label: '稼働中', variant: 'success' },
  stopped: { label: '停止', variant: 'secondary' },
  unavailable: { label: '利用不可', variant: 'secondary' },
  error: { label: 'エラー', variant: 'danger' },
};

function InfoRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex gap-4 px-3.5 py-2.5 text-xs">
      <span className="w-36 shrink-0 text-muted-foreground">{label}</span>
      <span className="min-w-0 break-all font-mono text-zinc-700">{children}</span>
    </div>
  );
}

export default function Settings() {
  const confirm = useConfirm();
  const [forbidden, setForbidden] = useState(false);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [keys, setKeys] = useState<KeyInfo[]>([]);
  const [audit, setAudit] = useState<AuditEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [tunnelBusy, setTunnelBusy] = useState(false);
  const [keyModalOpen, setKeyModalOpen] = useState(false);

  const loadKeys = useCallback(async (): Promise<void> => {
    try {
      const res = await api<{ keys: KeyInfo[] }>('/api/auth/keys');
      setKeys(res.keys);
    } catch (err) {
      setError(errorMessage(err));
    }
  }, []);

  const loadAudit = useCallback(async (): Promise<void> => {
    try {
      const res = await api<{ entries: AuditEntry[] }>('/api/audit?limit=200');
      setAudit(res.entries);
    } catch (err) {
      setError(errorMessage(err));
    }
  }, []);

  const refreshAll = useCallback(async (): Promise<void> => {
    try {
      const s = await api<Settings>('/api/settings');
      setSettings(s);
      setError(null);
      await Promise.all([loadKeys(), loadAudit()]);
    } catch (err) {
      if (err instanceof ApiRequestError && err.status === 403) {
        setForbidden(true);
      } else {
        setError(errorMessage(err));
      }
    }
  }, [loadKeys, loadAudit]);

  useEffect(() => {
    void refreshAll();
  }, [refreshAll]);


  async function toggleTunnel(): Promise<void> {
    if (!settings) return;
    const enabled = !settings.tunnel.enabled;
    setTunnelBusy(true);
    try {
      await api('/api/settings', {
        method: 'PUT',
        body: JSON.stringify({ tunnel: { enabled } }),
      });
      const s = await api<Settings>('/api/settings');
      setSettings(s);
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setTunnelBusy(false);
    }
  }

  function deleteKey(key: KeyInfo): void {
    if (!confirm(`キー「${key.name}」を削除しますか？`)) return;
    void (async () => {
      try {
        await api(`/api/auth/keys/${key.id}`, { method: 'DELETE' });
        await loadKeys();
      } catch (err) {
        setError(errorMessage(err));
      }
    })();
  }

  if (forbidden) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-3">
        <ShieldAlert className="h-8 w-8 text-amber-500" />
        <p className="text-sm text-zinc-600">この操作には管理者権限が必要です</p>
      </div>
    );
  }

  const tunnelMeta =
    (settings && (TUNNEL_STATUS[settings.tunnel.status] ?? null)) ?? TUNNEL_STATUS.disabled;
  const sortedAudit = [...audit].sort((a, b) => (a.ts < b.ts ? 1 : a.ts > b.ts ? -1 : 0));

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="mx-auto max-w-[1200px] space-y-3 p-5">
        <PageHeader title="設定">
          <Button variant="secondary" onClick={() => void refreshAll()}>
            <RefreshCw className="h-3.5 w-3.5" />
            更新
          </Button>
        </PageHeader>

        {error && <ErrorBanner>{error}</ErrorBanner>}

        {!settings ? (
          <Card className="flex items-center justify-center p-12">
            <Spinner />
          </Card>
        ) : (
          <Tabs defaultValue="server">
            <TabsList>
              <TabsTrigger value="server">サーバー設定</TabsTrigger>
              <TabsTrigger value="keys">APIキー</TabsTrigger>
              <TabsTrigger value="audit">監査ログ</TabsTrigger>
            </TabsList>

            <TabsContent value="server" className="mt-3 space-y-3">
              <Card>
                <CardHeader>
                  <CardTitle>サーバー設定</CardTitle>
                </CardHeader>
                <div className="divide-y divide-zinc-200/60">
                  <InfoRow label="データディレクトリ">{settings.data_dir}</InfoRow>
                  <InfoRow label="ワークスペース">{settings.workspace_root}</InfoRow>
                  <InfoRow label="ポート">
                    {settings.bind}:{settings.port}
                  </InfoRow>
                  <InfoRow label="バージョン">{settings.version}</InfoRow>
                </div>
              </Card>

              <Card>
                <CardHeader>
                  <CardTitle>トンネル</CardTitle>
                </CardHeader>
                <div className="flex items-center justify-between gap-4 px-3.5 py-3">
                  <div className="min-w-0 space-y-1.5">
                    <div className="flex items-center gap-2">
                      <Badge variant={tunnelMeta.variant}>{tunnelMeta.label}</Badge>
                      {!settings.tunnel.available && (
                        <span className="text-[11px] text-muted-foreground">cloudflared が利用できません</span>
                      )}
                    </div>
                    {settings.tunnel.url ? (
                      <a
                        href={settings.tunnel.url}
                        target="_blank"
                        rel="noreferrer"
                        className="flex items-center gap-1 truncate font-mono text-xs text-blue-600 hover:underline"
                      >
                        {settings.tunnel.url}
                        <ExternalLink className="h-3 w-3 shrink-0" />
                      </a>
                    ) : (
                      <span className="text-xs text-muted-foreground">
                        {settings.tunnel.enabled ? 'URLを取得中です…' : 'URLは未発行です'}
                      </span>
                    )}
                  </div>
                  <Button
                    variant={settings.tunnel.enabled ? 'destructive' : 'default'}
                    loading={tunnelBusy}
                    disabled={!settings.tunnel.available}
                    onClick={() => void toggleTunnel()}
                  >
                    {settings.tunnel.enabled ? '無効化' : '有効化'}
                  </Button>
                </div>
              </Card>
            </TabsContent>

            <TabsContent value="keys" className="mt-3">
              <Card className="overflow-hidden">
                <CardHeader>
                  <CardTitle className="flex items-center gap-2">
                    <KeyRound className="h-3.5 w-3.5 text-muted-foreground" />
                    APIキー
                  </CardTitle>
                  <Button onClick={() => setKeyModalOpen(true)}>
                    <Plus className="h-3.5 w-3.5" />
                    新規キー
                  </Button>
                </CardHeader>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>名前</TableHead>
                      <TableHead>ロール</TableHead>
                      <TableHead>作成日時</TableHead>
                      <TableHead>最終使用</TableHead>
                      <TableHead className="w-16 text-right">削除</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {keys.map((k) => (
                      <TableRow key={k.id}>
                        <TableCell className="font-medium text-zinc-700">{k.name}</TableCell>
                        <TableCell>
                          <Badge variant="secondary">{k.role}</Badge>
                        </TableCell>
                        <TableCell className="whitespace-nowrap font-mono text-muted-foreground">
                          {formatDateTime(k.created_at)}
                        </TableCell>
                        <TableCell className="whitespace-nowrap font-mono text-muted-foreground">
                          {formatDateTime(k.last_used_at)}
                        </TableCell>
                        <TableCell className="text-right">
                          <Button
                            variant="ghost"
                            size="icon"
                            className="hover:bg-red-50 hover:text-red-600"
                            title="削除"
                            onClick={() => deleteKey(k)}
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </Button>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
                {keys.length === 0 && <EmptyState>APIキーがありません</EmptyState>}
              </Card>
            </TabsContent>

            <TabsContent value="audit" className="mt-3">
              <Card className="overflow-hidden">
                <CardHeader>
                  <CardTitle className="flex items-center gap-2">
                    <ScrollText className="h-3.5 w-3.5 text-muted-foreground" />
                    監査ログ
                  </CardTitle>
                </CardHeader>
                <Table containerClassName="max-h-[520px]">
                  <TableHeader className="sticky top-0 z-10 bg-white">
                    <TableRow>
                      <TableHead>時刻</TableHead>
                      <TableHead>ユーザー</TableHead>
                      <TableHead>ロール</TableHead>
                      <TableHead>アクション</TableHead>
                      <TableHead>詳細</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {sortedAudit.map((a, i) => (
                      <TableRow key={i}>
                        <TableCell className="whitespace-nowrap font-mono text-muted-foreground">
                          {formatDateTime(a.ts)}
                        </TableCell>
                        <TableCell className="text-zinc-700">{a.user}</TableCell>
                        <TableCell className="text-muted-foreground">{a.role}</TableCell>
                        <TableCell className="font-mono text-blue-600">{a.action}</TableCell>
                        <TableCell className="text-muted-foreground">{a.detail ?? '-'}</TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
                {sortedAudit.length === 0 && <EmptyState>監査ログはありません</EmptyState>}
              </Card>
            </TabsContent>
          </Tabs>
        )}

        <KeyDialog open={keyModalOpen} onClose={() => setKeyModalOpen(false)} onCreated={() => void loadKeys()} />
      </div>
    </div>
  );
}

// ============================================================
// APIキー作成ダイアログ（作成後、鍵を一度だけ表示）
// ============================================================

function KeyDialog({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: () => void;
}) {
  const [name, setName] = useState('');
  const [role, setRole] = useState<Role>('operator');
  const [busy, setBusy] = useState(false);
  const [serverError, setServerError] = useState<string | null>(null);
  const [created, setCreated] = useState<CreatedKey | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!open) return;
    setName('');
    setRole('operator');
    setBusy(false);
    setServerError(null);
    setCreated(null);
    setCopied(false);
  }, [open]);

  async function submit(e: FormEvent): Promise<void> {
    e.preventDefault();
    if (!name.trim()) {
      setServerError('名前を入力してください');
      return;
    }
    setBusy(true);
    setServerError(null);
    try {
      const res = await api<CreatedKey>('/api/auth/keys', {
        method: 'POST',
        body: JSON.stringify({ name: name.trim(), role }),
      });
      setCreated(res);
      onCreated();
    } catch (err) {
      setServerError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function copyKey(): Promise<void> {
    if (!created) return;
    try {
      await navigator.clipboard.writeText(created.key);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{created ? 'APIキーを作成しました' : '新規APIキー'}</DialogTitle>
        </DialogHeader>
        {created ? (
          <div className="space-y-3">
            <div className="rounded-md border border-amber-200 bg-amber-50 p-3">
              <p className="mb-2 text-[11px] font-medium text-amber-700">
                ⚠ この鍵は二度と表示されません。今すぐコピーしてください。
              </p>
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 break-all rounded border border-zinc-200 bg-white px-2 py-1.5 font-mono text-xs text-zinc-700">
                  {created.key}
                </code>
                <Button variant="secondary" onClick={() => void copyKey()}>
                  {copied ? <Check className="h-3.5 w-3.5 text-emerald-600" /> : <Copy className="h-3.5 w-3.5" />}
                  {copied ? 'コピーしました' : 'コピー'}
                </Button>
              </div>
              <p className="mt-2 text-[11px] text-muted-foreground">
                {created.name}（{created.role}）
              </p>
            </div>
            <div className="flex justify-end">
              <Button onClick={onClose}>閉じる</Button>
            </div>
          </div>
        ) : (
          <form className="space-y-3" onSubmit={(e) => void submit(e)}>
            <Field label="名前" hint="用途が分かる名前（例: ci）">
              <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="ci" autoFocus />
            </Field>
            <Field label="ロール">
              <Select value={role} onValueChange={(v) => setRole(v as Role)}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="operator">operator — ジョブ実行・ターミナル・ファイル編集</SelectItem>
                  <SelectItem value="viewer">viewer — 読み取り専用</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            {serverError && <ErrorBanner>{serverError}</ErrorBanner>}
            <div className="flex justify-end gap-2 pt-1">
              <Button type="button" variant="ghost" onClick={onClose}>
                キャンセル
              </Button>
              <Button type="submit" loading={busy}>
                作成
              </Button>
            </div>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
