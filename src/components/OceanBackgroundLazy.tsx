import { lazy, Suspense } from "react";
import { useTheme } from "../theme";

const OceanBackgroundImpl = lazy(() =>
  import("./ocean/OceanBackground").then((m) => ({
    default: m.OceanBackground,
  }))
);

/**
 * Aerospace theme only: the ocean is a dark neon particle field — over the
 * light "day" theme it reads as mud, so we skip mounting (and loading the
 * vgpu chunk) entirely there.
 */
export function OceanBackgroundLazy() {
  const { theme } = useTheme();
  if (theme !== "aerospace") return null;
  return (
    <Suspense fallback={null}>
      <OceanBackgroundImpl />
    </Suspense>
  );
}
