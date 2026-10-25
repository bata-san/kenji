import { useEffect, useState } from 'react';
import { Monitor, ShieldCheck } from 'lucide-react';
import { api, errorMessage, formatBytes, type ResourceStatus, type Settings } from '@/api';

export default function DesktopPriority() {
  const [status, setStatus] = useState<ResourceStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    let active = true;
    void api<Settings>('/api/settings').then(s => { if (active) setStatus(s.resources); }).catch(() => {});
    return () => { active = false; };
  }, []);
  async function toggle() {
    if (!status || busy) return;
    setBusy(true);
    try {
      const next = await api<Settings>('/api/settings', { method: 'PUT', body: JSON.stringify({ desktop_priority: !status.enabled }) });
      setStatus(next.resources); setError('');
    } catch (e) { setError(errorMessage(e)); }
    finally { setBusy(false); }
  }
  if (!status) return null;
  return <section className={`rounded-xl border p-5 ${status.enabled ? 'border-emerald-300 bg-emerald-50' : 'border-zinc-200 bg-white'}`}>
    <div className="flex items-center justify-between gap-5">
      <div>
        <div className="flex items-center gap-2 text-base font-semibold"><Monitor className="h-5 w-5" />PC操作を優先</div>
        <p className="mt-2 text-xs leading-5 text-zinc-600">重いジョブを動かしながら、このPCを操作するための余力を残します。</p>
        <p className="mt-1 text-xs text-zinc-500">管理ジョブ全体のCPUを{status.cpu_limit_percent}%、メモリを{formatBytes(status.memory_limit_bytes)}までに制限。</p>
      </div>
      <button type="button" role="switch" aria-label="PC操作を優先" aria-checked={status.enabled} disabled={busy || !status.supported} onClick={() => void toggle()}
        className={`relative flex h-16 w-36 shrink-0 items-center rounded-full px-2 shadow-inner transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-emerald-600 disabled:opacity-60 ${status.enabled ? 'bg-emerald-600' : 'bg-zinc-300'}`}>
        <span className={`absolute text-sm font-bold ${status.enabled ? 'left-6 text-white' : 'right-6 text-zinc-600'}`}>{busy ? '適用中' : status.enabled ? 'ON' : 'OFF'}</span>
        <span className={`flex h-12 w-12 items-center justify-center rounded-full bg-white shadow transition-transform ${status.enabled ? 'translate-x-20' : ''}`}><ShieldCheck className={`h-6 w-6 ${status.enabled ? 'text-emerald-600' : 'text-zinc-400'}`} /></span>
      </button>
    </div>
    <p className="mt-3 text-[11px] leading-5 text-zinc-500">実行中・今後のKenji管理ジョブと子プロセスに適用。上限到達でジョブが失敗する場合があります。ターミナル、GPU、他アプリは対象外で、PC全体の応答性を保証するものではありません。</p>
    {(error || status.error) && <p role="alert" className="mt-2 text-xs text-red-700">{error || status.error}</p>}
  </section>;
}
