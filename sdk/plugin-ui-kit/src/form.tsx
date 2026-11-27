import * as React from "react";
import { useEffect } from "react";
import {
  joinClassNames,
  type ConfirmModalProps,
  type FormFieldProps,
} from "./types";

export function FormField({
  label,
  htmlFor,
  description,
  error,
  required,
  children,
  className,
}: FormFieldProps) {
  return (
    <div className={joinClassNames("cm-plugin-form-field", className)}>
      <label className="cm-plugin-form-field__label" htmlFor={htmlFor}>
        {label}
        {required ? (
          <span className="cm-plugin-form-field__required" aria-hidden="true">
            *
          </span>
        ) : null}
      </label>
      {children}
      {description ? (
        <p className="cm-plugin-form-field__description">{description}</p>
      ) : null}
      {error ? (
        <p className="cm-plugin-form-field__error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

export function ConfirmModal({
  open,
  title,
  message,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  tone = "default",
  busy = false,
  onConfirm,
  onCancel,
}: ConfirmModalProps) {
  useEffect(() => {
    if (!open) {
      return;
    }
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) {
        onCancel();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open, busy, onCancel]);

  if (!open) {
    return null;
  }

  return (
    <div
      className="cm-plugin-confirm-backdrop"
      role="presentation"
      onClick={() => {
        if (!busy) {
          onCancel();
        }
      }}
    >
      <div
        className="cm-plugin-confirm-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={typeof title === "string" ? title : undefined}
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="cm-plugin-confirm-dialog__title">{title}</h2>
        <div className="cm-plugin-confirm-dialog__message">{message}</div>
        <div className="cm-plugin-action-bar" data-align="end">
          <div className="cm-plugin-action-bar__group">
            <button
              type="button"
              className="cm-plugin-btn"
              disabled={busy}
              onClick={onCancel}
            >
              {cancelLabel}
            </button>
            <button
              type="button"
              className="cm-plugin-btn"
              data-tone={tone === "danger" ? "danger" : "primary"}
              disabled={busy}
              onClick={onConfirm}
            >
              {busy ? `${confirmLabel}...` : confirmLabel}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
