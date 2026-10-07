"use client";

import { type ReactNode, isValidElement, memo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import { CopyButton, REVEAL_CLASS, useTapReveal } from "@/components/copy-button";

/**
 * AI 回复正文的 Markdown 渲染器。
 *
 * 选型：react-markdown —— React 生态最成熟的方案，底层是 remark/rehype
 * 插件体系（unified 生态），后续要加代码高亮（rehype-highlight/shiki）、
 * 数学公式（remark-math + rehype-katex）等只需往 plugins 数组里追加，
 * 不用换渲染器。且它把 markdown 编译为 React 元素而非 dangerouslySetInnerHTML，
 * 默认无 XSS 风险，适合渲染 LLM 输出这种不可信文本。
 *
 * 样式统一放在 globals.css 的 .markdown 作用域下（跟随 immersive-theme
 * 的灰阶变量），组件层只保留必须用 JS 表达的映射（外链新窗口打开）。
 *
 * memo：流式生成时父组件每个 chunk 都会重渲染，已完成的历史轮次
 * text 不变，跳过重新解析。
 */
const components: Components = {
  // LLM 输出里的链接一律新窗口打开，不打断当前会话
  a: ({ children, href }) => (
    <a href={href} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  ),
  pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
};

/** 递归取出一棵 React 子树里的纯文本——代码块要复制的是原始代码，
 *  而 react-markdown 交到手里的已经是 <code> 元素树。 */
function plainText(node: ReactNode): string {
  if (typeof node === "string") return node;
  if (typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(plainText).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return plainText(node.props.children);
  return "";
}

/**
 * 代码块：右上角浮出复制按钮。
 *
 * 代码块里多半是命令或配置，手指划选是移动端最难用的操作之一，一个复制键
 * 省掉整段折腾。按钮定位在包裹层而不是 <pre> 内部——<pre> 自己是横向滚动
 * 容器（见 globals.css 的 .markdown pre），放里面会跟着代码一起滚走。
 */
function CodeBlock({ children }: { children?: ReactNode }) {
  const { revealProps, toggle } = useTapReveal();
  return (
    <div className="group/copy relative" onClick={toggle} {...revealProps}>
      <pre>{children}</pre>
      <CopyButton
        text={plainText(children)}
        className={`${REVEAL_CLASS} absolute right-1.5 top-1.5 border border-white/[0.08] bg-[rgba(28,28,32,0.9)] p-1.5 text-[var(--text-faint)] backdrop-blur-sm hover:text-[var(--text)]`}
      />
    </div>
  );
}

export const Markdown = memo(function Markdown({
  text,
  compact = false,
}: {
  text: string;
  /** 紧凑档：嵌在卡片里的短文（如更新说明）用，字号/行高压到与周边说明文字同级。
   *  必须靠 class 而非父级字号继承——.markdown 自己写死了 font-size。 */
  compact?: boolean;
}) {
  return (
    <div className={`markdown${compact ? " markdown--compact" : ""}`}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {text}
      </ReactMarkdown>
    </div>
  );
});
