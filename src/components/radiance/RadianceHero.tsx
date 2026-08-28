import { useEffect, useRef } from "react";
import { createRenderer } from "./renderer";

/**
 * Agent Radiance Cascades hero (ported from vgpu's example): a field of
 * glowing dots lit through a jump-flooded distance field and radiance
 * cascades. Mounted via RadianceHeroLazy so the vgpu chunk only loads when
 * this hero style is selected.
 */
export function RadianceHero() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const renderer = createRenderer({ canvas });
    // Purely decorative: WebGPU missing or any init/render failure must
    // degrade to the static fallback disc, never surface an error modal.
    renderer.ready.catch(() => {});
    return () => renderer.dispose();
  }, []);

  return <canvas ref={canvasRef} className="radiance-hero-canvas" aria-hidden />;
}
