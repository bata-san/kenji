// アプリ共通の小さなヘルパー群（shadcn プリミティブのラッパー）
import { useCallback, type ReactNode } from 'react';
import { Loader2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Label } from '@/components/ui/label';

/** ラベル + 入力コントロール + ヒント/エラー */
export function Field({
  label,
  hint,
  error,
  className,
  children,
}: {
  label: string;
  hint?: string;
  error?: string | null;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={cn('space-y-1', className)}>
      <Label>{label}</Label>
      {children}
      {error ? (
        <p className="text-[11px] font-medium text-red-600">{error}</p>
      ) : hint ? (
        <p className="text-[11px] text-muted-foreground">{hint}</p>
      ) : null}
    </div>
  );
}

/** インラインエラーバナー */
export function ErrorBanner({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
      {children}
    </div>
  );
}

/** 読み込みスピナー */
export function Spinner({ className }: { className?: string }) {
  return <Loader2 className={cn('h-4 w-4 animate-spin text-muted-foreground', className)} />;
}

/** 空状態 */
export function EmptyState({ children }: { children: ReactNode }) {
  return (
    <div className="flex items-center justify-center px-4 py-10 text-xs text-muted-foreground">
      {children}
    </div>
  );
}

/** ページ見出し + 右側のアクション類 */
export function PageHeader({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <h1 className="text-base font-semibold text-foreground">{title}</h1>
      <div className="flex items-center gap-2">{children}</div>
    </div>
  );
}

/** window.confirm ベースの確認ダイアログ */
export function useConfirm(): (message: string) => boolean {
  return useCallback((message: string) => window.confirm(message), []);
}
