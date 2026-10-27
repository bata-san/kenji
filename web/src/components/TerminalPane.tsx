import { useEffect, useRef, useState } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { Clipboard, ClipboardPaste, RefreshCw, X } from 'lucide-react';
import { api, wsUrl } from '@/api';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';

export type TerminalConnectionState = 'connecting' | 'connected' | 'reconnecting' | 'exited';

interface TerminalPaneProps {
  sessionId: string;
  active: boolean;
  onStateChange: (sessionId: string, state: TerminalConnectionState) => void;
  onRestart: (sessionId: string) => void;
  onClose: (sessionId: string) => void;
}

interface ControlMessage {
  type?: string;
  offset?: number;
  reset?: boolean;
  exited?: boolean;
}

const RECONNECT_MAX_MS = 10_000;
const PING_INTERVAL_MS = 25_000;
const STALE_AFTER_MS = 75_000;

/**
 * One xterm instance attached to a server-side PTY session.
 *
 * The WebSocket is only a view of the session. A close, page reload, route
 * change, or background tab never terminates the shell: the client resumes
 * from the last received byte offset and the server replays the missed output.
 */
export default function TerminalPane({
  sessionId,
  active,
  onStateChange,
  onRestart,
  onClose,
}: TerminalPaneProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const sendResizeRef = useRef<() => void>(() => {});
  const activeRef = useRef(active);
  const callbacksRef = useRef({ onStateChange, onRestart, onClose });
  const terminalRef = useRef<XTerm | null>(null);
  const [exited, setExited] = useState(false);
  const [clipboardError, setClipboardError] = useState('');

  const copySelection = async (): Promise<void> => {
    const selection = terminalRef.current?.getSelection() ?? '';
    if (!selection) return;
    try {
      await navigator.clipboard.writeText(selection);
      setClipboardError('');
    } catch {
      setClipboardError('クリップボードへのアクセスが拒否されました');
    }
  };

  const pasteClipboard = async (): Promise<void> => {
    try {
      const text = await navigator.clipboard.readText();
      if (text) terminalRef.current?.paste(text);
      setClipboardError('');
    } catch {
      setClipboardError('貼り付けにはブラウザのクリップボード許可が必要です');
    }
  };

  useEffect(() => {
    callbacksRef.current = { onStateChange, onRestart, onClose };
  }, [onStateChange, onRestart, onClose]);

  useEffect(() => {
    activeRef.current = active;
    if (!active) return;
    const frame = window.requestAnimationFrame(() => {
      try {
        fitRef.current?.fit();
      } catch {
        // コンテナ寸法確定前は無視
      }
      sendResizeRef.current();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [active]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let disposed = false;
    let socket: WebSocket | null = null;
    let reconnectTimer: number | null = null;
    let reconnectAttempt = 0;
    let offset = 0;
    let shellExited = false;
    let lastServerMessage = Date.now();

    const term = new XTerm({
      fontFamily: '"Cascadia Code", Consolas, "Courier New", monospace',
      fontSize: 14,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      theme: {
        background: '#090e18',
        foreground: '#dbe5f2',
        cursor: '#7dd3fc',
        selectionBackground: '#2563eb88',
        black: '#0b1220', red: '#fb7185', green: '#86efac', yellow: '#fde68a',
        blue: '#93c5fd', magenta: '#d8b4fe', cyan: '#67e8f9', white: '#e2e8f0',
        brightBlack: '#64748b', brightRed: '#fda4af', brightGreen: '#bbf7d0',
        brightYellow: '#fef08a', brightBlue: '#bfdbfe', brightMagenta: '#e9d5ff',
        brightCyan: '#a5f3fc', brightWhite: '#f8fafc',
      },
    });
    terminalRef.current = term;
    term.attachCustomKeyEventHandler((event) => {
      if (event.type === 'keydown' && event.shiftKey && event.ctrlKey) {
        if (event.key.toLowerCase() === 'c') {
          event.preventDefault();
          void copySelection();
          return false;
        }
        if (event.key.toLowerCase() === 'v') {
          event.preventDefault();
          void pasteClipboard();
          return false;
        }
      }
      return true;
    });
    const fitAddon = new FitAddon();
    fitRef.current = fitAddon;
    term.loadAddon(fitAddon);
    term.open(container);
    try {
      fitAddon.fit();
    } catch {
      // コンテナ寸法確定前は無視
    }

    const encoder = new TextEncoder();
    const sendResize = (): void => {
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }));
      }
    };
    sendResizeRef.current = sendResize;

    const fit = (): void => {
      if (!activeRef.current) return;
      try {
        fitAddon.fit();
      } catch {
        // 無視
      }
      sendResize();
    };

    const resizeObserver = new ResizeObserver(() => fit());
    resizeObserver.observe(container);

    const dataDisposable = term.onData((data) => {
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(encoder.encode(data));
      }
    });

    const detachSocket = (): void => {
      const current = socket;
      socket = null;
      if (!current) return;
      current.onopen = null;
      current.onmessage = null;
      current.onerror = null;
      current.onclose = null;
      try {
        current.close();
      } catch {
        // 無視
      }
    };

    const scheduleReconnect = (): void => {
      if (disposed || shellExited || reconnectTimer !== null) return;
      callbacksRef.current.onStateChange(sessionId, 'reconnecting');
      const delay = Math.min(500 * 2 ** reconnectAttempt, RECONNECT_MAX_MS) + Math.random() * 400;
      reconnectAttempt += 1;
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, delay);
    };

    const connect = (): void => {
      if (disposed || shellExited) return;
      detachSocket();
      callbacksRef.current.onStateChange(
        sessionId,
        reconnectAttempt === 0 ? 'connecting' : 'reconnecting',
      );
      lastServerMessage = Date.now();

      const url = wsUrl(
        `/api/terminal/ws?session=${encodeURIComponent(sessionId)}&since=${offset}`,
      );
      const ws = new WebSocket(url);
      ws.binaryType = 'arraybuffer';
      socket = ws;

      ws.onopen = () => {
        sendResize();
      };

      ws.onmessage = (event: MessageEvent) => {
        if (disposed) return;
        lastServerMessage = Date.now();

        if (typeof event.data === 'string') {
          let message: ControlMessage;
          try {
            message = JSON.parse(event.data) as ControlMessage;
          } catch {
            return;
          }

          if (message.type === 'ready') {
            if (message.reset !== false) {
              term.reset();
            }
            offset = typeof message.offset === 'number' ? message.offset : 0;

            if (message.exited) {
              shellExited = true;
              setExited(true);
              callbacksRef.current.onStateChange(sessionId, 'exited');
            } else {
              reconnectAttempt = 0;
              callbacksRef.current.onStateChange(sessionId, 'connected');
              sendResize();
            }
          } else if (message.type === 'exit') {
            shellExited = true;
            setExited(true);
            callbacksRef.current.onStateChange(sessionId, 'exited');
          } else if (message.type === 'reconnect') {
            try {
              ws.close();
            } catch {
              // 無視
            }
          }
          return;
        }

        if (event.data instanceof ArrayBuffer) {
          const bytes = new Uint8Array(event.data);
          term.write(bytes);
          offset += bytes.byteLength;
        }
      };

      ws.onclose = () => {
        if (socket === ws) socket = null;
        if (disposed || shellExited) return;
        scheduleReconnect();
        // A session may have been removed while this page was offline (shell
        // exit + server-side reaping, or a server restart). Check after a few
        // failed attempts instead of retrying forever.
        if (reconnectAttempt >= 3) void verifySession();
      };
    };

    const verifySession = async (): Promise<void> => {
      if (disposed || shellExited) return;
      try {
        const response = await api<{ sessions: Array<{ id: string }> }>(
          '/api/terminal/sessions',
        );
        if (disposed || shellExited) return;
        if (!response.sessions.some((session) => session.id === sessionId)) {
          shellExited = true;
          setExited(true);
          callbacksRef.current.onStateChange(sessionId, 'exited');
        }
      } catch {
        // Still offline; the normal retry loop continues.
      }
    };

    const heartbeat = (): void => {
      if (disposed || shellExited) return;
      if (
        document.visibilityState === 'visible' &&
        socket?.readyState === WebSocket.OPEN &&
        Date.now() - lastServerMessage > STALE_AFTER_MS
      ) {
        // Half-open connection (often caused by an idle proxy): force the
        // close handler to reconnect from the last received offset.
        try {
          socket.close();
        } catch {
          // 無視
        }
        return;
      }
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ type: 'ping' }));
      }
    };

    const reconnectNow = (): void => {
      if (disposed || shellExited) return;
      if (socket?.readyState === WebSocket.CONNECTING) return;
      if (socket?.readyState === WebSocket.OPEN) {
        if (Date.now() - lastServerMessage <= STALE_AFTER_MS) {
          fit();
          return;
        }
        try {
          socket.close();
        } catch {
          // 無視
        }
        return;
      }
      if (reconnectTimer !== null) {
        window.clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      reconnectAttempt = 0;
      connect();
    };

    const onVisibility = (): void => {
      if (document.visibilityState === 'visible') reconnectNow();
    };
    const onOnline = (): void => reconnectNow();

    connect();
    const pingTimer = window.setInterval(heartbeat, PING_INTERVAL_MS);
    document.addEventListener('visibilitychange', onVisibility);
    window.addEventListener('online', onOnline);

    return () => {
      disposed = true;
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);
      window.clearInterval(pingTimer);
      document.removeEventListener('visibilitychange', onVisibility);
      window.removeEventListener('online', onOnline);
      resizeObserver.disconnect();
      dataDisposable.dispose();
      detachSocket();
      sendResizeRef.current = () => {};
      fitRef.current = null;
      terminalRef.current = null;
      term.dispose();
    };
  }, [sessionId]);

  return (
    <div className={cn('absolute inset-0', active ? 'block' : 'hidden')}>
      <div
        ref={containerRef}
        className="h-full w-full rounded-lg border border-slate-700 bg-[#090e18] p-3 pt-11 shadow-inner shadow-black/30"
      />
      <div className="absolute right-3 top-3 z-10 flex items-center gap-1 rounded-md border border-slate-700 bg-slate-900/95 p-1 shadow-lg">
        <Button size="sm" variant="ghost" className="h-7 gap-1.5 px-2 text-xs text-slate-200 hover:bg-slate-700" onClick={() => void copySelection()} title="選択範囲をコピー (Ctrl+Shift+C)">
          <Clipboard className="h-3.5 w-3.5" /> コピー
        </Button>
        <Button size="sm" variant="ghost" className="h-7 gap-1.5 px-2 text-xs text-slate-200 hover:bg-slate-700" onClick={() => void pasteClipboard()} title="クリップボードから貼り付け (Ctrl+Shift+V)">
          <ClipboardPaste className="h-3.5 w-3.5" /> 貼り付け
        </Button>
      </div>
      {clipboardError && <div className="absolute bottom-3 right-3 z-10 rounded bg-amber-950/95 px-2 py-1 text-xs text-amber-200">{clipboardError}</div>}
      {exited && (
        <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 rounded-lg bg-zinc-950/85 backdrop-blur-sm">
          <p className="text-sm font-medium text-zinc-100">セッション終了</p>
          <div className="flex items-center gap-2">
            <Button onClick={() => callbacksRef.current.onRestart(sessionId)}>
              <RefreshCw className="h-3.5 w-3.5" />
              再起動
            </Button>
            <Button variant="outline" onClick={() => callbacksRef.current.onClose(sessionId)}>
              <X className="h-3.5 w-3.5" />
              タブを閉じる
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
