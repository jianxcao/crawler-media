import type { ReactNode } from "react";

export type PluginKitTone = "neutral" | "ok" | "info" | "warn" | "danger";

export interface PageHeaderProps {
  title: ReactNode;
  subtitle?: ReactNode;
  badge?: ReactNode;
  actions?: ReactNode;
  className?: string;
}

export interface CardProps {
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  footer?: ReactNode;
  children?: ReactNode;
  className?: string;
}

export interface DataTableColumn<T> {
  key: string;
  header: ReactNode;
  align?: "left" | "center" | "right";
  render: (row: T) => ReactNode;
}

export interface DataTableProps<T> {
  columns: readonly DataTableColumn<T>[];
  rows: readonly T[];
  getRowKey: (row: T, index: number) => string;
  emptyMessage?: ReactNode;
  onRowClick?: (row: T) => void;
  className?: string;
}

export interface FormFieldProps {
  label: string;
  htmlFor?: string;
  description?: ReactNode;
  error?: ReactNode;
  required?: boolean;
  children?: ReactNode;
  className?: string;
}

export interface StatBadgeProps {
  label: ReactNode;
  value: ReactNode;
  hint?: ReactNode;
  tone?: PluginKitTone;
  className?: string;
}

export interface ActionBarItem {
  id: string;
  label: string;
  tone?: "default" | "primary" | "danger";
  disabled?: boolean;
  loading?: boolean;
  onClick: () => void;
}

export interface ActionBarProps {
  items?: readonly ActionBarItem[];
  primary?: ReactNode;
  secondary?: ReactNode;
  align?: "start" | "between" | "end";
  children?: ReactNode;
  className?: string;
}

export interface ConfirmModalProps {
  open: boolean;
  title: ReactNode;
  message: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  tone?: "default" | "danger";
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export interface EmptyStateProps {
  title: ReactNode;
  description?: ReactNode;
  icon?: ReactNode;
  action?: ReactNode;
  className?: string;
}

export interface StatusPillProps {
  label: ReactNode;
  tone?: PluginKitTone;
  className?: string;
}

export function joinClassNames(
  base: string,
  extra?: string,
): string {
  return extra ? `${base} ${extra}` : base;
}
