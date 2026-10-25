import { useState, type FormEvent } from 'react';
import { Navigate, useNavigate } from 'react-router-dom';
import { Server } from 'lucide-react';
import { api, getToken, setToken, type LoginResponse } from '@/api';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Separator } from '@/components/ui/separator';
import { ErrorBanner, Field } from '@/components/kit';

export default function Login() {
  const navigate = useNavigate();
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const hasToken = getToken() !== null;

  async function submit(e: FormEvent): Promise<void> {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const res = await api<LoginResponse>('/api/auth/login', {
        method: 'POST',
        body: JSON.stringify({ username, password }),
      });
      setToken(res.token);
      navigate('/', { replace: true });
    } catch {
      // ロックアウト中も含め、一律同じメッセージを表示する
      setError('ユーザー名またはパスワードが違います');
      setBusy(false);
    }
  }

  if (hasToken) {
    return <Navigate to="/" replace />;
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-zinc-50 p-4">
      <Card className="w-full max-w-sm">
        <CardContent className="p-6">
          <div className="flex flex-col items-center gap-1.5">
            <div className="mb-2 flex h-11 w-11 items-center justify-center rounded-lg border border-zinc-200 bg-zinc-50">
              <Server className="h-5 w-5 text-blue-600" />
            </div>
            <h1 className="text-xl font-semibold tracking-wide text-zinc-900">Kenji</h1>
            <p className="text-xs text-muted-foreground">リモートコンピューティング基盤</p>
          </div>
          <Separator className="my-5" />
          <form className="space-y-3" onSubmit={(e) => void submit(e)}>
            <Field label="ユーザー名">
              <Input
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                autoComplete="username"
                autoFocus
                required
              />
            </Field>
            <Field label="パスワード">
              <Input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="current-password"
                required
              />
            </Field>
            {error && <ErrorBanner>{error}</ErrorBanner>}
            <Button type="submit" className="w-full" loading={busy}>
              ログイン
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
