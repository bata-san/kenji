import { useEffect, useRef, useState } from 'react';
import { Activity, Cpu, ExternalLink, Globe, HardDrive, MemoryStick, Zap } from 'lucide-react';
import {
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { api, formatBytes, formatUptime, type Overview } from '@/api';
import { Badge } from '@/components/ui/badge';
import { Card, CardHeader, CardTitle } from '@/components/ui/card';
import { Progress } from '@/components/ui/progress';
import { PageHeader } from '@/components/kit';

interface Sample {
  t: string;
  cpu: number;
  mem: number;
}

const MAX_SAMPLES = 60;

export default function Dashboard() {
  const [overview, setOverview] = useState<Overview | null>(null);
  const [samples, setSamples] = useState<Sample[]>([]);
  const [offline, setOffline] = useState(false);
  const failRef = useRef(0);

  useEffect(() => {
    let cancelled = false;

    const tick = async (): Promise<void> => {
      try {
        const ov = await api<Overview>('/api/system/overview');
        if (cancelled) return;
        failRef.current = 0;
        setOffline(false);
        setOverview(ov);
        const memPct =
          ov.memory.total_bytes > 0 ? (ov.memory.used_bytes / ov.memory.total_bytes) * 100 : 0;
        const t = new Date().toLocaleTimeString('ja-JP', { hour12: false });
        setSamples((prev) => {
          const next = [...prev, { t, cpu: ov.cpu.usage_percent, mem: Math.round(memPct * 10) / 10 }];
          return next.length > MAX_SAMPLES ? next.slice(next.length - MAX_SAMPLES) : next;
        });
      } catch {
        if (cancelled) return;
        failRef.current += 1;
        if (failRef.current >= 3) setOffline(true);
      }
    };

    void tick();
    const id = window.setInterval(() => void tick(), 2000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, []);

  const ov = overview;

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="mx-auto max-w-[1600px] space-y-3 p-5">
        <PageHeader title="ダッシュボード">
          {offline && (
            <Badge variant="danger" dot>
              接続なし
            </Badge>
          )}
          {ov && (
            <span className="font-mono text-[11px] text-muted-foreground">
              {ov.hostname} · {ov.os} · 稼働 {formatUptime(ov.uptime_secs)}
            </span>
          )}
        </PageHeader>

        {ov ? (
          <>
            <div className="grid grid-cols-4 gap-3">
              <Card className="p-3.5">
                <div className="flex items-center gap-1.5 text-muted-foreground">
                  <Cpu className="h-3.5 w-3.5" />
                  <span className="text-[11px] font-medium">CPU使用率</span>
                </div>
                <div className="mt-2 text-2xl font-semibold tabular-nums text-zinc-900">
                  {ov.cpu.usage_percent.toFixed(1)}
                  <span className="text-sm text-muted-foreground">%</span>
                </div>
                <div className="mt-1 text-[11px] text-muted-foreground">{ov.cpu.cores} コア</div>
              </Card>

              <Card className="p-3.5">
                <div className="flex items-center gap-1.5 text-muted-foreground">
                  <MemoryStick className="h-3.5 w-3.5" />
                  <span className="text-[11px] font-medium">メモリ</span>
                </div>
                <div className="mt-2.5 font-mono text-sm tabular-nums text-zinc-900">
                  {formatBytes(ov.memory.used_bytes)}
                  <span className="text-muted-foreground"> / {formatBytes(ov.memory.total_bytes)}</span>
                </div>
                <Progress
                  className="mt-2 h-1.5"
                  value={
                    ov.memory.total_bytes > 0
                      ? (ov.memory.used_bytes / ov.memory.total_bytes) * 100
                      : 0
                  }
                />
                <div className="mt-1 text-[11px] tabular-nums text-muted-foreground">
                  {ov.memory.total_bytes > 0
                    ? ((ov.memory.used_bytes / ov.memory.total_bytes) * 100).toFixed(1)
                    : '0.0'}
                  % 使用中
                </div>
              </Card>

              <Card className="p-3.5">
                <div className="flex items-center gap-1.5 text-muted-foreground">
                  <Activity className="h-3.5 w-3.5" />
                  <span className="text-[11px] font-medium">ジョブ概要</span>
                </div>
                <div className="mt-2.5 flex items-end gap-5">
                  <div>
                    <span className="text-xl font-semibold tabular-nums text-blue-600">
                      {ov.jobs_summary.running}
                    </span>
                    <span className="ml-1 text-[11px] text-muted-foreground">実行中</span>
                  </div>
                  <div>
                    <span className="text-xl font-semibold tabular-nums text-zinc-900">
                      {ov.jobs_summary.total}
                    </span>
                    <span className="ml-1 text-[11px] text-muted-foreground">合計</span>
                  </div>
                  <div>
                    <span className="text-xl font-semibold tabular-nums text-red-600">
                      {ov.jobs_summary.failed}
                    </span>
                    <span className="ml-1 text-[11px] text-muted-foreground">失敗</span>
                  </div>
                </div>
              </Card>

              {ov.tunnel.url ? (
                <Card className="p-3.5">
                  <div className="flex items-center gap-1.5 text-muted-foreground">
                    <Globe className="h-3.5 w-3.5" />
                    <span className="text-[11px] font-medium">トンネル</span>
                  </div>
                  <a
                    className="mt-3 flex items-center gap-1 truncate font-mono text-xs text-blue-600 hover:underline"
                    href={ov.tunnel.url}
                    target="_blank"
                    rel="noreferrer"
                    title={ov.tunnel.url}
                  >
                    {ov.tunnel.url}
                    <ExternalLink className="h-3 w-3 shrink-0" />
                  </a>
                </Card>
              ) : null}
            </div>

            {ov.gpus.length > 0 && (
              <Card>
                <CardHeader>
                  <CardTitle className="flex items-center gap-2">
                    <Zap className="h-3.5 w-3.5 text-muted-foreground" />
                    GPU
                  </CardTitle>
                </CardHeader>
                <div className="grid grid-cols-2 gap-x-6 gap-y-4 p-3.5">
                  {ov.gpus.map((g) => {
                    const vramPct =
                      g.memory_total_bytes > 0
                        ? (g.memory_used_bytes / g.memory_total_bytes) * 100
                        : 0;
                    return (
                      <div key={g.name}>
                        <div className="truncate text-xs font-medium text-zinc-700" title={g.name}>
                          {g.name}
                        </div>
                        <div className="mt-2 flex items-center justify-between text-[11px] text-muted-foreground">
                          <span>使用率</span>
                          <span className="tabular-nums">{g.utilization_percent}%</span>
                        </div>
                        <Progress className="mt-1 h-1.5" value={g.utilization_percent} />
                        <div className="mt-2 flex items-center justify-between text-[11px] text-muted-foreground">
                          <span>VRAM</span>
                          <span className="font-mono tabular-nums">
                            {formatBytes(g.memory_used_bytes)} / {formatBytes(g.memory_total_bytes)}
                          </span>
                        </div>
                        <Progress className="mt-1 h-1.5" value={vramPct} indicatorClassName="bg-emerald-500" />
                      </div>
                    );
                  })}
                </div>
              </Card>
            )}

            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <HardDrive className="h-3.5 w-3.5 text-muted-foreground" />
                  ディスク
                </CardTitle>
              </CardHeader>
              <div className="grid grid-cols-2 gap-x-6 gap-y-4 p-3.5">
                {ov.disks.map((d) => {
                  const usedPct =
                    d.total_bytes > 0
                      ? ((d.total_bytes - d.available_bytes) / d.total_bytes) * 100
                      : 0;
                  return (
                    <div key={d.mount_point}>
                      <div className="flex items-baseline justify-between">
                        <span className="font-mono text-xs font-medium text-zinc-700">{d.label}</span>
                        <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
                          空き {formatBytes(d.available_bytes)} / {formatBytes(d.total_bytes)}
                        </span>
                      </div>
                      <Progress className="mt-1.5 h-1.5" value={usedPct} />
                    </div>
                  );
                })}
              </div>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <Activity className="h-3.5 w-3.5 text-muted-foreground" />
                  CPU / メモリ 使用率（直近代分）
                </CardTitle>
              </CardHeader>
              <div className="h-[220px] p-2 pr-4">
                <ResponsiveContainer width="100%" height="100%">
                  <LineChart data={samples} margin={{ top: 10, right: 5, bottom: 0, left: 0 }}>
                    <CartesianGrid stroke="#e4e4e7" strokeDasharray="3 3" vertical={false} />
                    <XAxis
                      dataKey="t"
                      tick={{ fill: '#71717a', fontSize: 10 }}
                      tickLine={false}
                      axisLine={{ stroke: '#e4e4e7' }}
                      minTickGap={48}
                    />
                    <YAxis
                      domain={[0, 100]}
                      unit="%"
                      tick={{ fill: '#71717a', fontSize: 10 }}
                      tickLine={false}
                      axisLine={false}
                      width={36}
                    />
                    <Tooltip
                      contentStyle={{
                        background: '#ffffff',
                        border: '1px solid #e4e4e7',
                        borderRadius: 8,
                        fontSize: 12,
                      }}
                      labelStyle={{ color: '#71717a' }}
                      itemStyle={{ color: '#18181b' }}
                    />
                    <Legend wrapperStyle={{ fontSize: 11 }} />
                    <Line
                      type="monotone"
                      dataKey="cpu"
                      name="CPU使用率"
                      stroke="#3b82f6"
                      strokeWidth={1.5}
                      dot={false}
                      isAnimationActive={false}
                    />
                    <Line
                      type="monotone"
                      dataKey="mem"
                      name="メモリ使用率"
                      stroke="#10b981"
                      strokeWidth={1.5}
                      dot={false}
                      isAnimationActive={false}
                    />
                  </LineChart>
                </ResponsiveContainer>
              </div>
            </Card>
          </>
        ) : (
          <Card className="flex items-center justify-center p-12 text-xs text-muted-foreground">
            {offline ? 'サーバーに接続できません' : '読み込み中...'}
          </Card>
        )}
      </div>
    </div>
  );
}
