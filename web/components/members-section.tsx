"use client";

/**
 * 设置 -> 成员：成员账号、能力与资源范围的唯一管理入口。
 *
 * 列表只负责扫描状态，创建和编辑使用独立弹窗，避免在表格中展开长表单导致
 * 行高跳变。创建/重置产生的明文密码进入一次性结果弹窗，关闭后前端也不再
 * 保留，和后端“仅返回一次”的凭据语义一致。
 */

import { useCallback, useEffect, useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import { AvatarBadge } from "@/components/avatar-badge";
import { BrandLoader } from "@/components/brand-loader";
import { copyText } from "@/components/copy-button";
import { useConfirm, useToast } from "@/components/feedback";
import { CheckIcon, MoreIcon, PlusIcon } from "@/components/icons";
import { Modal } from "@/components/modal";
import {
  createMember,
  deleteMember,
  listMembers,
  resetMemberPassword,
  setMemberStatus,
  updateMember,
  type MemberUpdatePayload,
  type MemberView,
} from "@/lib/api/members";
import { formatRelativeTime } from "@/lib/time";

interface PasswordResult {
  title: string;
  username: string;
  password: string;
}

/** 生成随机初始密码（前端生成，创建前即可复制；后端只保存哈希）。 */
function generatePassword(): string {
  const alphabet = "abcdefghjkmnpqrstuvwxyzACDEFGHJKLMNPQRSTUVWXYZ2345679";
  const bytes = crypto.getRandomValues(new Uint8Array(14));
  return Array.from(bytes, (byte) => alphabet[byte % alphabet.length]).join("");
}

export function MembersSection() {
  const toast = useToast();
  const confirm = useConfirm();
  const [members, setMembers] = useState<MemberView[] | null>(null);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<MemberView | null>(null);
  const [passwordResult, setPasswordResult] = useState<PasswordResult | null>(null);

  const reload = useCallback(async () => setMembers(await listMembers()), []);

  useEffect(() => {
    void reload().catch((error) => toast.error(`加载成员失败：${(error as Error).message}`));
  }, [reload, toast]);

  const replaceMember = (next: MemberView) => {
    setMembers((rows) => rows?.map((row) => (row.id === next.id ? next : row)) ?? rows);
    setEditing((current) => (current?.id === next.id ? next : current));
  };

  const resetPassword = async (member: MemberView) => {
    const accepted = await confirm({
      title: `重置「${member.login}」的密码？`,
      description: "旧密码和该成员的全部登录会立即失效。新密码只显示一次。",
      confirmLabel: "重置密码",
    });
    if (!accepted) return;
    try {
      const result = await resetMemberPassword(member.id);
      setPasswordResult({
        title: "密码已重置",
        username: result.username || member.login,
        password: result.password,
      });
    } catch (error) {
      toast.error(`重置失败：${(error as Error).message}`);
    }
  };

  const toggleStatus = async (member: MemberView) => {
    const enabling = !member.enabled;
    if (!enabling) {
      const accepted = await confirm({
        title: `停用「${member.login}」？`,
        description: "该成员的全部设备会立即下线，个人数据和订阅保留，可随时重新启用。",
        confirmLabel: "停用成员",
        tone: "danger",
      });
      if (!accepted) return;
    }
    try {
      await setMemberStatus(member.id, enabling);
      await reload();
      toast.success(enabling ? "成员已启用" : "成员已停用");
    } catch (error) {
      toast.error(`操作失败：${(error as Error).message}`);
    }
  };

  const removeMember = async (member: MemberView) => {
    const accepted = await confirm({
      title: `删除成员「${member.login}」？`,
      description:
        "订阅转由管理员接管，已下载内容不受影响。此操作不可恢复。",
      confirmLabel: "删除成员",
      tone: "danger",
    });
    if (!accepted) return;
    try {
      await deleteMember(member.id);
      setMembers((rows) => rows?.filter((row) => row.id !== member.id) ?? rows);
      setEditing(null);
      toast.success("成员已删除");
    } catch (error) {
      toast.error(`删除失败：${(error as Error).message}`);
    }
  };

  return (
    <section>
      <div className="mb-3 flex items-center justify-between gap-4">
        <div>
          <h3 className="text-body font-semibold text-[var(--text)]">成员账号</h3>
          <p className="mt-0.5 text-caption text-[var(--text-faint)]">
            管理登录状态、功能权限和可见媒体库
          </p>
        </div>
        <button
          type="button"
          onClick={() => setCreating(true)}
          className="btn-accent flex shrink-0 items-center gap-1.5 rounded-full px-4 py-2 text-sub font-semibold"
        >
          <PlusIcon className="size-4" />
          添加成员
        </button>
      </div>

      <div className="css-glass overflow-hidden !rounded-xl">
        <div className="grid grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)_minmax(88px,130px)_36px] gap-4 border-b border-white/[0.07] px-4 py-2.5 text-caption font-medium text-[var(--text-faint)] max-md:hidden">
          <span>成员</span>
          <span>角色</span>
          <span>创建时间</span>
          <span className="sr-only">操作</span>
        </div>
        {members === null ? (
          <div className="flex items-center justify-center gap-2 px-5 py-10 text-ui text-[var(--text-muted)]">
            <BrandLoader className="size-5" />
            正在加载成员…
          </div>
        ) : members.length === 0 ? (
          <div className="px-5 py-10 text-center">
            <p className="text-body font-medium text-[var(--text)]">还没有成员账号</p>
            <p className="mt-1 text-sub text-[var(--text-muted)]">
              添加成员后即可分别管理登录状态。
            </p>
          </div>
        ) : (
          <div className="divide-y divide-white/[0.055]">
            {members.map((member) => (
              <MemberTableRow
                key={member.id}
                member={member}
                enabled={member.enabled}
                onEdit={() => setEditing(member)}
                onResetPassword={() => void resetPassword(member)}
                onToggleStatus={() => void toggleStatus(member)}
                onDelete={() => void removeMember(member)}
              />
            ))}
          </div>
        )}
      </div>

      <CreateMemberDialog
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(member, password) => {
          setCreating(false);
          setMembers((rows) => (rows ? [...rows, member] : [member]));
          setPasswordResult({ title: "成员已创建", username: member.login, password });
        }}
      />

      {editing && (
        <EditMemberDialog
          key={editing.id}
          member={editing}
          enabled={editing.enabled}
          onClose={() => setEditing(null)}
          onSaved={async (next) => {
            replaceMember(next);
            await reload();
            setEditing(null);
            toast.success("成员设置已保存");
          }}
        />
      )}

      <PasswordResultDialog result={passwordResult} onClose={() => setPasswordResult(null)} />
    </section>
  );
}

function MemberTableRow({
  member,
  enabled,
  onEdit,
  onResetPassword,
  onToggleStatus,
  onDelete,
}: {
  member: MemberView;
  enabled: boolean;
  onEdit: () => void;
  onResetPassword: () => void;
  onToggleStatus: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="grid grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)_minmax(88px,130px)_36px] items-center gap-4 px-4 py-3.5 transition-colors hover:bg-white/[0.025] max-md:grid-cols-[minmax(0,1fr)_36px] max-md:gap-x-3 max-md:gap-y-2.5">
      <div className="flex min-w-0 items-center gap-3">
        <AvatarBadge nickname={member.login} avatarUrl={null} className="size-9" />
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="truncate text-ui font-semibold text-[var(--text)]">
              {member.login}
            </span>
            <span
              className={`size-1.5 shrink-0 rounded-full ${enabled ? "bg-[var(--ok)]" : "bg-white/25"}`}
              title={enabled ? "已启用" : "已停用"}
            />
          </div>
          <p className="truncate text-caption text-[var(--text-faint)]">@{member.login}</p>
        </div>
      </div>
      <div className="flex min-w-0 flex-wrap gap-1.5 max-md:col-start-1">
        <Badge>{member.role === "admin" ? "管理员" : "成员"}</Badge>
      </div>
      {/* min-w-0：网格子项默认 min-width:auto，会把整行撑到内容宽度、把最右的
          ⋯ 列挤出 overflow-hidden 的容器外——PC 上窄一点的设置面板就看不到菜单 */}
      <p className="min-w-0 truncate text-caption text-[var(--text-muted)] max-md:col-start-1">
        {formatRelativeTime(member.created_at)}
      </p>
      <MemberActionsMenu
        member={member}
        enabled={enabled}
        onEdit={onEdit}
        onResetPassword={onResetPassword}
        onToggleStatus={onToggleStatus}
        onDelete={onDelete}
      />
    </div>
  );
}

function MemberActionsMenu({
  member,
  enabled,
  onEdit,
  onResetPassword,
  onToggleStatus,
  onDelete,
}: {
  member: MemberView;
  enabled: boolean;
  onEdit: () => void;
  onResetPassword: () => void;
  onToggleStatus: () => void;
  onDelete: () => void;
}) {
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2 text-sub font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)]";

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          aria-label={`${member.login} 的更多操作`}
          title="更多操作"
          className="glass-row !size-8 justify-center !p-0 data-[state=open]:!bg-[var(--glass-fill-active)] data-[state=open]:!text-[var(--text)] max-md:col-start-2 max-md:row-start-1"
        >
          <MoreIcon className="size-4" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={6}
          collisionPadding={12}
          className="menu-surface z-50 min-w-[9rem] p-1"
        >
          <DropdownMenu.Item onSelect={onEdit} className={itemClass}>
            编辑成员
          </DropdownMenu.Item>
          <DropdownMenu.Item onSelect={onResetPassword} className={itemClass}>
            重置密码
          </DropdownMenu.Item>
          <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />
          <DropdownMenu.Item
            onSelect={onToggleStatus}
            className={`${itemClass} ${enabled ? "!text-[#ffb36b] data-[highlighted]:!bg-[#ffb36b]/10" : ""}`}
          >
            {enabled ? "停用成员" : "启用成员"}
          </DropdownMenu.Item>
          <DropdownMenu.Item
            onSelect={onDelete}
            className={`${itemClass} !text-[#ff6b6b] data-[highlighted]:!bg-[#ff6b6b]/10`}
          >
            删除成员
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

function CreateMemberDialog({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: (member: MemberView, password: string) => void;
}) {
  const toast = useToast();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState(() => generatePassword());
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setUsername("");
    setPassword(generatePassword());
    setBusy(false);
  }, [open]);

  const submit = async () => {
    if (username.trim().length < 3) return toast.error("用户名至少 3 个字符");
    if (password.length < 8) return toast.error("密码至少 8 位");
    setBusy(true);
    try {
      const member = await createMember(username.trim(), password, "");
      onCreated(member, password);
    } catch (error) {
      toast.error(`创建失败：${(error as Error).message}`);
      setBusy(false);
    }
  };

  return (
    <Modal open={open} onClose={onClose} label="添加成员" width="lg">
      <div className="p-6 max-md:p-5">
        <h2 className="text-title font-bold text-white">添加成员</h2>
        <p className="mt-1 text-sub text-[var(--text-muted)]">
          新成员以用户名登录，创建后由管理员管理启用状态。
        </p>

        <div className="mt-5">
          <Field label="用户名" hint="登录使用，创建后不可修改">
            <input
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              autoFocus
              autoComplete="off"
              placeholder="如 yee"
              className={INPUT_CLASS}
            />
          </Field>
        </div>
        <div className="mt-4">
          <SectionTitle
            title="初始密码"
            description="已随机生成。点击密码行即可复制，创建成功后还会显示一次。"
          />
          <div className="overflow-hidden rounded-xl border border-white/[0.08] bg-black/20">
            <CredentialRow label="密码" value={password} mono />
            <button
              type="button"
              onClick={() => setPassword(generatePassword())}
              className="flex w-full items-center justify-between border-t border-white/[0.08] bg-white/[0.035] px-4 py-3 text-left text-sub font-medium text-white/85 transition-colors hover:bg-white/[0.07]"
            >
              重新生成密码
              <span className="text-caption text-[var(--text-faint)]">换一个随机密码</span>
            </button>
          </div>
        </div>

        <div className="mt-6 flex justify-end gap-3">
          <button type="button" onClick={onClose} className="btn-glass h-9 px-4 text-ui font-medium">
            取消
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => void submit()}
            className="btn-accent h-9 rounded-full px-5 text-ui font-semibold disabled:opacity-40"
          >
            {busy ? "创建中…" : "创建成员"}
          </button>
        </div>
      </div>
    </Modal>
  );
}

function EditMemberDialog({
  member,
  enabled,
  onClose,
  onSaved,
}: {
  member: MemberView;
  enabled: boolean;
  onClose: () => void;
  onSaved: (member: MemberView) => void | Promise<void>;
}) {
  const toast = useToast();
  const [login, setLogin] = useState(member.login);
  const [newEnabled, setNewEnabled] = useState(enabled);
  const [busy, setBusy] = useState(false);

  const save = async () => {
    setBusy(true);
    // 新契约成员只有 login/password/enabled 三个字段：功能权限、内容分级与
    // 媒体库/站点范围已下沉为会话级能力，不再按成员编辑
    const payload: MemberUpdatePayload = { enabled: newEnabled };
    if (login.trim().length > 0 && login.trim() !== member.login) payload.login = login.trim();
    try {
      await onSaved(await updateMember(member.id, payload));
    } catch (error) {
      toast.error(`保存失败：${(error as Error).message}`);
      setBusy(false);
    }
  };

  return (
    <Modal
      open
      onClose={onClose}
      label={`编辑成员 ${member.login}`}
      width="md"
      panelClassName="flex max-h-[82dvh] flex-col"
    >
      <div className="scroll-thin overflow-y-auto p-6 max-md:p-5">
        <div className="flex items-center gap-3">
          <AvatarBadge nickname={member.login} avatarUrl={null} className="size-11" />
          <div className="min-w-0">
            <h2 className="truncate text-title font-bold text-white">编辑成员</h2>
            <p className="truncate text-sub text-[var(--text-muted)]">@{member.login}</p>
          </div>
          <StatusBadge enabled={newEnabled} />
        </div>

        <div className="mt-6 border-t border-white/[0.07] pt-5">
          <SectionTitle title="基本信息" />
          <Field label="用户名">
            <input value={login} onChange={(event) => setLogin(event.target.value)} className={INPUT_CLASS} />
          </Field>
        </div>

        <div className="mt-6 border-t border-white/[0.07] pt-5">
          <SectionTitle title="启用状态" description="停用后该成员无法登录，个人数据与订阅保留。" />
          <PermissionToggle
            label="允许登录"
            description="停用后该成员的全部设备立即下线，可随时重新启用"
            checked={newEnabled}
            onChange={setNewEnabled}
          />
        </div>
      </div>

      <div className="flex shrink-0 justify-end gap-3 border-t border-white/[0.07] px-6 py-4 max-md:px-5">
        <button type="button" onClick={onClose} className="btn-glass h-9 px-4 text-ui font-medium">
          取消
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => void save()}
          className="btn-accent flex h-9 items-center gap-1.5 rounded-full px-5 text-ui font-semibold disabled:opacity-40"
        >
          <CheckIcon className="size-4" />
          {busy ? "保存中…" : "保存设置"}
        </button>
      </div>
    </Modal>
  );
}

function PasswordResultDialog({
  result,
  onClose,
}: {
  result: PasswordResult | null;
  onClose: () => void;
}) {
  if (!result) return null;
  const credentialText = `账号：${result.username}\n密码：${result.password}`;
  return (
    <Modal open onClose={onClose} label={result.title} width="md" raised>
      <div className="p-6 max-md:p-5">
        <h2 className="text-title font-bold text-white">{result.title}</h2>
        <p className="mt-1 text-sub leading-6 text-[var(--text-muted)]">
          密码只在这里显示一次。点击账号或密码即可复制，关闭弹窗后无法再次查看。
        </p>
        <div className="mt-5 overflow-hidden rounded-xl border border-white/[0.08] bg-black/20">
          <CredentialRow label="账号" value={result.username} />
          <CredentialRow label="密码" value={result.password} mono />
          <CopySurface
            text={credentialText}
            successMessage="账号和密码已复制"
            idleLabel=""
            className="flex w-full items-center justify-between border-t border-white/[0.08] bg-white/[0.035] px-4 py-3 text-left text-sub font-medium text-white/85 transition-colors hover:bg-white/[0.07]"
          >
            复制全部登录信息
          </CopySurface>
        </div>
        <div className="mt-5 flex justify-end">
          <button type="button" onClick={onClose} className="btn-glass h-9 px-4 text-ui font-medium">
            完成
          </button>
        </div>
      </div>
    </Modal>
  );
}

const INPUT_CLASS =
  "w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60";

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <label className="mt-4 block first:mt-0">
      <span className="mb-1 flex items-baseline gap-2 text-sub font-medium text-[var(--text)]">
        {label}
        {hint && <span className="font-normal text-[var(--text-faint)]">{hint}</span>}
      </span>
      {children}
    </label>
  );
}

function SectionTitle({ title, description }: { title: string; description?: string }) {
  return (
    <div className="mb-3">
      <h3 className="text-ui font-semibold text-white/90">{title}</h3>
      {description && <p className="mt-0.5 text-caption text-[var(--text-faint)]">{description}</p>}
    </div>
  );
}

function PermissionToggle({
  label,
  description,
  checked,
  disabled = false,
  onChange,
}: {
  label: string;
  description: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className={`flex items-center justify-between gap-5 py-3 ${disabled ? "opacity-40" : "cursor-pointer"}`}>
      <span className="min-w-0">
        <span className="block text-ui font-medium text-[var(--text)]">{label}</span>
        <span className="mt-0.5 block text-caption text-[var(--text-faint)]">{description}</span>
      </span>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
        className="size-4 shrink-0 accent-[var(--accent)]"
      />
    </label>
  );
}

function Badge({ children }: { children: React.ReactNode }) {
  return <span className="rounded-md bg-white/[0.06] px-2 py-1 text-caption text-white/70">{children}</span>;
}

function StatusBadge({ enabled }: { enabled: boolean }) {
  return (
    <span className={`ml-auto rounded-full border px-2.5 py-1 text-caption font-medium ${enabled ? "border-emerald-400/25 bg-[var(--ok)]/10 text-emerald-300" : "border-white/[0.1] bg-white/[0.04] text-[var(--text-faint)]"}`}>
      {enabled ? "已启用" : "已停用"}
    </span>
  );
}

function CredentialRow({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <CopySurface
      text={value}
      successMessage={`${label}已复制`}
      className="flex w-full items-center gap-4 border-b border-white/[0.06] px-4 py-3 text-left transition-colors hover:bg-white/[0.045]"
    >
      <span className="w-10 shrink-0 text-caption text-[var(--text-faint)]">{label}</span>
      <span className={`min-w-0 flex-1 break-all text-ui text-white ${mono ? "font-mono" : ""}`}>
        {value}
      </span>
    </CopySurface>
  );
}

function CopySurface({
  text,
  successMessage,
  idleLabel = "点击复制",
  className,
  children,
}: {
  text: string;
  successMessage: string;
  idleLabel?: string;
  className: string;
  children: React.ReactNode;
}) {
  const toast = useToast();
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1800);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const copy = async () => {
    try {
      await copyText(text);
      setCopied(true);
      toast.success(successMessage);
    } catch {
      toast.error("复制失败，请长按内容手动复制");
    }
  };

  return (
    <button
      type="button"
      onClick={() => void copy()}
      aria-label={`${successMessage.replace("已复制", "")}，点击复制`}
      className={className}
    >
      {children}
      {(copied || idleLabel) && (
        <span
          aria-live="polite"
          className={`shrink-0 text-caption ${copied ? "text-emerald-300" : "text-[var(--text-faint)]"}`}
        >
          {copied ? "已复制" : idleLabel}
        </span>
      )}
    </button>
  );
}
