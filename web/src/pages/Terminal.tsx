import { useCallback, useEffect, useRef, useState } from 'react';
import { Plus, X } from 'lucide-react';
import {
  api,
  ApiRequestError,
  errorMessage,
  type TerminalSessionInfo,
} from '@/api';
import { cn } from '@/lib/utils';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ErrorBanner, PageHeader, Spinner } from '@/components/kit';
import TerminalPane, { type TerminalConnectionState } from '@/components/TerminalPane';

const ACTIVE_KEY = 'kenji_terminal_active';

const STATE_META: Record<
  TerminalConnectionState,
  { label: string; dot: string; variant: 'success' | 'warning' | 'danger' }
> = {
  connected: { label: '接続中', dot: 'bg-emerald-500', variant: 'success' },
  connecting: { label: '接続中…', dot: 'bg-amber-400 animate-pulse', variant: 'warning' },
  reconnecting: { label: '再接続中…', dot: 'bg-amber-500 animate-pulse', variant: 'warning' },
  exited: { label: '終了', dot: 'bg-red-500', variant: 'danger' },
};

function rememberActive(id: string | null): void {
  if (id) localStorage.setItem(ACTIVE_KEY, id);
  else localStorage.removeItem(ACTIVE_KEY);
}

async function deleteSession(id: string): Promise<void> {
  try {
    await api(`/api/terminal/sessions/${id}`, { method: 'DELETE' });
  } catch (error) {
    // Already gone (server restart, exited shell, or another tab closed it).
    if (error instanceof ApiRequestError && error.status === 404) return;
    throw error;
  }
}

export default function Terminal() {
  const [tabs, setTabs] = useState<TerminalSessionInfo[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [states, setStates] = useState<Record<string, TerminalConnectionState>>({});
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const initialized = useRef(false);
  const creatingRef = useRef(false);

  const createSession = useCallback(async (): Promise<TerminalSessionInfo> => {
    return api<TerminalSessionInfo>('/api/terminal/sessions', {
      method: 'POST',
      body: JSON.stringify({}),
    });
  }, []);

  const loadSessions = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const response = await api<{ sessions: TerminalSessionInfo[] }>(
        '/api/terminal/sessions',
      );
      if (response.sessions.length > 0) {
        setTabs(response.sessions);
        const preferred = localStorage.getItem(ACTIVE_KEY);
        const next =
          response.sessions.find((session) => session.id === preferred)?.id ??
          response.sessions[0].id;
        setActiveId(next);
        rememberActive(next);
      } else {
        // Keep the old behaviour: opening the page gives you a shell.
        const created = await createSession();
        setTabs([created]);
        setActiveId(created.id);
        rememberActive(created.id);
      }
      setError(null);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, [createSession]);

  useEffect(() => {
    if (initialized.current) return;
    initialized.current = true;
    void loadSessions();
  }, [loadSessions]);

  useEffect(() => {
    if (!activeId && tabs.length > 0) {
      const next = tabs[0].id;
      setActiveId(next);
      rememberActive(next);
    }
  }, [activeId, tabs]);

  const selectTab = (id: string): void => {
    setActiveId(id);
    rememberActive(id);
  };

  const addTab = useCallback(async (): Promise<void> => {
    if (creatingRef.current) return;
    creatingRef.current = true;
    setCreating(true);
    setError(null);
    try {
      const created = await createSession();
      setTabs((previous) => [...previous, created]);
      setActiveId(created.id);
      rememberActive(created.id);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      creatingRef.current = false;
      setCreating(false);
    }
  }, [createSession]);

  const closeTab = useCallback(
    async (id: string): Promise<void> => {
      setError(null);
      try {
        await deleteSession(id);
      } catch (err) {
        setError(errorMessage(err));
        return;
      }

      const index = tabs.findIndex((tab) => tab.id === id);
      const next = tabs.filter((tab) => tab.id !== id);
      setTabs(next);
      if (activeId === id) {
        const fallback = next[Math.min(Math.max(index, 0), next.length - 1)] ?? null;
        const nextId = fallback?.id ?? null;
        setActiveId(nextId);
        rememberActive(nextId);
      }
      setStates((previous) => {
        if (!(id in previous)) return previous;
        const copy = { ...previous };
        delete copy[id];
        return copy;
      });
    },
    [tabs, activeId],
  );

  const restartTab = useCallback(
    async (id: string): Promise<void> => {
      setError(null);
      try {
        // The old shell has exited, so delete it before opening its
        // replacement (exited sessions are also pruned server-side).
        await deleteSession(id);
      } catch (err) {
        setError(errorMessage(err));
        return;
      }

      try {
        const created = await createSession();
        setTabs((previous) =>
          previous.map((tab) => (tab.id === id ? created : tab)),
        );
        setActiveId(created.id);
        rememberActive(created.id);
        setStates((previous) => {
          if (!(id in previous)) return previous;
          const copy = { ...previous };
          delete copy[id];
          return copy;
        });
      } catch (err) {
        const next = tabs.filter((tab) => tab.id !== id);
        setTabs(next);
        if (activeId === id) {
          const nextId = next[0]?.id ?? null;
          setActiveId(nextId);
          rememberActive(nextId);
        }
        setError(errorMessage(err));
      }
    },
    [tabs, activeId, createSession],
  );

  const handleState = useCallback(
    (id: string, state: TerminalConnectionState): void => {
      setStates((previous) =>
        previous[id] === state ? previous : { ...previous, [id]: state },
      );
    },
    [],
  );

  const activeMeta = activeId ? states[activeId] : undefined;
  const activeState = activeMeta ? STATE_META[activeMeta] : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="px-5 pt-4 pb-2">
        <PageHeader title="ターミナル">
          <Badge variant="info" dot className="hidden md:inline-flex">
            バックグラウンド継続
          </Badge>
          {activeState && (
            <Badge variant={activeState.variant} dot>
              {activeState.label}
            </Badge>
          )}
          <Button onClick={() => void addTab()} loading={creating} disabled={loading}>
            <Plus className="h-3.5 w-3.5" />
            新しいタブ
          </Button>
        </PageHeader>
        <p className="mt-1.5 text-[11px] text-muted-foreground">
          画面移動・再読み込み・ブラウザを閉じても、セッションはサーバー側で継続します。
        </p>
      </div>

      {error && (
        <div className="px-5 pb-2">
          <ErrorBanner>{error}</ErrorBanner>
        </div>
      )}

      {tabs.length > 0 && (
        <div className="flex min-h-[36px] items-end gap-1 overflow-x-auto border-b border-zinc-200 px-3">
          {tabs.map((tab) => {
            const isActive = tab.id === activeId;
            const state = tab.id ? states[tab.id] : undefined;
            const meta = state ? STATE_META[state] : STATE_META.connecting;
            return (
              <div
                key={tab.id}
                className={cn(
                  'group flex shrink-0 items-center rounded-t-md border border-b-0 px-1',
                  isActive
                    ? 'border-zinc-200 bg-white shadow-sm'
                    : 'border-transparent hover:bg-white/70',
                )}
              >
                <button
                  type="button"
                  onClick={() => selectTab(tab.id)}
                  title={`${tab.title}（${meta.label}）`}
                  className="flex items-center gap-1.5 px-1.5 py-2 text-xs"
                >
                  <span className={cn('h-1.5 w-1.5 rounded-full', meta.dot)} />
                  <span
                    className={cn(
                      'max-w-[160px] truncate',
                      isActive ? 'font-medium text-zinc-900' : 'text-zinc-500',
                    )}
                  >
                    {tab.title}
                  </span>
                </button>
                <button
                  type="button"
                  onClick={() => void closeTab(tab.id)}
                  aria-label={`${tab.title}を閉じる`}
                  title="タブを閉じる（シェルも終了）"
                  className="rounded p-1 text-zinc-400 opacity-70 hover:bg-zinc-200 hover:text-zinc-700 group-hover:opacity-100"
                >
                  <X className="h-3 w-3" />
                </button>
              </div>
            );
          })}
        </div>
      )}

      <div className="relative min-h-0 flex-1 px-3 pb-3">
        {loading ? (
          <div className="flex h-full items-center justify-center">
            <Spinner />
          </div>
        ) : tabs.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-3 rounded-lg border border-dashed border-zinc-300">
            <p className="text-xs text-muted-foreground">ターミナルセッションがありません</p>
            <Button onClick={() => void addTab()} loading={creating}>
              <Plus className="h-3.5 w-3.5" />
              新しいタブ
            </Button>
          </div>
        ) : (
          tabs.map((tab) => (
            <TerminalPane
              key={tab.id}
              sessionId={tab.id}
              active={tab.id === activeId}
              onStateChange={handleState}
              onRestart={(id) => void restartTab(id)}
              onClose={(id) => void closeTab(id)}
            />
          ))
        )}
      </div>
    </div>
  );
}
