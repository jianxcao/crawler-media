import { BrandLoader } from "@/components/brand-loader";

export function RouteFallback() {
  return (
    <div className="relative z-10 flex h-full w-full items-center justify-center">
      <BrandLoader className="size-6" />
    </div>
  );
}
