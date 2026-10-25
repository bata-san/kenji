import * as React from 'react';
import { cva, type VariantProps } from 'class-variance-authority';
import { cn } from '@/lib/utils';

const badgeVariants = cva(
  'inline-flex shrink-0 items-center gap-1.5 rounded-full border px-2 py-0.5 text-[11px] font-medium leading-none transition-colors',
  {
    variants: {
      variant: {
        default: 'border-transparent bg-primary text-primary-foreground',
        secondary: 'border-transparent bg-zinc-100 text-zinc-600',
        destructive: 'border-transparent bg-destructive text-destructive-foreground',
        outline: 'border-zinc-200 text-zinc-700',
        info: 'border-blue-200 bg-blue-50 text-blue-700',
        success: 'border-emerald-200 bg-emerald-50 text-emerald-700',
        warning: 'border-amber-200 bg-amber-50 text-amber-700',
        danger: 'border-red-200 bg-red-50 text-red-700',
        violet: 'border-violet-200 bg-violet-50 text-violet-700',
      },
    },
    defaultVariants: {
      variant: 'secondary',
    },
  },
);

/** バリアントごとのドット色 */
const DOT_COLORS: Record<NonNullable<BadgeProps['variant']>, string> = {
  default: 'bg-primary',
  secondary: 'bg-zinc-400',
  destructive: 'bg-destructive',
  outline: 'bg-zinc-400',
  info: 'bg-blue-500',
  success: 'bg-emerald-500',
  warning: 'bg-amber-500',
  danger: 'bg-red-500',
  violet: 'bg-violet-500',
};

export interface BadgeProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof badgeVariants> {
  /** true のとき先頭にカラードットを表示する */
  dot?: boolean;
}

function Badge({ className, variant, dot = false, children, ...props }: BadgeProps) {
  return (
    <div className={cn(badgeVariants({ variant }), className)} {...props}>
      {dot && <span className={cn('h-1.5 w-1.5 rounded-full', DOT_COLORS[variant ?? 'secondary'])} />}
      {children}
    </div>
  );
}

export { Badge, badgeVariants };
