import { useEffect, useRef } from "react";
import { createStarfieldRenderer } from "./renderer";

/**
 * Starfield backdrop (ported from vgpu's earth example): hashed star layers,
 * galactic dust band, and a slowly drifting HDR sun with bloom. Aerospace
 * theme only — mounted via StarfieldBackgroundLazy.
 */
export function StarfieldBackground() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const renderer = createStarfieldRenderer({ canvas });
    // Purely decorative: WebGPU missing or any init/render failure must
    // degrade to the plain theme gradient, never surface an error modal.
    renderer.ready.catch(() => {});
    return () => renderer.dispose();
  }, []);

  return <canvas ref={canvasRef} className="starfield-bg" aria-hidden />;
}
