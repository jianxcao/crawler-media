/**
 * 根布局(SAP 版):<html>/<head> 由 index.html 静态提供(FOUC 脚本、meta、
 * manifest、字体 @font-face 都在那里,解析即生效)。这里只挂全站都需要的
 * 组件与样式:液态玻璃样式、全局主题、键盘适配。
 */
import { Outlet } from "react-router-dom";
import { ViewportKeyboard } from "@/components/viewport-keyboard";

// 先引入液态玻璃组件自带的样式,再引入本项目的全局深色主题(后者可覆盖前者)。
import "@/vendor/liquid-glass/styles.css";
import "./globals.css";

export default function RootLayout() {
  return (
    <>
      {/* 挂在根布局:登录页等 AppShell 之外的页面也有输入框,同样需要键盘适配 */}
      <ViewportKeyboard />
      <Outlet />
    </>
  );
}
