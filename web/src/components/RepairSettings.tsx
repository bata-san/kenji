import { useState } from 'react';
import { api, errorMessage, type RepairConfig, type Settings } from '@/api';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import { Input } from '@/components/ui/input';

export default function RepairSettings({ settings, onSaved }: { settings: Settings; onSaved: (s: Settings) => void }) {
  const [draft, setDraft] = useState<RepairConfig>(settings.repair);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  function field(name: keyof RepairConfig, value: string | boolean) { setDraft(prev => ({ ...prev, [name]: value })); }
  async function save() {
    setBusy(true); setMessage('');
    try {
      onSaved(await api<Settings>('/api/settings', { method: 'PUT', body: JSON.stringify({ repair: draft }) }));
      setError(''); setMessage('保存しました。APIキーはサーバープロセスの環境変数に設定してください。環境変数の変更にはサーバーの再起動が必要です。');
    } catch (e) { setError(errorMessage(e)); }
    finally { setBusy(false); }
  }
  async function diagnose() {
    setBusy(true); setMessage('');
    try {
      await api('/api/repair/diagnose', { method: 'POST' });
      setMessage('診断ジョブを起動しました。状態を更新して確認してください。'); setError('');
    } catch (e) { setError(errorMessage(e)); }
    finally { setBusy(false); }
  }
  return <Card className="space-y-4 p-5">
    <div><h2 className="text-sm font-semibold">クラッシュ時のAI診断・修復案</h2>
      <p className="mt-2 text-xs leading-5 text-zinc-600">独立した監視プロセスがサーバーの異常終了を検出し、piベースの診断ジョブを起動します。AIは修復手順を提案しますが、ファイル変更・コマンド実行・自動デプロイはしません。</p></div>
    <label className="flex items-center gap-3 text-sm"><input type="checkbox" checked={draft.enabled} disabled={busy} onChange={e => field('enabled', e.target.checked)} />異常終了時に自動で診断する</label>
    <div className="grid gap-4 sm:grid-cols-2">
      <label className="space-y-1 text-xs">OpenAI互換プロバイダURL<Input value={draft.base_url} onChange={e => field('base_url', e.target.value)} placeholder="https://api.openai.com/v1" /></label>
      <label className="space-y-1 text-xs">モデルID<Input value={draft.model} onChange={e => field('model', e.target.value)} /></label>
      <label className="space-y-1 text-xs">APIキーの環境変数名<Input value={draft.api_key_env} onChange={e => field('api_key_env', e.target.value)} placeholder="KENJI_REPAIR_API_KEY" /></label>
      <label className="space-y-1 text-xs">ハーネス worker.mjs の絶対パス<Input value={draft.harness_path} onChange={e => field('harness_path', e.target.value)} /></label>
    </div>
    <p className="text-xs leading-5 text-zinc-500">事前準備: Node.js 22.19以降と、agentディレクトリでの npm ci --ignore-scripts が必要です。キー本体は保存しません。異常終了コードだけをプロバイダに送信し、生ログ・ソース・認証情報は自動送信しません。診断は最大3分、同時実行1件、再実行間隔は5分以上です。API利用料が発生します。</p>
    <div className="flex flex-wrap gap-2"><Button loading={busy} onClick={() => void save()}>設定を保存</Button><Button variant="secondary" disabled={busy || !settings.repair.enabled} onClick={() => void diagnose()}>手動で診断を試す</Button><Button variant="secondary" onClick={() => void api<Settings>('/api/settings').then(onSaved).catch(e => setError(errorMessage(e)))}>状態を更新</Button></div>
    <div className="rounded-md bg-zinc-50 p-3 text-xs"><p>直近の診断: {({ idle: '待機中', running: '実行中', completed: '完了', failed: '失敗' })[settings.repair_status.state] ?? settings.repair_status.state}</p>{settings.repair_status.output_file && <p className="mt-1 break-all">結果ファイル: {settings.repair_status.output_file}</p>}{settings.repair_status.error && <p className="mt-1 text-red-700">{settings.repair_status.error}</p>}</div>
    {message && <p role="status" className="text-xs text-emerald-700">{message}</p>}{error && <p role="alert" className="text-xs text-red-700">{error}</p>}
  </Card>;
}
