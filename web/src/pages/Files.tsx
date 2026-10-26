import { useCallback, useEffect, useRef, useState, type ChangeEvent, type KeyboardEvent } from 'react';
import {
  ChevronRight,
  Download,
  File as FileIcon,
  Folder,
  FolderPlus,
  Loader2,
  Pencil,
  RefreshCw,
  Trash2,
  Upload,
} from 'lucide-react';
import {
  api,
  errorMessage,
  formatBytes,
  formatDateTime,
  getToken,
  type FileEntry,
  type FileList,
} from '@/api';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table';
import { EmptyState, ErrorBanner, PageHeader, useConfirm } from '@/components/kit';

function sortEntries(list: FileEntry[]): FileEntry[] {
  return [...list].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === 'dir' ? -1 : 1;
    return a.name.localeCompare(b.name, 'ja');
  });
}

function normalizePath(p: string): string {
  let s = p.trim();
  if (s === '') return '/';
  if (!s.startsWith('/')) s = `/${s}`;
  s = s.replace(/\/{2,}/g, '/');
  if (s.length > 1 && s.endsWith('/')) s = s.slice(0, -1);
  return s;
}

function joinPath(dir: string, name: string): string {
  return dir === '/' ? `/${name}` : `${dir}/${name}`;
}

function crumbs(path: string): Array<{ label: string; path: string }> {
  const items: Array<{ label: string; path: string }> = [{ label: 'マイワークスペース', path: '/' }];
  let cur = '';
  for (const part of path.split('/').filter(Boolean)) {
    cur += `/${part}`;
    items.push({ label: part, path: cur });
  }
  return items;
}

export default function Files() {
  const confirm = useConfirm();
  const [dir, setDir] = useState('/');
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [pathInput, setPathInput] = useState('/');
  const [uploading, setUploading] = useState<{ done: number; total: number } | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async (path: string): Promise<void> => {
    setLoading(true);
    try {
      const res = await api<FileList>(`/api/files/list?path=${encodeURIComponent(path)}`);
      setEntries(sortEntries(res.entries));
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(dir);
  }, [dir, load]);

  useEffect(() => {
    setPathInput(dir);
  }, [dir]);

  function go(path: string): void {
    setDir(normalizePath(path));
  }

  function onPathInputKeyDown(e: KeyboardEvent<HTMLInputElement>): void {
    if (e.key === 'Enter') go(pathInput);
  }

  function mkdir(): void {
    const name = window.prompt('新規フォルダ名を入力してください', '');
    if (name === null) return;
    const clean = name.trim();
    if (clean === '' || clean.includes('/')) {
      setError('フォルダ名が不正です');
      return;
    }
    void (async () => {
      try {
        await api('/api/files/mkdir', { method: 'POST', body: JSON.stringify({ path: joinPath(dir, clean) }) });
        setError(null);
        await load(dir);
      } catch (err) {
        setError(errorMessage(err));
      }
    })();
  }

  function rename(entry: FileEntry): void {
    const name = window.prompt('新しい名前を入力してください', entry.name);
    if (name === null) return;
    const clean = name.trim();
    if (clean === '' || clean === entry.name || clean.includes('/')) return;
    void (async () => {
      try {
        await api('/api/files/move', {
          method: 'POST',
          body: JSON.stringify({ from: joinPath(dir, entry.name), to: joinPath(dir, clean) }),
        });
        setError(null);
        await load(dir);
      } catch (err) {
        setError(errorMessage(err));
      }
    })();
  }

  function remove(entry: FileEntry): void {
    const suffix = entry.kind === 'dir' ? '（フォルダ内のファイルもすべて削除されます）' : '';
    if (!confirm(`「${entry.name}」を削除しますか？${suffix}`)) return;
    void (async () => {
      try {
        await api(`/api/files/delete?path=${encodeURIComponent(joinPath(dir, entry.name))}`, {
          method: 'DELETE',
        });
        setError(null);
        await load(dir);
      } catch (err) {
        setError(errorMessage(err));
      }
    })();
  }

  function download(entry: FileEntry): void {
    const token = getToken();
    if (!token) return;
    const url = `/api/files/download?path=${encodeURIComponent(joinPath(dir, entry.name))}&token=${encodeURIComponent(token)}`;
    window.open(url, '_blank');
  }

  function onUploadChange(e: ChangeEvent<HTMLInputElement>): void {
    const files = Array.from(e.target.files ?? []);
    if (files.length === 0) return;
    void (async () => {
      setUploading({ done: 0, total: files.length });
      try {
        for (let i = 0; i < files.length; i += 1) {
          const fd = new FormData();
          fd.append('file', files[i]);
          await api(`/api/files/upload?path=${encodeURIComponent(dir)}`, { method: 'POST', body: fd });
          setUploading({ done: i + 1, total: files.length });
        }
        setError(null);
        await load(dir);
      } catch (err) {
        setError(errorMessage(err));
      } finally {
        setUploading(null);
        if (fileInputRef.current) fileInputRef.current.value = '';
      }
    })();
  }

  const crumbItems = crumbs(dir);

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="mx-auto max-w-[1600px] space-y-3 p-5">
        <PageHeader title="ファイル">
          {uploading && (
            <span className="flex items-center gap-1.5 text-[11px] text-blue-600">
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
              アップロード中 {uploading.done}/{uploading.total}
            </span>
          )}
          <Button variant="secondary" disabled={uploading !== null} onClick={mkdir}>
            <FolderPlus className="h-3.5 w-3.5" />
            新規フォルダ
          </Button>
          <Button
            variant="secondary"
            disabled={uploading !== null}
            onClick={() => fileInputRef.current?.click()}
          >
            <Upload className="h-3.5 w-3.5" />
            アップロード
          </Button>
          <Button variant="secondary" disabled={uploading !== null} onClick={() => void load(dir)}>
            <RefreshCw className="h-3.5 w-3.5" />
            更新
          </Button>
          <input
            ref={fileInputRef}
            type="file"
            multiple
            className="hidden"
            onChange={onUploadChange}
            disabled={uploading !== null}
          />
        </PageHeader>

        {error && <ErrorBanner>{error}</ErrorBanner>}

        <div className="flex items-center gap-2 rounded-lg border border-zinc-200 bg-white px-3 py-2">
          <nav className="flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto">
            {crumbItems.map((c, i) => (
              <span key={c.path} className="flex shrink-0 items-center gap-0.5">
                {i > 0 && <ChevronRight className="h-3 w-3 text-zinc-300" />}
                <button
                  type="button"
                  className={
                    i === crumbItems.length - 1
                      ? 'text-xs font-medium text-zinc-900'
                      : 'text-xs text-zinc-500 hover:text-blue-600 hover:underline'
                  }
                  onClick={() => go(c.path)}
                >
                  {c.label}
                </button>
              </span>
            ))}
          </nav>
          <Input
            className="w-64 font-mono"
            value={pathInput}
            onChange={(e) => setPathInput(e.target.value)}
            onKeyDown={onPathInputKeyDown}
            placeholder="/src"
            spellCheck={false}
          />
        </div>

        <Card className="overflow-hidden">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>名前</TableHead>
                <TableHead className="w-32">サイズ</TableHead>
                <TableHead className="w-48">更新日時</TableHead>
                <TableHead className="w-28 text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {entries.map((entry) => (
                <TableRow key={entry.name}>
                  <TableCell className="max-w-0">
                    {entry.kind === 'dir' ? (
                      <button
                        type="button"
                        className="flex items-center gap-2 text-left text-zinc-700 hover:text-blue-600 hover:underline"
                        onClick={() => go(joinPath(dir, entry.name))}
                      >
                        <Folder className="h-3.5 w-3.5 shrink-0 text-blue-500" />
                        <span className="truncate">{entry.name}</span>
                      </button>
                    ) : (
                      <span className="flex items-center gap-2 text-zinc-700">
                        <FileIcon className="h-3.5 w-3.5 shrink-0 text-zinc-400" />
                        <span className="truncate">{entry.name}</span>
                      </span>
                    )}
                  </TableCell>
                  <TableCell className="font-mono tabular-nums text-muted-foreground">
                    {entry.kind === 'dir' ? '-' : formatBytes(entry.size)}
                  </TableCell>
                  <TableCell className="whitespace-nowrap font-mono text-muted-foreground">
                    {formatDateTime(entry.modified)}
                  </TableCell>
                  <TableCell>
                    <div className="flex items-center justify-end gap-0.5">
                      {entry.kind === 'file' && (
                        <Button
                          variant="ghost"
                          size="icon"
                          title="ダウンロード"
                          onClick={() => download(entry)}
                        >
                          <Download className="h-3.5 w-3.5" />
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        size="icon"
                        title="名前変更"
                        onClick={() => rename(entry)}
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="hover:bg-red-50 hover:text-red-600"
                        title="削除"
                        onClick={() => remove(entry)}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          {loading && entries.length === 0 && <EmptyState>読み込み中...</EmptyState>}
          {!loading && entries.length === 0 && <EmptyState>このフォルダは空です</EmptyState>}
        </Card>
      </div>
    </div>
  );
}
