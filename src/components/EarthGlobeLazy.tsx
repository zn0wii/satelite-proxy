import { lazy, Suspense } from "react";

const EarthGlobeImpl = lazy(() =>
  import("./earth/EarthGlobe").then((m) => ({
    default: m.EarthGlobe,
  }))
);

/** Code-splits the WebGPU earth globe, mirroring OceanBackgroundLazy. */
export function EarthGlobeLazy({ interactive = true }: { interactive?: boolean }) {
  return (
    <Suspense fallback={null}>
      <EarthGlobeImpl interactive={interactive} />
    </Suspense>
  );
}
