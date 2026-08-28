import { useEffect, useRef } from "react";
import { createGlobeRenderer } from "./renderer";

/**
 * Procedural Earth globe (ported from vgpu's earth example): baked
 * albedo/cloud maps, day/night terminator with city lights, atmosphere rim.
 * Drag rotates the camera orbit with inertia — unless `interactive` is off,
 * e.g. inside the simple-mode start button where drag must not steal clicks.
 */
export function EarthGlobe({ interactive = true }: { interactive?: boolean }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const renderer = createGlobeRenderer({ canvas, interactive });
    // Purely decorative: WebGPU missing or any init/render failure must
    // degrade to an empty corner, never surface an error modal.
    renderer.ready.catch(() => {});
    return () => renderer.dispose();
  }, [interactive]);

  return <canvas ref={canvasRef} className="dash-globe" aria-hidden />;
}
