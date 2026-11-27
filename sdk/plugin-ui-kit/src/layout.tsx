import * as React from "react";
import {
  joinClassNames,
  type ActionBarProps,
  type CardProps,
  type EmptyStateProps,
  type PageHeaderProps,
} from "./types";

export function PageHeader({
  title,
  subtitle,
  badge,
  actions,
  className,
}: PageHeaderProps) {
  return (
    <header className={joinClassNames("cm-plugin-page-header", className)}>
      <div>
        <div className="cm-plugin-page-header__title-row">
          <h1 className="cm-plugin-page-header__title">{title}</h1>
          {badge}
        </div>
        {subtitle ? (
          <p className="cm-plugin-page-header__subtitle">{subtitle}</p>
        ) : null}
      </div>
      {actions ? (
        <div className="cm-plugin-page-header__actions">{actions}</div>
      ) : null}
    </header>
  );
}

export function Card({
  title,
  description,
  actions,
  footer,
  children,
  className,
}: CardProps) {
  return (
    <section className={joinClassNames("cm-plugin-card", className)}>
      {title || description || actions ? (
        <div className="cm-plugin-card__header">
          <div>
            {title ? <h2 className="cm-plugin-card__title">{title}</h2> : null}
            {description ? (
              <p className="cm-plugin-card__description">{description}</p>
            ) : null}
          </div>
          {actions ? (
            <div className="cm-plugin-action-bar__group">{actions}</div>
          ) : null}
        </div>
      ) : null}
      {children}
      {footer ? <div className="cm-plugin-card__footer">{footer}</div> : null}
    </section>
  );
}

export function ActionBar({
  items,
  primary,
  secondary,
  align = "between",
  children,
  className,
}: ActionBarProps) {
  return (
    <div
      className={joinClassNames("cm-plugin-action-bar", className)}
      data-align={align}
    >
      {secondary ? (
        <div className="cm-plugin-action-bar__group">{secondary}</div>
      ) : null}
      {items && items.length > 0 ? (
        <div className="cm-plugin-action-bar__group">
          {items.map((item) => (
            <button
              key={item.id}
              type="button"
              className="cm-plugin-btn"
              data-tone={item.tone ?? "default"}
              disabled={item.disabled || item.loading}
              onClick={item.onClick}
            >
              {item.loading ? `${item.label}...` : item.label}
            </button>
          ))}
        </div>
      ) : null}
      {primary ? (
        <div className="cm-plugin-action-bar__group">{primary}</div>
      ) : null}
      {children}
    </div>
  );
}

export function EmptyState({
  title,
  description,
  icon,
  action,
  className,
}: EmptyStateProps) {
  return (
    <section
      className={joinClassNames("cm-plugin-empty-state", className)}
      aria-label={typeof title === "string" ? title : undefined}
    >
      {icon ? <div aria-hidden="true">{icon}</div> : null}
      <h3 className="cm-plugin-empty-state__title">{title}</h3>
      {description ? (
        <p className="cm-plugin-empty-state__description">{description}</p>
      ) : null}
      {action ? <div>{action}</div> : null}
    </section>
  );
}
