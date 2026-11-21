export interface ConfirmOptions {
  title: string;
  /** 补充说明（后果、影响范围）；支持多行 */
  description?: string;
  /** 将要发生的动作清单（每行一条，带图标点缀）：重操作确认用它代替
   *  大段文字——一眼扫清"点下去会发生什么"。与 description 可并存
   *  （description 作导语）。 */
  bullets?: string[];
  confirmLabel?: string;
  cancelLabel?: string;
  /** danger 时确认键为红色（删除、撤销令牌等破坏性操作） */
  tone?: "default" | "danger";
  /** 可选的勾选项（如「已有的也重新生成」）：一个入口带一个开关，
   *  避免菜单里出现「生成 / 重新生成」这种只差一个词的两条。带了
   *  checkbox 的 confirm 解析为 ConfirmResult，否则仍是 boolean。 */
  checkbox?: ConfirmCheckbox;
}

export interface ConfirmCheckbox {
  label: string;
  /** 勾上之后的额外后果说明，灰字放在 label 下方 */
  description?: string;
  defaultChecked?: boolean;
}

/** 带 checkbox 的确认结果：ok=false 时 checked 无意义 */
export interface ConfirmResult {
  ok: boolean;
  checked: boolean;
}

export interface PromptOptions {
  title: string;
  description?: string;
  initialValue?: string;
  placeholder?: string;
  confirmLabel?: string;
  maxLength?: number;
  /** number 时渲染数字输入（数字键盘、隐藏原生步进钮），仍以字符串返回由调用方解析 */
  kind?: "text" | "number";
  /** 数字输入的下限（透传给 input min） */
  min?: number;
  /** 单位后缀（如 "GiB"）：常驻在输入框内部右侧，与输入值视觉上同框不粘连 */
  unit?: string;
}
