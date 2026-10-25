import { useEffect, useState } from 'react';
import { NavLink, Outlet, useNavigate } from 'react-router-dom';
import {
  FolderOpen,
  LayoutDashboard,
  ListChecks,
  LogOut,
  Server,
  Settings as SettingsIcon,
  Terminal as TerminalIcon,
  type LucideIcon,
} from 'lucide-react';
import { api, clearToken, type Me } from '@/api';
import { cn } from '@/lib/utils';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';

interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
  end?: boolean;
}

const NAV: NavItem[] = [
  { to: '/', label: 'ダッシュボード', icon: LayoutDashboard, end: true },
  { to: '/jobs', label: 'ジョブ', icon: ListChecks },
  { to: '/terminal', label: 'ターミナル', icon: TerminalIcon },
  { to: '/files', label: 'ファイル', icon: FolderOpen },
  { to: '/settings', label: '設定', icon: SettingsIcon },
];

const ROLE_META: Record<string, { label: string; variant: 'secondary' | 'info' | 'outline'; className?: string }> = {
  viewer: { label: 'ビューアー', variant: 'secondary' },
  operator: { label: 'オペレーター', variant: 'info' },
  admin: { label: '管理者', variant: 'outline', className: 'border-amber-300 bg-amber-50 text-amber-700' },
};

export default function Layout() {
  const navigate = useNavigate();
  const [me, setMe] = useState<Me | null>(null);

  useEffect(() => {
    void api<Me>('/api/auth/me')
      .then(setMe)
      .catch(() => setMe(null));
  }, []);

  function logout(): void {
    clearToken();
    navigate('/login');
  }

  const role = me ? ROLE_META[me.role] : null;

  return (
    <div className="flex h-screen overflow-hidden bg-background text-foreground">
      <aside className="fixed inset-y-0 left-0 z-20 flex w-[220px] flex-col border-r border-zinc-200 bg-white">
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-zinc-200 px-4">
          <Server className="h-4 w-4 text-blue-600" />
          <span className="text-sm font-semibold tracking-wide">Kenji</span>
        </div>
        <nav className="flex-1 space-y-0.5 overflow-y-auto p-2">
          {NAV.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.end}
              className={({ isActive }) =>
                cn(
                  'flex items-center gap-2.5 rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors',
                  isActive
                    ? 'bg-zinc-100 text-zinc-900'
                    : 'text-zinc-500 hover:bg-zinc-100/70 hover:text-zinc-900',
                )
              }
            >
              <item.icon className="h-4 w-4 shrink-0" />
              {item.label}
            </NavLink>
          ))}
        </nav>
        <div className="border-t border-zinc-200 px-4 py-2.5 text-[10px] text-zinc-400">
          Kenji — リモートコンピューティング基盤
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col pl-[220px]">
        <header className="flex h-12 shrink-0 items-center justify-end gap-3 border-b border-zinc-200 bg-white px-4">
          {me && <span className="text-xs text-zinc-600">{me.username}</span>}
          {role && (
            <Badge variant={role.variant} className={role.className}>
              {role.label}
            </Badge>
          )}
          <Button variant="ghost" onClick={logout}>
            <LogOut className="h-3.5 w-3.5" />
            ログアウト
          </Button>
        </header>
        <main className="flex min-h-0 flex-1 flex-col overflow-hidden">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
