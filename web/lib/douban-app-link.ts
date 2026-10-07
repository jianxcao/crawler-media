"use client";

import { useEffect, useState } from "react";

/**
 * 豆瓣词条外链的移动端 App 直跳。
 *
 * 桌面维持网页词条地址；无悬停设备（手机/平板）把「豆瓣」外链换成豆瓣
 * 官方的 App 分发地址（豆瓣移动版「APP 中打开」按钮同款）：
 *
 *   https://www.douban.com/doubanapp/dispatch?uri=/movie/{id}/&dt_dapp=1
 *
 * 已安装豆瓣 App 时：iOS 经 Universal Link、Android 经 App Link（或分发
 * 页内的 douban:// scheme）直接拉起 App 进入对应词条页；未安装则落到
 * 网页版词条 + 下载引导，不会白屏。直接写 douban:// scheme 做 href 不可
 * 行——未装 App 时 iOS 会弹「无法打开该网页」，Android 则毫无反应。
 * 电影与剧集共用 /movie/ 命名空间：豆瓣的影视 subject id 本就是同一套。
 *
 * 挂载后（useEffect）才计算：SSR 拿不到设备能力，首帧一律返回 null，
 * 避免服务端与客户端渲染出不同 href 的 hydration 冲突。
 */
export function useDoubanAppHref(doubanId: string | null | undefined): string | null {
  const [href, setHref] = useState<string | null>(null);
  useEffect(() => {
    if (!doubanId || !/^\d+$/.test(doubanId) || !window.matchMedia("(hover: none)").matches) {
      setHref(null);
      return;
    }
    setHref(`https://www.douban.com/doubanapp/dispatch?uri=/movie/${doubanId}/&dt_dapp=1`);
  }, [doubanId]);
  return href;
}
