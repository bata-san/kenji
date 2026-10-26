import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from 'react';
import {
  ArrowUpCircle,
  Check,
  Copy,
  Download,
  ExternalLink,
  KeyRound,
  Plus,
  RefreshCw,
  ScrollText,
  ShieldAlert,
  Trash2,
  UserPlus,
  Users,
} from 'lucide-react';
import {
  api,
  ApiRequestError,
  errorMessage,
  formatDateTime,
  type AuditEntry,
  type CreatedKey,
  type KeyInfo,
  type Me,
  type Role,
  type Settings,
  type UpdateStatusInfo,
  type UserInfo,
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

import RepairSettings from '@/components/RepairSettings';
import DesktopPriority from '@/components/DesktopPriority';

type BadgeVariant = NonNullable<BadgeProps['variant']>;

const TUNNEL_STATUS: Record<string, { label: string; variant: BadgeVariant }> = {
  disabled: { label: '無効', variant: 'secondary' },
  starting: { label: '起動中', variant: 'warning' },
  running: { label: '稼働中', variant: 'success' },
  stopped: { label: '停止', variant: 'secondary' },
  unavailable: { label: '利用不可', variant: 'secondary' },
  error: { label: 'エラー', variant: 'danger' },
};

const UPDATE_STATUS: Record<string, { label: string; variant: BadgeVariant }> = {
  idle: { label: '待機中', variant: 'secondary' },
  checking: { label: '確認中', variant: 'warning' },
  downloading: { label: 'ダウンロード中', variant: 'warning' },
  verifying: { label: '検証中', variant: 'warning' },
  installing: { label: '適用中', variant: 'warning' },
  failed: { label: '失敗', variant: 'danger' },
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
  const [users, setUsers] = useState<UserInfo[]>([]);
  const [me, setMe] = useState<Me | null>(null);
  const [audit, setAudit] = useState<AuditEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [tunnelBusy, setTunnelBusy] = useState(false);
  const [keyModalOpen, setKeyModalOpen] = useState(false);
  const [userModalOpen, setUserModalOpen] = useState(false);
  const [passwordTarget, setPasswordTarget] = useState<UserInfo | null>(null);
  const [userBusy, setUserBusy] = useState<string | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateStatusInfo | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [prefBusy, setPrefBusy] = useState<string | null>(null);
  const [restarting, setRestarting] = useState(false);

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

  const loadUsers = useCallback(async (): Promise<void> => {
    try {
      const [res, meRes] = await Promise.all([
        api<{ users: UserInfo[] }>('/api/users'),
        api<Me>('/api/auth/me'),
      ]);
      setUsers(res.users);
      setMe(meRes);
    } catch (err) {
      setError(errorMessage(err));
    }
  }, []);

  const loadUpdate = useCallback(async (): Promise<void> => {
    try {
      const res = await api<UpdateStatusInfo>('/api/update/status');
      setUpdateInfo(res);
    } catch (err) {
      setError(errorMessage(err));
    }
  }, []);

  const refreshAll = useCallback(async (): Promise<void> => {
    try {
      const s = await api<Settings>('/api/settings');
      setSettings(s);
      setError(null);
      await Promise.all([loadKeys(), loadUsers(), loadAudit(), loadUpdate()]);
    } catch (err) {
      if (err instanceof ApiRequestError && err.status === 403) {
        setForbidden(true);
      } else {
        setError(errorMessage(err));
      }
    }
  }, [loadKeys, loadUsers, loadAudit, loadUpdate]);

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

  async function checkUpdate(): Promise<void> {
    setUpdateBusy(true);
    try {
      const res = await api<UpdateStatusInfo>('/api/update/check', { method: 'POST' });
      setUpdateInfo(res);
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setUpdateBusy(false);
    }
  }

  async function installUpdate(): Promise<void> {
    const latest = updateInfo?.latest;
    if (!updateInfo?.available || !latest || restarting) return;
    if (!confirm(`v${latest.version} をダウンロードして適用します。完了後、サーバーは自動的に再起動します。`)) return;
    setUpdateBusy(true);
    try {
      await api('/api/update/install', { method: 'POST' });
      setRestarting(true);
      void waitForRestart();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setUpdateBusy(false);
    }
  }

  async function waitForRestart(): Promise<void> {
    // 旧プロセス停止 → 新プロセス起動を待ってからリロードする
    const deadline = Date.now() + 90_000;
    await new Promise((r) => setTimeout(r, 3000));
    while (Date.now() < deadline) {
      try {
        const res = await fetch('/api/health');
        if (res.ok) {
          window.location.reload();
          return;
        }
      } catch {
        // 再起動中は接続できない → リトライ
      }
      await new Promise((r) => setTimeout(r, 2000));
    }
    setRestarting(false);
    setError('再起動の完了を確認できませんでした。しばらくしてからページを再読み込みしてください。');
  }

  async function setUpdatePref(patch: { auto_check?: boolean; auto_install?: boolean }): Promise<void> {
    setPrefBusy(patch.auto_check != null ? 'auto_check' : 'auto_install');
    try {
      const s = await api<Settings>('/api/settings', {
        method: 'PUT',
        body: JSON.stringify({ update: patch }),
      });
      setSettings(s);
      const u = await api<UpdateStatusInfo>('/api/update/status');
      setUpdateInfo(u);
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPrefBusy(null);
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

  async function changeUserRole(user: UserInfo, role: Role): Promise<void> {
    if (user.role === role) return;
    if (
      user.username === me?.username &&
      role !== 'admin' &&
      !confirm('自分自身を管理者から降格します。続けますか？')
    ) {
      return;
    }
    setUserBusy(`role:${user.id}`);
    try {
      await api(`/api/users/${encodeURIComponent(user.username)}/role`, {
        method: 'PUT',
        body: JSON.stringify({ role }),
      });
      await loadUsers();
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setUserBusy(null);
    }
  }

  function deleteUser(user: UserInfo): void {
    if (!confirm(`ユーザー「${user.username}」を削除しますか？`)) return;
    void (async () => {
      setUserBusy(`delete:${user.id}`);
      try {
        await api(`/api/users/${encodeURIComponent(user.username)}`, { method: 'DELETE' });
        await loadUsers();
        setError(null);
      } catch (err) {
        setError(errorMessage(err));
      } finally {
        setUserBusy(null);
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
  const updateMeta = UPDATE_STATUS[updateInfo?.status ?? 'idle'] ?? UPDATE_STATUS.idle;
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
              <TabsTrigger value="update">アップデート</TabsTrigger>
              <TabsTrigger value="repair">AI診断・修復</TabsTrigger>
              <TabsTrigger value="keys">APIキー</TabsTrigger>
              <TabsTrigger value="users">ユーザー</TabsTrigger>
              <TabsTrigger value="audit">監査ログ</TabsTrigger>
            </TabsList>

            <TabsContent value="server" className="mt-3 space-y-3">
              <DesktopPriority />
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

            <TabsContent value="repair" className="mt-3 space-y-3">
              <RepairSettings settings={settings} onSaved={setSettings} />
            </TabsContent>

            <TabsContent value="update" className="mt-3 space-y-3">
              <Card>
                <CardHeader>
                  <CardTitle className="flex items-center gap-2">
                    <ArrowUpCircle className="h-3.5 w-3.5 text-muted-foreground" />
                    アップデート
                  </CardTitle>
                  <div className="flex gap-2">
                    <Button variant="secondary" loading={updateBusy} onClick={() => void checkUpdate()}>
                      <RefreshCw className="h-3.5 w-3.5" />
                      最新版を確認
                    </Button>
                    <Button
                      disabled={!updateInfo?.available || restarting}
                      loading={updateBusy}
                      onClick={() => void installUpdate()}
                    >
                      <Download className="h-3.5 w-3.5" />
                      アップデートを適用
                    </Button>
                  </div>
                </CardHeader>
                <div className="divide-y divide-zinc-200/60">
                  <InfoRow label="現在のバージョン">v{updateInfo?.current_version ?? settings.version}</InfoRow>
                  <InfoRow label="状態">
                    <Badge variant={updateMeta.variant}>{updateMeta.label}</Badge>
                  </InfoRow>
                  <InfoRow label="最新バージョン">
                    {updateInfo?.latest ? (
                      <span className="inline-flex items-center gap-1.5">
                        <a
                          href={updateInfo.latest.notes_url}
                          target="_blank"
                          rel="noreferrer"
                          className="text-blue-600 hover:underline"
                        >
                          v{updateInfo.latest.version}
                        </a>
                        {updateInfo.available ? (
                          <Badge variant="success">更新あり</Badge>
                        ) : (
                          <span className="text-muted-foreground">最新状態です</span>
                        )}
                      </span>
                    ) : (
                      <span className="text-muted-foreground">未確認</span>
                    )}
                  </InfoRow>
                  <InfoRow label="最終確認">{formatDateTime(updateInfo?.last_check_at)}</InfoRow>
                  <InfoRow label="配布元">{updateInfo?.settings.repo ?? settings.update.repo}</InfoRow>
                  {updateInfo?.error && (
                    <InfoRow label="エラー">
                      <span className="text-red-600">{updateInfo.error}</span>
                    </InfoRow>
                  )}
                </div>
                {restarting && (
                  <div className="flex items-center gap-2 border-t border-zinc-200/60 px-3.5 py-3 text-xs text-muted-foreground">
                    <Spinner />
                    サーバーを再起動しています。完了後に自動的にページを再読み込みします…
                  </div>
                )}
              </Card>

              <Card>
                <CardHeader>
                  <CardTitle>アップデート設定</CardTitle>
                </CardHeader>
                <div className="divide-y divide-zinc-200/60">
                  <div className="flex items-center justify-between gap-4 px-3.5 py-3">
                    <div className="min-w-0">
                      <p className="text-xs font-medium text-zinc-700">自動確認</p>
                      <p className="mt-0.5 text-[11px] text-muted-foreground">
                        {settings.update.check_interval_hours}時間ごとに配布元の新バージョンを確認します
                      </p>
                    </div>
                    <Button
                      variant={settings.update.auto_check ? 'destructive' : 'default'}
                      loading={prefBusy === 'auto_check'}
                      onClick={() => void setUpdatePref({ auto_check: !settings.update.auto_check })}
                    >
                      {settings.update.auto_check ? '無効化' : '有効化'}
                    </Button>
                  </div>
                  <div className="flex items-center justify-between gap-4 px-3.5 py-3">
                    <div className="min-w-0">
                      <p className="text-xs font-medium text-zinc-700">自動インストール</p>
                      <p className="mt-0.5 text-[11px] text-muted-foreground">
                        新バージョン検出時にSHA256検証・適用・再起動まで自動で行います
                      </p>
                    </div>
                    <Button
                      variant={settings.update.auto_install ? 'destructive' : 'default'}
                      loading={prefBusy === 'auto_install'}
                      onClick={() => void setUpdatePref({ auto_install: !settings.update.auto_install })}
                    >
                      {settings.update.auto_install ? '無効化' : '有効化'}
                    </Button>
                  </div>
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

            <TabsContent value="users" className="mt-3">
              <Card className="overflow-hidden">
                <CardHeader>
                  <CardTitle className="flex items-center gap-2">
                    <Users className="h-3.5 w-3.5 text-muted-foreground" />
                    ユーザー管理
                  </CardTitle>
                  <Button onClick={() => setUserModalOpen(true)}>
                    <UserPlus className="h-3.5 w-3.5" />
                    新規ユーザー
                  </Button>
                </CardHeader>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>ユーザー名</TableHead>
                      <TableHead>ロール</TableHead>
                      <TableHead>作成日時</TableHead>
                      <TableHead>更新日時</TableHead>
                      <TableHead className="w-24 text-right">操作</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {users.map((u) => (
                      <TableRow key={u.id}>
                        <TableCell className="font-medium text-zinc-700">
                          {u.username}
                          {me?.username === u.username && (
                            <span className="ml-2 text-[10px] text-muted-foreground">(自分)</span>
                          )}
                        </TableCell>
                        <TableCell>
                          <Select
                            value={u.role}
                            onValueChange={(v) => void changeUserRole(u, v as Role)}
                            disabled={userBusy !== null}
                          >
                            <SelectTrigger className="h-7 w-32">
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              <SelectItem value="viewer">viewer</SelectItem>
                              <SelectItem value="operator">operator</SelectItem>
                              <SelectItem value="admin">admin</SelectItem>
                            </SelectContent>
                          </Select>
                        </TableCell>
                        <TableCell className="whitespace-nowrap font-mono text-muted-foreground">
                          {formatDateTime(u.created_at)}
                        </TableCell>
                        <TableCell className="whitespace-nowrap font-mono text-muted-foreground">
                          {formatDateTime(u.updated_at)}
                        </TableCell>
                        <TableCell>
                          <div className="flex items-center justify-end gap-0.5">
                            <Button
                              variant="ghost"
                              size="icon"
                              title="パスワード変更"
                              onClick={() => setPasswordTarget(u)}
                            >
                              <KeyRound className="h-3.5 w-3.5" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon"
                              className="hover:bg-red-50 hover:text-red-600"
                              title="削除"
                              disabled={me?.username === u.username || userBusy === `delete:${u.id}`}
                              onClick={() => deleteUser(u)}
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
                {users.length === 0 && <EmptyState>ユーザーがいません</EmptyState>}
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
        <UserDialog
          open={userModalOpen}
          onClose={() => setUserModalOpen(false)}
          onCreated={() => void loadUsers()}
        />
        <PasswordDialog
          user={passwordTarget}
          onClose={() => setPasswordTarget(null)}
          onSaved={() => void loadUsers()}
        />
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

// ============================================================
// ユーザー作成ダイアログ
// ============================================================

function UserDialog({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: () => void;
}) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [role, setRole] = useState<Role>('operator');
  const [busy, setBusy] = useState(false);
  const [serverError, setServerError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setUsername('');
    setPassword('');
    setRole('operator');
    setBusy(false);
    setServerError(null);
  }, [open]);

  async function submit(e: FormEvent): Promise<void> {
    e.preventDefault();
    if (!username.trim()) {
      setServerError('ユーザー名を入力してください');
      return;
    }
    if (password.length < 6) {
      setServerError('パスワードは6文字以上にしてください');
      return;
    }
    setBusy(true);
    setServerError(null);
    try {
      await api('/api/users', {
        method: 'POST',
        body: JSON.stringify({ username: username.trim(), password, role }),
      });
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
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>新規ユーザー</DialogTitle>
        </DialogHeader>
        <form className="space-y-3" onSubmit={(e) => void submit(e)}>
          <Field label="ユーザー名" hint="英数字と . _ - @（1〜64文字）">
            <Input
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder="taro"
              autoFocus
              autoComplete="off"
            />
          </Field>
          <Field label="パスワード" hint="6文字以上">
            <Input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete="new-password"
            />
          </Field>
          <Field label="ロール">
            <Select value={role} onValueChange={(v) => setRole(v as Role)}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="viewer">viewer — 読み取り専用</SelectItem>
                <SelectItem value="operator">operator — ジョブ実行・ターミナル・ファイル編集</SelectItem>
                <SelectItem value="admin">admin — すべての管理操作</SelectItem>
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
      </DialogContent>
    </Dialog>
  );
}

// ============================================================
// パスワード変更ダイアログ
// ============================================================

function PasswordDialog({
  user,
  onClose,
  onSaved,
}: {
  user: UserInfo | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [serverError, setServerError] = useState<string | null>(null);

  useEffect(() => {
    if (!user) return;
    setPassword('');
    setBusy(false);
    setServerError(null);
  }, [user]);

  async function submit(e: FormEvent): Promise<void> {
    e.preventDefault();
    if (!user) return;
    if (password.length < 6) {
      setServerError('パスワードは6文字以上にしてください');
      return;
    }
    setBusy(true);
    setServerError(null);
    try {
      await api(`/api/users/${encodeURIComponent(user.username)}/password`, {
        method: 'PUT',
        body: JSON.stringify({ password }),
      });
      onSaved();
      onClose();
    } catch (err) {
      setServerError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={user !== null} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>パスワード変更 — {user?.username}</DialogTitle>
        </DialogHeader>
        <form className="space-y-3" onSubmit={(e) => void submit(e)}>
          <Field label="新しいパスワード" hint="6文字以上">
            <Input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoFocus
              autoComplete="new-password"
            />
          </Field>
          {serverError && <ErrorBanner>{serverError}</ErrorBanner>}
          <div className="flex justify-end gap-2 pt-1">
            <Button type="button" variant="ghost" onClick={onClose}>
              キャンセル
            </Button>
            <Button type="submit" loading={busy}>
              変更
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
