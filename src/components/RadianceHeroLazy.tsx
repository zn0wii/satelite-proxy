import { lazy, Suspense } from "react";

const RadianceHeroImpl = lazy(() =>
  import("./radiance/RadianceHero").then((m) => ({
    default: m.RadianceHero,
  }))
);

/** Code-splits the WebGPU radiance-cascades hero, mirroring ParticleSphereLazy. */
export function RadianceHeroLazy() {
  return (
    <Suspense fallback={<div className="radiance-hero-fallback" aria-hidden />}>
      <RadianceHeroImpl />
    </Suspense>
  );
}
