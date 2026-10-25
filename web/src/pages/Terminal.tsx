import { useEffect, useRef, useState } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { RefreshCw } from 'lucide-react';
import { wsUrl } from '@/api';
import { PageHeader } from '@/components/kit';
import { Button } from '@/components/ui/button';

export default function Terminal() {
  const containerRef = useRef<HTMLDivElement>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const termRef = useRef<XTerm | null>(null);
  const exitedRef = useRef(false);
  const reconnectRef = useRef<() => void>(() => {});
  const [exited, setExited] = useState(false);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const term = new XTerm({
      fontFamily: 'Consolas, "Courier New", monospace',
      fontSize: 13,
      cursorBlink: true,
      theme: {
        background: '#09090b',
        foreground: '#e4e4e7',
        cursor: '#3b82f6',
        selectionBackground: '#3f3f46',
      },
    });
    termRef.current = term;

    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    try {
      fit.fit();
    } catch {
      // コンテナ寸法確定前は無視
    }

    const encoder = new TextEncoder();
    const decoder = new TextDecoder();

    const sendResize = (): void => {
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }));
      }
    };

    const ro = new ResizeObserver(() => {
      try {
        fit.fit();
      } catch {
        // 無視
      }
      sendResize();
    });
    ro.observe(container);

    // キー入力 → サーバーへバイナリフレーム（UTF-8）
    const dataDisposable = term.onData((data) => {
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(encoder.encode(data));
      }
    });

    const connect = (): void => {
      try {
        wsRef.current?.close();
      } catch {
        // 無視
      }
      exitedRef.current = false;
      setExited(false);

      const ws = new WebSocket(wsUrl('/api/terminal/ws'));
      ws.binaryType = 'arraybuffer';
      wsRef.current = ws;

      ws.onopen = () => {
        sendResize();
      };
      ws.onmessage = (ev: MessageEvent) => {
        if (typeof ev.data === 'string') {
          // テキストフレーム = JSON 制御メッセージ
          try {
            const msg = JSON.parse(ev.data) as { type?: string };
            if (msg.type === 'exit') {
              exitedRef.current = true;
              setExited(true);
            }
          } catch {
            // 無視
          }
        } else if (ev.data instanceof ArrayBuffer) {
          // バイナリフレーム = PTY 出力（UTF-8）
          term.write(decoder.decode(ev.data));
        }
      };
      ws.onclose = () => {
        wsRef.current = null;
        if (!exitedRef.current) {
          // exit フレームなしの切断 = 異常終了
          exitedRef.current = true;
          setExited(true);
        }
      };
    };
    reconnectRef.current = connect;
    connect();
    term.focus();

    return () => {
      ro.disconnect();
      dataDisposable.dispose();
      try {
        wsRef.current?.close();
      } catch {
        // 無視
      }
      wsRef.current = null;
      term.dispose();
      termRef.current = null;
    };
  }, []);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="px-5 pt-4 pb-3">
        <PageHeader title="ターミナル">
          <span className="font-mono text-[11px] text-muted-foreground">PowerShell (ConPTY)</span>
        </PageHeader>
      </div>
      <div className="relative min-h-0 flex-1 px-3 pb-3">
        {/* ターミナル本体は慣例に従いダークパネルのまま */}
        <div ref={containerRef} className="h-full w-full rounded-lg border border-zinc-200 bg-[#09090b] p-2" />
        {exited && (
          <div className="absolute inset-3 z-10 flex flex-col items-center justify-center gap-3 rounded-lg bg-zinc-950/80 backdrop-blur-sm">
            <p className="text-sm font-medium text-zinc-100">セッション終了</p>
            <Button onClick={() => reconnectRef.current()}>
              <RefreshCw className="h-3.5 w-3.5" />
              再接続
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}
