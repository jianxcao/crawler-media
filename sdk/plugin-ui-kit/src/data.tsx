import * as React from "react";
import {
  joinClassNames,
  type DataTableProps,
  type StatBadgeProps,
  type StatusPillProps,
} from "./types";

export function DataTable<T>({
  columns,
  rows,
  getRowKey,
  emptyMessage = "No entries",
  onRowClick,
  className,
}: DataTableProps<T>) {
  return (
    <div className={joinClassNames("cm-plugin-table-wrap", className)}>
      <table className="cm-plugin-table">
        <thead>
          <tr>
            {columns.map((col) => (
              <th
                key={col.key}
                style={col.align ? { textAlign: col.align } : undefined}
              >
                {col.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 ? (
            <tr>
              <td className="cm-plugin-table__empty" colSpan={columns.length}>
                {emptyMessage}
              </td>
            </tr>
          ) : (
            rows.map((row, idx) => (
              <tr
                key={getRowKey(row, idx)}
                data-clickable={onRowClick ? "true" : undefined}
                onClick={onRowClick ? () => onRowClick(row) : undefined}
              >
                {columns.map((col) => (
                  <td
                    key={col.key}
                    style={col.align ? { textAlign: col.align } : undefined}
                  >
                    {col.render(row)}
                  </td>
                ))}
              </tr>
            ))
          )}
        </tbody>
      </table>
    </div>
  );
}

export function StatBadge({
  label,
  value,
  hint,
  tone = "neutral",
  className,
}: StatBadgeProps) {
  return (
    <div
      className={joinClassNames("cm-plugin-stat-badge", className)}
      data-tone={tone}
    >
      <span className="cm-plugin-stat-badge__label">{label}</span>
      <strong className="cm-plugin-stat-badge__value">{value}</strong>
      {hint ? <span className="cm-plugin-stat-badge__hint">{hint}</span> : null}
    </div>
  );
}

export function StatusPill({
  label,
  tone = "neutral",
  className,
}: StatusPillProps) {
  return (
    <span
      className={joinClassNames("cm-plugin-status-pill", className)}
      data-tone={tone}
    >
      {label}
    </span>
  );
}
