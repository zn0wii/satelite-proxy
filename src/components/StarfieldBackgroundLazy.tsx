import { lazy, Suspense } from "react";
import { useTheme } from "../theme";

const StarfieldBackgroundImpl = lazy(() =>
  import("./earth/StarfieldBackground").then((m) => ({
    default: m.StarfieldBackground,
  }))
);

/**
 * Aerospace theme only, same gate as OceanBackgroundLazy: the starfield is a
 * night sky — over the light "day" theme it would fight the UI, so we skip
 * mounting (and loading the vgpu chunk) entirely there.
 */
export function StarfieldBackgroundLazy() {
  const { theme } = useTheme();
  if (theme !== "aerospace") return null;
  return (
    <Suspense fallback={null}>
      <StarfieldBackgroundImpl />
    </Suspense>
  );
}
