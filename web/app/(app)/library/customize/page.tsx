import { LibraryCustomizeView } from "@/components/library-customize-view";

/** 自定义媒体库首页（/library/customize）：行的顺序、显隐、排序与名字；桌面与移动端同一个页面。 */
export default function LibraryCustomizePage() {
  return (
    <div className="flex h-full flex-col">
      <LibraryCustomizeView />
    </div>
  );
}
